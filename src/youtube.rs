use anyhow::{Context, Result, anyhow};
use crossbeam_channel::{Receiver, unbounded};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Deserialize)]
pub struct VideoMetadata {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub duration: Option<f64>,
}

#[derive(Debug, Clone)]
pub enum DownloadEvent {
    Progress(f32),
    Finished(PathBuf),
    Failed(String),
    Cancelled,
}

pub struct DownloadJob {
    pub events: Receiver<DownloadEvent>,
    cancel: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

pub struct MetadataJob {
    pub events: Receiver<Result<VideoMetadata, String>>,
    cancel: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl MetadataJob {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Ok(mut child) = self.child.lock()
            && let Some(process) = child.as_mut()
            && process.try_wait().ok().flatten().is_none()
        {
            kill_process_tree(process.id());
            let _ = process.kill();
        }
    }
}

impl Drop for MetadataJob {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl DownloadJob {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Ok(mut child) = self.child.lock()
            && let Some(child) = child.as_mut()
        {
            kill_process_tree(child.id());
            let _ = child.kill();
        }
    }
}

impl Drop for DownloadJob {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub fn tool_path(name: &str) -> PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    if let Some(dir) = exe_dir {
        for candidate in [
            dir.join("tools").join(format!("{name}.exe")),
            dir.join(format!("{name}.exe")),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    let data_tools = directories::ProjectDirs::from("", "AnimRef", "AnimRef")
        .map(|p| p.data_dir().join("tools").join(format!("{name}.exe")));
    if let Some(candidate) = data_tools
        && candidate.is_file()
    {
        return candidate;
    }
    PathBuf::from(format!("{name}.exe"))
}

pub fn cache_dir() -> PathBuf {
    directories::ProjectDirs::from("", "AnimRef", "AnimRef")
        .map(|p| p.cache_dir().join("youtube"))
        .unwrap_or_else(|| std::env::temp_dir().join("AnimRef-cache").join("youtube"))
}

fn base_command() -> Command {
    let mut command = Command::new(tool_path("yt-dlp"));
    let deno = tool_path("deno");
    let ffmpeg = tool_path("ffmpeg");
    command
        .arg("--ignore-config")
        .arg("--no-playlist")
        .arg("--no-warnings")
        .arg("--socket-timeout")
        .arg("20")
        .arg("--retries")
        .arg("1")
        .arg("--extractor-retries")
        .arg("1");
    if deno.exists() {
        command
            .arg("--js-runtimes")
            .arg(format!("deno:{}", deno.display()));
    }
    if ffmpeg.exists() {
        command
            .arg("--ffmpeg-location")
            .arg(ffmpeg.parent().unwrap_or(Path::new(".")));
    }
    hide_console(&mut command);
    command
}

pub fn begin_metadata(url: String) -> Result<MetadataJob> {
    let mut command = base_command();
    command
        .arg("--dump-single-json")
        .arg("--skip-download")
        .arg(url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console(&mut command);
    let child = command.spawn().with_context(|| {
        format!(
            "Could not start yt-dlp at {}",
            tool_path("yt-dlp").display()
        )
    })?;
    let child_id = child.id();
    let child = Arc::new(Mutex::new(Some(child)));
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_child = Arc::clone(&child);
    let worker_cancel = Arc::clone(&cancel);
    let (tx, events) = crossbeam_channel::bounded(1);
    thread::spawn(move || {
        let (stdout, stderr) = {
            let mut slot = match worker_child.lock() {
                Ok(slot) => slot,
                Err(_) => return,
            };
            let Some(process) = slot.as_mut() else {
                return;
            };
            let stdout = process.stdout.take();
            let stderr = process.stderr.take();
            (stdout, stderr)
        };
        let stdout_reader = stdout.map(|mut pipe| {
            thread::spawn(move || {
                let mut bytes = Vec::new();
                let result = std::io::Read::read_to_end(&mut pipe, &mut bytes);
                (result, bytes)
            })
        });
        let stderr_reader = stderr.map(|mut pipe| {
            thread::spawn(move || {
                let mut bytes = Vec::new();
                let result = std::io::Read::read_to_end(&mut pipe, &mut bytes);
                (result, bytes)
            })
        });
        let started = Instant::now();
        let status = loop {
            if worker_cancel.load(Ordering::SeqCst) || started.elapsed() > Duration::from_secs(45) {
                kill_process_tree(child_id);
                if let Ok(mut slot) = worker_child.lock()
                    && let Some(process) = slot.as_mut()
                {
                    let _ = process.kill();
                    let _ = process.wait();
                }
                break None;
            }
            let result = worker_child.lock().ok().and_then(|mut slot| {
                slot.as_mut()
                    .and_then(|process| process.try_wait().ok())
                    .flatten()
            });
            if let Some(status) = result {
                break Some(status);
            }
            thread::sleep(Duration::from_millis(100));
        };
        if let Ok(mut slot) = worker_child.lock() {
            *slot = None;
        }
        let stdout = stdout_reader.and_then(|reader| reader.join().ok());
        let stderr = stderr_reader.and_then(|reader| reader.join().ok());
        let result = if worker_cancel.load(Ordering::SeqCst) {
            Err("Metadata lookup cancelled.".to_owned())
        } else if status.is_none() {
            Err("Metadata lookup timed out after 45 seconds.".to_owned())
        } else {
            let stdout = stdout
                .and_then(|(result, bytes)| result.ok().map(|_| bytes))
                .unwrap_or_default();
            let stderr = stderr
                .and_then(|(result, bytes)| result.ok().map(|_| bytes))
                .unwrap_or_default();
            if status.is_some_and(|status| !status.success()) {
                let tail = String::from_utf8_lossy(&stderr);
                Err(tail
                    .lines()
                    .rev()
                    .take(8)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join(" | "))
            } else {
                serde_json::from_slice::<VideoMetadata>(&stdout)
                    .map_err(|error| format!("yt-dlp returned invalid metadata: {error}"))
                    .and_then(|metadata| {
                        if metadata.duration.is_none_or(|d| !d.is_finite() || d <= 0.0) {
                            Err("The source has no usable duration.".to_owned())
                        } else {
                            Ok(metadata)
                        }
                    })
            }
        };
        let _ = tx.send(result);
    });
    Ok(MetadataJob {
        events,
        cancel,
        child,
    })
}

pub fn range_cache_path(video_id: &str, start: f64, end: f64, _quality: &str) -> PathBuf {
    let key = format!("{video_id}|{start:.3}|{end:.3}");
    let digest = Sha256::digest(key.as_bytes());
    cache_dir().join(format!("{}.mp4", hex(&digest)))
}

pub fn begin_download(
    url: String,
    video_id: String,
    start: f64,
    end: f64,
    quality: String,
) -> Result<DownloadJob> {
    if !start.is_finite() || !end.is_finite() || start < 0.0 || start >= end {
        return Err(anyhow!("Enter a valid start and end time."));
    }
    let output_path = range_cache_path(&video_id, start, end, &quality);
    if output_path.is_file() && fs::metadata(&output_path).is_ok_and(|m| m.len() > 0) {
        let (tx, events) = unbounded();
        let _ = tx.send(DownloadEvent::Finished(output_path));
        return Ok(DownloadJob {
            events,
            cancel: Arc::new(AtomicBool::new(false)),
            child: Arc::new(Mutex::new(None)),
        });
    }
    fs::create_dir_all(
        output_path
            .parent()
            .context("Cache directory is unavailable")?,
    )?;
    let temp_path = output_path.with_extension("partial.mp4");
    let _ = fs::remove_file(&temp_path);
    let start_s = format_time(start);
    let end_s = format_time(end);
    let child_handle = Arc::new(Mutex::new(None));
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, events) = unbounded();
    let diagnostics = Arc::new(Mutex::new(VecDeque::<String>::new()));
    let worker_child = Arc::clone(&child_handle);
    let worker_cancel = Arc::clone(&cancel);
    let worker_diagnostics = Arc::clone(&diagnostics);
    thread::spawn(move || {
        let mut command = base_command();
        command
            .arg("--newline")
            .arg("--progress")
            .arg("--download-sections")
            .arg(format!("*{start_s}-{end_s}"))
            .arg("--force-keyframes-at-cuts")
            .arg("--format")
            .arg("bestvideo[ext=mp4]+bestaudio[ext=m4a]/best[ext=mp4]/best")
            .arg("--merge-output-format")
            .arg("mp4")
            .arg("--remux-video")
            .arg("mp4")
            .arg("--output")
            .arg(&temp_path)
            .arg(&url)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_console(&mut command);
        let spawned = command.spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                let _ = tx.send(DownloadEvent::Failed(format!(
                    "Could not start yt-dlp: {error}"
                )));
                return;
            }
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let child_id = child.id();
        if let Ok(mut shared) = worker_child.lock() {
            *shared = Some(child);
        }

        let (line_tx, line_rx) = std::sync::mpsc::channel::<(bool, String)>();
        if let Some(stdout) = stdout {
            let line_tx = line_tx.clone();
            thread::spawn(move || {
                for line in BufReader::new(stdout)
                    .lines()
                    .map_while(std::result::Result::ok)
                {
                    let _ = line_tx.send((false, line));
                }
            });
        }
        if let Some(stderr) = stderr {
            let line_tx = line_tx.clone();
            thread::spawn(move || {
                for line in BufReader::new(stderr)
                    .lines()
                    .map_while(std::result::Result::ok)
                {
                    let _ = line_tx.send((true, line));
                }
            });
        }
        drop(line_tx);

        let mut cancel_tree_sent = false;
        loop {
            if worker_cancel.load(Ordering::SeqCst) {
                if !cancel_tree_sent {
                    kill_process_tree(child_id);
                    cancel_tree_sent = true;
                }
                if let Ok(mut shared) = worker_child.lock()
                    && let Some(child) = shared.as_mut()
                {
                    let _ = child.kill();
                }
            }
            match line_rx.recv_timeout(Duration::from_millis(120)) {
                Ok((true, line)) => push_diagnostic(&worker_diagnostics, line),
                Ok((false, line)) => {
                    if let Some(progress) = parse_progress(&line) {
                        let _ = tx.send(DownloadEvent::Progress(progress));
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => (),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => (),
            }
            let status = worker_child
                .lock()
                .ok()
                .and_then(|mut shared| shared.as_mut().and_then(|c| c.try_wait().ok().flatten()));
            if let Some(status) = status {
                if let Ok(mut shared) = worker_child.lock() {
                    *shared = None;
                }
                if worker_cancel.load(Ordering::SeqCst) {
                    let _ = fs::remove_file(&temp_path);
                    let _ = tx.send(DownloadEvent::Cancelled);
                } else if status.success() {
                    match finalize_download(&temp_path, &output_path, start, end) {
                        Ok(()) => {
                            let _ = tx.send(DownloadEvent::Finished(output_path.clone()));
                        }
                        Err(error) => {
                            let _ = tx.send(DownloadEvent::Failed(error.to_string()));
                        }
                    }
                } else {
                    let _ = fs::remove_file(&temp_path);
                    let detail = diagnostic_text(&worker_diagnostics);
                    let message = if detail.is_empty() {
                        format!(
                            "yt-dlp exited with {status}. Confirm FFmpeg, Deno, and yt-dlp are installed."
                        )
                    } else {
                        format!("yt-dlp exited with {status}: {detail}")
                    };
                    let _ = tx.send(DownloadEvent::Failed(message));
                }
                return;
            }
        }
    });
    Ok(DownloadJob {
        events,
        cancel,
        child: child_handle,
    })
}

fn push_diagnostic(log: &Arc<Mutex<VecDeque<String>>>, line: String) {
    let line = line.trim().to_owned();
    if line.is_empty() {
        return;
    }
    if let Ok(mut lines) = log.lock() {
        if lines.len() >= 8 {
            lines.pop_front();
        }
        lines.push_back(line);
    }
}

fn diagnostic_text(log: &Arc<Mutex<VecDeque<String>>>) -> String {
    log.lock()
        .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join(" | "))
        .unwrap_or_default()
}

fn kill_process_tree(pid: u32) {
    if pid == 0 {
        return;
    }
    let mut command = Command::new("taskkill.exe");
    command
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_console(&mut command);
    let _ = command.status();
}

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000000);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

fn finalize_download(
    temp: &Path,
    output: &Path,
    expected_start: f64,
    expected_end: f64,
) -> Result<()> {
    if !temp.is_file() || fs::metadata(temp)?.len() == 0 {
        return Err(anyhow!("yt-dlp finished without producing a clip."));
    }
    let mut command = Command::new(tool_path("ffprobe"));
    command
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(temp);
    hide_console(&mut command);
    let probe = command
        .output()
        .context("Could not start ffprobe to verify the downloaded clip")?;
    if !probe.status.success() {
        return Err(anyhow!("ffprobe could not read the downloaded clip."));
    }
    let duration: f64 = String::from_utf8_lossy(&probe.stdout)
        .trim()
        .parse()
        .context("ffprobe returned an invalid clip duration")?;
    let target = expected_end - expected_start;
    let tolerance = 0.75_f64.max(target * 0.05);
    if !duration.is_finite() || (duration - target).abs() > tolerance {
        return Err(anyhow!(
            "Downloaded clip duration ({duration:.2}s) does not match the selected {:.2}s section.",
            target
        ));
    }
    if output.exists() {
        fs::remove_file(output)?;
    }
    fs::rename(temp, output)
        .with_context(|| format!("Could not move clip into cache: {}", output.display()))?;
    Ok(())
}

fn parse_progress(line: &str) -> Option<f32> {
    let marker = line.find("%")?;
    let before = &line[..marker];
    let start = before
        .rfind(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map_or(0, |i| i + 1);
    before[start..]
        .parse::<f32>()
        .ok()
        .map(|p| p.clamp(0.0, 100.0))
}

fn format_time(seconds: f64) -> String {
    let total_ms = (seconds.max(0.0) * 1000.0).round() as u64;
    let hours = total_ms / 3_600_000;
    let minutes = (total_ms / 60_000) % 60;
    let secs = (total_ms / 1000) % 60;
    let millis = total_ms % 1000;
    format!("{hours:02}:{minutes:02}:{secs:02}.{millis:03}")
}

fn hex(bytes: &[u8]) -> String {
    const CHARS: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(CHARS[(byte >> 4) as usize] as char);
        out.push(CHARS[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_key_distinguishes_source_ranges_without_claiming_quality_variants() {
        assert_ne!(
            range_cache_path("id", 1.0, 2.0, "best"),
            range_cache_path("id", 1.0, 3.0, "best")
        );
        assert_eq!(
            range_cache_path("id", 1.0, 2.0, "best"),
            range_cache_path("id", 1.0, 2.0, "720p")
        );
    }
    #[test]
    fn time_arguments_use_ffmpeg_timestamp_shape() {
        assert_eq!(format_time(3661.234), "01:01:01.234");
    }
    #[test]
    fn progress_parser_ignores_other_numbers() {
        assert_eq!(parse_progress("[download] 36.7% of 20MiB"), Some(36.7));
    }

    #[test]
    #[ignore = "requires internet access and working yt-dlp/FFmpeg/Deno sidecars"]
    fn downloads_and_decodes_a_real_youtube_section_through_app_jobs() {
        let url = "https://www.youtube.com/watch?v=jNQXAC9IVRw".to_owned();
        let expected_cache = range_cache_path("jNQXAC9IVRw", 2.0, 4.0, "best");
        let existed_before_metadata = expected_cache.is_file();
        let metadata_job = begin_metadata(url.clone()).expect("start yt-dlp metadata job");
        let metadata = metadata_job
            .events
            .recv_timeout(Duration::from_secs(60))
            .expect("metadata lookup timed out")
            .expect("yt-dlp metadata lookup failed");
        assert_eq!(metadata.id, "jNQXAC9IVRw");
        assert!(metadata.duration.unwrap_or_default() >= 4.0);
        assert_eq!(
            expected_cache.is_file(),
            existed_before_metadata,
            "metadata lookup must not create the media cache"
        );

        let job = begin_download(url, metadata.id, 2.0, 4.0, "best".to_owned())
            .expect("start selected-section download job");
        let deadline = Instant::now() + Duration::from_secs(180);
        let path = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "section download timed out");
            let event = match job
                .events
                .recv_timeout(remaining.min(Duration::from_secs(3)))
            {
                Ok(event) => event,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    panic!("section download event stream disconnected")
                }
            };
            match event {
                DownloadEvent::Progress(_) => (),
                DownloadEvent::Finished(path) => break path,
                DownloadEvent::Failed(error) => panic!("section download failed: {error}"),
                DownloadEvent::Cancelled => panic!("section download was unexpectedly cancelled"),
            }
        };
        assert!(path.is_file());
        let duration = crate::media::probe_duration(&path).expect("ffprobe clip duration");
        assert!(
            (duration - 2.0).abs() <= 0.75,
            "unexpected clip duration {duration:.3}s"
        );

        let mut command = Command::new(tool_path("ffmpeg"));
        command
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args(["-frames:v", "1", "-f", "null", "-"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        hide_console(&mut command);
        let decoded = command.output().expect("start FFmpeg decode check");
        assert!(
            decoded.status.success(),
            "FFmpeg could not decode the downloaded clip: {}",
            String::from_utf8_lossy(&decoded.stderr)
        );
    }
}
