//! Public Pinterest Pin downloads for image and video Pins.
//!
//! The importer accepts only individual Pinterest Pin URLs (including `pin.it`
//! short links), runs yt-dlp without user config or browser cookies, and keeps
//! each download in a private staging directory until it passes media checks.

use anyhow::{Context, Result, anyhow, bail};
use crossbeam_channel::{Receiver, Sender, unbounded};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(150);
const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinterestMediaKind {
    Image,
    Video,
}

#[derive(Debug, Clone)]
pub struct PinterestMedia {
    /// Canonical Pin URL, suitable for storing in `Element::source_url`.
    pub original_url: String,
    pub media_kind: PinterestMediaKind,
    pub local_path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub duration: Option<f64>,
}

#[derive(Debug, Clone)]
pub enum PinterestEvent {
    Progress(String),
    Finished(PinterestMedia),
    Failed(String),
    Cancelled,
}

pub struct PinterestJob {
    pub events: Receiver<PinterestEvent>,
    cancel: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl PinterestJob {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Ok(mut slot) = self.child.lock()
            && let Some(mut child) = slot.take()
            && child.try_wait().ok().flatten().is_none()
        {
            kill_process_tree(child.id());
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for PinterestJob {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[derive(Debug, Deserialize)]
struct YtDlpMetadata {
    #[serde(default)]
    id: String,
    extractor_key: Option<String>,
    webpage_url: Option<String>,
    duration: Option<f64>,
    #[serde(default)]
    formats: Vec<VideoFormat>,
    #[serde(default)]
    thumbnails: Vec<Thumbnail>,
}

#[derive(Debug, Deserialize)]
struct VideoFormat {
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Thumbnail {
    url: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
}

/// Starts a cancellable background download for one public Pinterest Pin.
pub fn begin_download(url: String) -> Result<PinterestJob> {
    validate_pin_url(&url)?;
    let (tx, events) = unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let child = Arc::new(Mutex::new(None));
    let worker_cancel = Arc::clone(&cancel);
    let worker_child = Arc::clone(&child);
    thread::Builder::new()
        .name("pinterest-download".into())
        .spawn(move || {
            let result = download_pin(&url, &tx, &worker_cancel, &worker_child);
            match result {
                Ok(media) => {
                    if worker_cancel.load(Ordering::SeqCst) {
                        let _ = tx.send(PinterestEvent::Cancelled);
                    } else {
                        let _ = tx.send(PinterestEvent::Finished(media));
                    }
                }
                Err(_error) if worker_cancel.load(Ordering::SeqCst) => {
                    let _ = tx.send(PinterestEvent::Cancelled);
                }
                Err(error) => {
                    let _ = tx.send(PinterestEvent::Failed(error.to_string()));
                }
            }
        })?;
    Ok(PinterestJob {
        events,
        cancel,
        child,
    })
}

fn download_pin(
    input_url: &str,
    tx: &Sender<PinterestEvent>,
    cancel: &AtomicBool,
    child_slot: &Arc<Mutex<Option<Child>>>,
) -> Result<PinterestMedia> {
    let _ = tx.send(PinterestEvent::Progress(
        "Reading public Pin metadata".into(),
    ));
    check_cancel(cancel)?;
    let metadata_output = run_ytdlp_json(input_url, child_slot, cancel)?;
    let metadata: YtDlpMetadata = serde_json::from_slice(&metadata_output)
        .context("Pinterest returned unreadable Pin metadata")?;
    if !metadata
        .extractor_key
        .as_deref()
        .is_some_and(|extractor| extractor.eq_ignore_ascii_case("Pinterest"))
    {
        bail!("This link did not resolve to an individual public Pinterest Pin");
    }
    let pin_id = metadata.id.trim();
    if pin_id.is_empty() || !pin_id.bytes().all(|b| b.is_ascii_digit()) {
        bail!("Pinterest did not return a valid Pin id");
    }
    let page_url = metadata
        .webpage_url
        .as_deref()
        .ok_or_else(|| anyhow!("Pinterest did not return the Pin's canonical page URL"))?;
    validate_pin_url(page_url)?;
    if pin_id_from_url(page_url) != Some(pin_id) {
        bail!("Pinterest Pin metadata did not match the linked Pin");
    }
    let canonical_url = format!("https://www.pinterest.com/pin/{pin_id}/");
    let cache = pinterest_cache_dir();
    fs::create_dir_all(&cache).context("Could not create Pinterest cache")?;
    let staging = tempfile::Builder::new()
        .prefix("pinterest-")
        .tempdir_in(&cache)
        .context("Could not create a private Pinterest staging folder")?;

    // Formats or duration identify a video. Never turn a known video Pin's
    // poster thumbnail into a still image if its video stream is unavailable.
    let has_video_evidence = metadata.duration.is_some_and(|d| d.is_finite() && d > 0.0)
        || metadata
            .formats
            .iter()
            .any(|format| format.url.as_deref().is_some_and(is_https_url));
    if has_video_evidence {
        let _ = tx.send(PinterestEvent::Progress("Downloading Pin video".into()));
        let (path, width, height, duration) = download_video(
            &canonical_url,
            &metadata,
            staging.path(),
            child_slot,
            cancel,
        )?;
        check_cancel(cancel)?;
        let cached = publish_file(&path, &cache, &metadata.id, "mp4")?;
        Ok(PinterestMedia {
            original_url: canonical_url,
            media_kind: PinterestMediaKind::Video,
            local_path: cached,
            width,
            height,
            duration: Some(duration),
        })
    } else {
        if !metadata.formats.is_empty() {
            bail!("Pinterest returned media formats that could not be classified safely");
        }
        let _ = tx.send(PinterestEvent::Progress("Downloading Pin image".into()));
        let (path, width, height, ext) = download_image(&metadata, staging.path(), cancel)?;
        check_cancel(cancel)?;
        let cached = publish_file(&path, &cache, &metadata.id, ext)?;
        Ok(PinterestMedia {
            original_url: canonical_url,
            media_kind: PinterestMediaKind::Image,
            local_path: cached,
            width,
            height,
            duration: None,
        })
    }
}

fn run_ytdlp_json(
    url: &str,
    child_slot: &Arc<Mutex<Option<Child>>>,
    cancel: &AtomicBool,
) -> Result<Vec<u8>> {
    let mut command = base_ytdlp_command();
    command
        .args(["--ignore-no-formats-error", "--dump-single-json"])
        .arg(url)
        .stderr(Stdio::null());
    let (status, stdout) = run_capture(
        command,
        child_slot,
        cancel,
        COMMAND_TIMEOUT,
        8 * 1024 * 1024,
    )
    .context("Could not start yt-dlp for Pinterest")?;
    if !status.success() && serde_json::from_slice::<YtDlpMetadata>(&stdout).is_err() {
        bail!("Could not read this public Pinterest Pin. Check the link and try again.");
    }
    if stdout.is_empty() {
        bail!("Pinterest returned no Pin metadata");
    }
    Ok(stdout)
}

fn download_video(
    url: &str,
    metadata: &YtDlpMetadata,
    staging: &Path,
    child_slot: &Arc<Mutex<Option<Child>>>,
    cancel: &AtomicBool,
) -> Result<(PathBuf, u32, u32, f64)> {
    let template = staging.join("video.%(ext)s");
    let mut command = base_ytdlp_command();
    command
        .args(["-f", "best[ext=mp4]/best", "-o"])
        .arg(template)
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_console(&mut command);
    let process = command
        .spawn()
        .context("Could not start Pinterest video download")?;
    *child_slot
        .lock()
        .map_err(|_| anyhow!("Pinterest job is unavailable"))? = Some(process);
    let status = wait_child(child_slot, cancel, COMMAND_TIMEOUT)?;
    if !status.success() {
        bail!("Could not download this Pinterest video. The Pin may be unavailable or restricted.");
    }
    let video = fs::read_dir(staging)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"))
        })
        .ok_or_else(|| anyhow!("Pinterest did not produce a video file"))?;
    let (width, height, duration) = probe_video(&video, child_slot, cancel)?;
    if let Some(advertised) = metadata.duration.filter(|d| d.is_finite() && *d > 0.0)
        && (advertised - duration).abs() > 3.0
    {
        bail!("Downloaded Pinterest video duration did not match Pin metadata");
    }
    Ok((video, width, height, duration))
}

fn download_image(
    metadata: &YtDlpMetadata,
    staging: &Path,
    cancel: &AtomicBool,
) -> Result<(PathBuf, u32, u32, &'static str)> {
    let thumbnail = metadata
        .thumbnails
        .iter()
        .filter_map(|thumbnail| {
            let url = thumbnail.url.as_deref()?;
            if !is_pinimg_url(url) { return None; }
            Some((thumbnail.width.unwrap_or(0) as u64 * thumbnail.height.unwrap_or(0) as u64, url))
        })
        .max_by_key(|(pixels, _)| *pixels)
        .map(|(_, url)| url)
        .ok_or_else(|| anyhow!("This Pin has no downloadable image; Pinterest did not expose a video stream either."))?;

    check_cancel(cancel)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() < 5 && is_pinimg_url(attempt.url().as_str()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()?;
    let mut response = client
        .get(thumbnail)
        .send()
        .context("Could not retrieve the Pinterest image")?;
    if !response.status().is_success() {
        bail!("Pinterest image server returned HTTP {}", response.status());
    }
    if let Some(length) = response.content_length()
        && length > MAX_IMAGE_BYTES
    {
        bail!("Pinterest image is larger than the 64 MB import limit");
    }
    let path = staging.join("image.download");
    let mut file = File::create(&path)?;
    let mut total = 0u64;
    let mut buffer = [0u8; 32 * 1024];
    loop {
        check_cancel(cancel)?;
        let count = response.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > MAX_IMAGE_BYTES {
            bail!("Pinterest image is larger than the 64 MB import limit");
        }
        file.write_all(&buffer[..count])?;
    }
    file.flush()?;
    let decoded = image::ImageReader::open(&path)?
        .with_guessed_format()?
        .decode()
        .context("Pinterest image data could not be decoded")?;
    let (width, height) = (decoded.width(), decoded.height());
    let ext = match image::ImageReader::open(&path)?
        .with_guessed_format()?
        .format()
    {
        Some(image::ImageFormat::Png) => "png",
        Some(image::ImageFormat::Jpeg) => "jpg",
        Some(image::ImageFormat::Gif) => "gif",
        Some(image::ImageFormat::WebP) => "webp",
        Some(image::ImageFormat::Bmp) => "bmp",
        _ => bail!("Pinterest returned an unsupported image format"),
    };
    Ok((path, width, height, ext))
}

fn probe_video(
    path: &Path,
    child_slot: &Arc<Mutex<Option<Child>>>,
    cancel: &AtomicBool,
) -> Result<(u32, u32, f64)> {
    let mut command = Command::new(crate::youtube::tool_path("ffprobe"));
    command
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-show_entries",
            "format=duration",
            "-of",
            "json",
        ])
        .arg(path)
        .stderr(Stdio::null());
    let (status, stdout) = run_capture(
        command,
        child_slot,
        cancel,
        Duration::from_secs(15),
        1024 * 1024,
    )
    .context("Could not verify Pinterest video with ffprobe")?;
    if !status.success() {
        bail!("Pinterest download is not a readable video");
    }
    let value: serde_json::Value = serde_json::from_slice(&stdout)?;
    let stream = value
        .get("streams")
        .and_then(|v| v.get(0))
        .ok_or_else(|| anyhow!("Pinterest video has no video stream"))?;
    let width = stream
        .get("width")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32;
    let height = stream
        .get("height")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32;
    let duration = value
        .get("format")
        .and_then(|v| v.get("duration"))
        .and_then(|v| v.as_str()?.parse::<f64>().ok())
        .unwrap_or_default();
    if width == 0 || height == 0 || !duration.is_finite() || duration <= 0.0 {
        bail!("Pinterest video metadata is incomplete");
    }
    Ok((width, height, duration))
}

fn wait_child(
    child_slot: &Arc<Mutex<Option<Child>>>,
    cancel: &AtomicBool,
    timeout: Duration,
) -> Result<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        let mut slot = child_slot
            .lock()
            .map_err(|_| anyhow!("Pinterest job is unavailable"))?;
        if cancel.load(Ordering::SeqCst) {
            if let Some(mut child) = slot.take() {
                kill_process_tree(child.id());
                let _ = child.kill();
                let _ = child.wait();
            }
            bail!("Pinterest download cancelled");
        }
        let child = slot
            .as_mut()
            .ok_or_else(|| anyhow!("Pinterest download process disappeared"))?;
        if let Some(status) = child.try_wait()? {
            *slot = None;
            return Ok(status);
        }
        if Instant::now() >= deadline {
            kill_process_tree(child.id());
            let _ = child.kill();
            let _ = child.wait();
            *slot = None;
            bail!(
                "Pinterest request timed out after {} seconds",
                timeout.as_secs()
            );
        }
        drop(slot);
        thread::sleep(Duration::from_millis(100));
    }
}

fn run_capture(
    mut command: Command,
    child_slot: &Arc<Mutex<Option<Child>>>,
    cancel: &AtomicBool,
    timeout: Duration,
    max_bytes: u64,
) -> Result<(std::process::ExitStatus, Vec<u8>)> {
    command.stdout(Stdio::piped());
    hide_console(&mut command);
    let mut child = command
        .spawn()
        .context("Could not start media verification process")?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("Media process output unavailable"))?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut stdout = stdout;
        let mut buffer = [0u8; 16 * 1024];
        while let Ok(count) = stdout.read(&mut buffer) {
            if count == 0 {
                break;
            }
            let remaining = max_bytes.saturating_sub(bytes.len() as u64) as usize;
            let keep = count.min(remaining);
            bytes.extend_from_slice(&buffer[..keep]);
        }
        bytes
    });
    *child_slot
        .lock()
        .map_err(|_| anyhow!("Pinterest job is unavailable"))? = Some(child);
    let status = wait_child(child_slot, cancel, timeout);
    let stdout = reader
        .join()
        .map_err(|_| anyhow!("Media process output reader stopped unexpectedly"))?;
    Ok((status?, stdout))
}

fn base_ytdlp_command() -> Command {
    let mut command = Command::new(crate::youtube::tool_path("yt-dlp"));
    let deno = crate::youtube::tool_path("deno");
    command.args([
        "--ignore-config",
        "--no-playlist",
        "--no-warnings",
        "--socket-timeout",
        "20",
        "--retries",
        "1",
        "--extractor-retries",
        "1",
    ]);
    if deno.exists() {
        command.args(["--js-runtimes", &format!("deno:{}", deno.display())]);
    }
    let ffmpeg = crate::youtube::tool_path("ffmpeg");
    if ffmpeg.exists()
        && let Some(directory) = ffmpeg.parent()
    {
        command.arg("--ffmpeg-location").arg(directory);
    }
    command
}

fn pinterest_cache_dir() -> PathBuf {
    directories::ProjectDirs::from("", "AnimRef", "AnimRef")
        .map(|paths| paths.cache_dir().join("pinterest"))
        .unwrap_or_else(|| std::env::temp_dir().join("AnimRef-cache").join("pinterest"))
}

fn publish_file(staged: &Path, cache: &Path, id: &str, ext: &str) -> Result<PathBuf> {
    let content_hash = file_hash(staged)?;
    let dest = cache.join(format!("{id}-{content_hash}.{ext}"));
    if dest.is_file() {
        if file_hash(&dest)? == content_hash {
            return Ok(dest);
        }
        match fs::remove_file(&dest) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).context("Could not replace a damaged Pinterest cache entry");
            }
        }
    }
    match fs::rename(staged, &dest) {
        Ok(()) => Ok(dest),
        Err(_error)
            if dest.is_file()
                && file_hash(&dest).ok().as_deref() == Some(content_hash.as_str()) =>
        {
            Ok(dest)
        }
        Err(error) => Err(error).context("Could not atomically publish Pinterest download"),
    }
}

fn file_hash(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(hex(&digest.finalize()))
}

fn validate_pin_url(url: &str) -> Result<()> {
    if url.len() > 2048
        || url
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
        || url.contains('\\')
    {
        bail!("Paste a valid Pinterest Pin link");
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        bail!("Paste a valid Pinterest Pin link");
    };
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
        bail!("Pinterest link must use HTTP or HTTPS");
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        bail!("Pinterest link contains an invalid host");
    }
    let host = authority
        .split(':')
        .next()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let path = rest
        .get(authority.len()..)
        .unwrap_or_default()
        .split(['?', '#'])
        .next()
        .unwrap_or_default();
    if host == "pin.it" {
        let short_code = path.trim_matches('/');
        if short_code.is_empty()
            || short_code.contains('/')
            || !short_code
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            bail!("Paste a valid Pinterest short link");
        }
        return Ok(());
    }
    const PINTEREST_TLDS: &[&str] = &[
        "com", "fr", "de", "ch", "jp", "cl", "ca", "it", "co.uk", "nz", "ru", "com.au", "at", "pt",
        "co.kr", "es", "com.mx", "dk", "ph", "th", "com.uy", "co", "nl", "info", "kr", "ie", "vn",
        "com.vn", "ec", "mx", "in", "pe", "co.at", "hu", "co.in", "co.nz", "id", "com.ec",
        "com.py", "tw", "be", "uk", "com.bo", "com.pe",
    ];
    let is_pinterest = host == "pinterest.com"
        || host.ends_with(".pinterest.com")
        || PINTEREST_TLDS
            .iter()
            .any(|tld| host == format!("pinterest.{tld}"));
    if !is_pinterest || !path.split('/').any(|segment| segment == "pin") {
        bail!("Paste a link to an individual Pinterest Pin");
    }
    let id = path
        .split('/')
        .skip_while(|segment| *segment != "pin")
        .nth(1)
        .unwrap_or_default();
    let id = id.rsplit_once("--").map_or(id, |(_, suffix)| suffix);
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        bail!("Pinterest Pin link does not contain a valid Pin id");
    }
    Ok(())
}

fn pin_id_from_url(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let path = rest.get(authority.len()..)?.split(['?', '#']).next()?;
    let pin_segment = path.split('/').find(|segment| *segment == "pin")?;
    let id = path
        .split('/')
        .skip_while(|segment| *segment != pin_segment)
        .nth(1)?;
    let id = id.rsplit_once("--").map_or(id, |(_, suffix)| suffix);
    id.bytes().all(|byte| byte.is_ascii_digit()).then_some(id)
}

fn is_pinimg_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    host == "pinimg.com" || host.ends_with(".pinimg.com")
}

fn is_https_url(url: &str) -> bool {
    url.starts_with("https://")
}
fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::SeqCst) {
        bail!("Pinterest download cancelled");
    }
    Ok(())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn kill_process_tree(pid: u32) {
    #[cfg(windows)]
    {
        let mut command = Command::new("taskkill.exe");
        command
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hide_console(&mut command);
        let _ = command.status();
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
    }
}

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000000);
}
#[cfg(not(windows))]
fn hide_console(_: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn wait_for_media(job: &PinterestJob) -> PinterestMedia {
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "Pinterest API download timed out");
            match job
                .events
                .recv_timeout(remaining.min(Duration::from_secs(1)))
            {
                Ok(PinterestEvent::Finished(media)) => return media,
                Ok(PinterestEvent::Failed(error)) => {
                    panic!("Pinterest API download failed: {error}")
                }
                Ok(PinterestEvent::Cancelled) => panic!("Pinterest API download was cancelled"),
                Ok(PinterestEvent::Progress(_))
                | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("Pinterest job ended without a result: {error}"),
            }
        }
    }

    #[test]
    fn accepts_only_single_pin_urls_and_pin_it_short_links() {
        assert!(validate_pin_url("https://www.pinterest.com/pin/123456789/").is_ok());
        assert!(validate_pin_url("https://pinterest.ca/pin/a-title--123456789/").is_ok());
        assert!(validate_pin_url("https://pin.it/AbCdEf").is_ok());
        assert!(validate_pin_url("https://example.com/pin/123456789").is_err());
        assert!(validate_pin_url("https://pinterest.com/user/board/").is_err());
        assert!(validate_pin_url("https://pinterest.com.attacker.example/pin/123456789").is_err());
        assert!(validate_pin_url("javascript:https://pinterest.com/pin/123/").is_err());
        assert_eq!(
            pin_id_from_url("https://www.pinterest.com/pin/slug--123456789/"),
            Some("123456789")
        );
        assert_eq!(
            pin_id_from_url("https://example.com/pin/123456789/"),
            Some("123456789")
        );
    }

    #[test]
    fn image_urls_are_limited_to_https_pinimg_hosts() {
        assert!(is_pinimg_url("https://i.pinimg.com/originals/a.jpg"));
        assert!(!is_pinimg_url("http://i.pinimg.com/a.jpg"));
        assert!(!is_pinimg_url("https://pinimg.com.attacker.example/a.jpg"));
    }

    #[test]
    fn publish_file_reuses_matching_cache_and_repairs_damaged_cache() {
        let directory = tempfile::tempdir().unwrap();
        let cache = directory.path().join("cache");
        fs::create_dir_all(&cache).unwrap();

        let matching_source = directory.path().join("matching-source.mp4");
        fs::write(&matching_source, b"valid media bytes").unwrap();
        let expected_hash = file_hash(&matching_source).unwrap();
        let expected_path = cache.join(format!("123-{expected_hash}.mp4"));
        fs::write(&expected_path, b"damaged cache bytes").unwrap();
        let repaired = publish_file(&matching_source, &cache, "123", "mp4").unwrap();
        assert_eq!(repaired, expected_path);
        assert_eq!(fs::read(&repaired).unwrap(), b"valid media bytes");

        let duplicate_source = directory.path().join("duplicate-source.mp4");
        fs::write(&duplicate_source, b"valid media bytes").unwrap();
        let reused = publish_file(&duplicate_source, &cache, "123", "mp4").unwrap();
        assert_eq!(reused, expected_path);
        assert_eq!(fs::read(&reused).unwrap(), b"valid media bytes");
        assert!(
            duplicate_source.is_file(),
            "matching cache should be reused"
        );
    }

    #[test]
    #[ignore = "network Pinterest acceptance; preserves files in the Pinterest cache"]
    fn downloads_public_image_and_video_using_job_api() {
        let image_job =
            begin_download("https://www.pinterest.com/pin/2392606048574191/".into()).unwrap();
        let image = wait_for_media(&image_job);
        assert_eq!(image.media_kind, PinterestMediaKind::Image);
        assert_eq!(
            image.original_url,
            "https://www.pinterest.com/pin/2392606048574191/"
        );
        assert_eq!((image.width, image.height), (750, 1166));
        assert!(image.local_path.is_file());
        image::ImageReader::open(&image.local_path)
            .unwrap()
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap();
        println!(
            "Pinterest API image: {}x{}, {} bytes at {}",
            image.width,
            image.height,
            fs::metadata(&image.local_path).unwrap().len(),
            image.local_path.display()
        );

        let video_job =
            begin_download("https://www.pinterest.com/pin/664281013778109217/".into()).unwrap();
        let video = wait_for_media(&video_job);
        assert_eq!(video.media_kind, PinterestMediaKind::Video);
        assert_eq!(
            video.original_url,
            "https://www.pinterest.com/pin/664281013778109217/"
        );
        assert_eq!((video.width, video.height), (1080, 1920));
        assert!((video.duration.unwrap_or_default() - 57.7).abs() < 1.0);
        assert!(video.local_path.is_file());
        println!(
            "Pinterest API video: {}x{}, {:.3}s, {} bytes at {}",
            video.width,
            video.height,
            video.duration.unwrap(),
            fs::metadata(&video.local_path).unwrap().len(),
            video.local_path.display()
        );
    }
}
