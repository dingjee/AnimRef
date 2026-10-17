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
    Arc, Mutex, OnceLock,
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
    Progress(DownloadProgress),
    Finished(PathBuf),
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadStage {
    Preparing,
    Fetching,
    Finalizing,
    Verifying,
    Finished,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DownloadProgress {
    pub stage: DownloadStage,
    /// Output media timestamp from FFmpeg's machine-readable `-progress` protocol.
    pub elapsed_seconds: Option<f64>,
    pub target_seconds: Option<f64>,
    /// FFmpeg output file bytes (`total_size`), not network bytes.
    pub output_bytes: Option<u64>,
    pub detail: Option<String>,
}

impl DownloadProgress {
    fn stage(stage: DownloadStage, target_seconds: Option<f64>) -> Self {
        Self {
            stage,
            elapsed_seconds: None,
            target_seconds,
            output_bytes: None,
            detail: None,
        }
    }
}

pub struct DownloadJob {
    pub events: Receiver<DownloadEvent>,
    cancel: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    completion_gate: Arc<Mutex<()>>,
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
        if let Ok(_gate) = self.completion_gate.lock() {
            self.cancel.store(true, Ordering::SeqCst);
        } else {
            self.cancel.store(true, Ordering::SeqCst);
        }
        if let Ok(mut child) = self.child.lock()
            && let Some(child) = child.as_mut()
            && child.try_wait().ok().flatten().is_none()
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
    fs::create_dir_all(
        output_path
            .parent()
            .context("Cache directory is unavailable")?,
    )?;
    let job_dir = unique_job_dir(output_path.parent().unwrap_or(Path::new(".")))?;
    let temp_path = job_dir.join("section.mp4");
    let start_s = format_time(start);
    let end_s = format_time(end);
    let child_handle = Arc::new(Mutex::new(None));
    let cancel = Arc::new(AtomicBool::new(false));
    let completion_gate = Arc::new(Mutex::new(()));
    let (tx, events) = unbounded();
    let diagnostics = Arc::new(Mutex::new(VecDeque::<String>::new()));
    let worker_child = Arc::clone(&child_handle);
    let worker_cancel = Arc::clone(&cancel);
    let worker_completion_gate = Arc::clone(&completion_gate);
    let worker_diagnostics = Arc::clone(&diagnostics);
    thread::spawn(move || {
        let target_seconds = end - start;
        let _ = tx.send(DownloadEvent::Progress(DownloadProgress::stage(
            DownloadStage::Preparing,
            Some(target_seconds),
        )));
        if worker_cancel.load(Ordering::SeqCst) {
            let _ = fs::remove_dir_all(&job_dir);
            let _ = tx.send(DownloadEvent::Cancelled);
            return;
        }
        if output_path.is_file() {
            let _ = tx.send(DownloadEvent::Progress(DownloadProgress {
                stage: DownloadStage::Preparing,
                elapsed_seconds: None,
                target_seconds: Some(target_seconds),
                output_bytes: None,
                detail: Some("Checking cached clip".into()),
            }));
            match verify_download_cancellable(
                &output_path,
                start,
                end,
                &worker_child,
                &worker_cancel,
            ) {
                Ok(()) => {
                    let gate = worker_completion_gate.lock();
                    if worker_cancel.load(Ordering::SeqCst) || gate.is_err() {
                        let _ = fs::remove_dir_all(&job_dir);
                        let _ = tx.send(DownloadEvent::Cancelled);
                    } else {
                        let _ = tx.send(DownloadEvent::Progress(DownloadProgress::stage(
                            DownloadStage::Finished,
                            Some(target_seconds),
                        )));
                        let _ = tx.send(DownloadEvent::Finished(output_path.clone()));
                        let _ = fs::remove_dir_all(&job_dir);
                    }
                    return;
                }
                Err(_) if worker_cancel.load(Ordering::SeqCst) => {
                    let _ = fs::remove_dir_all(&job_dir);
                    let _ = tx.send(DownloadEvent::Cancelled);
                    return;
                }
                Err(_) => {
                    if let Err(error) = fs::remove_file(&output_path) {
                        let _ = fs::remove_dir_all(&job_dir);
                        let _ = tx.send(DownloadEvent::Failed(format!(
                            "The cached clip is invalid and could not be removed: {error}"
                        )));
                        return;
                    }
                }
            }
        }
        if worker_cancel.load(Ordering::SeqCst) {
            let _ = fs::remove_dir_all(&job_dir);
            let _ = tx.send(DownloadEvent::Cancelled);
            return;
        }
        let mut command = base_command();
        command
            .arg("--newline")
            .arg("--progress")
            .arg("--download-sections")
            .arg(format!("*{start_s}-{end_s}"))
            .arg("--force-keyframes-at-cuts")
            .arg("--downloader")
            .arg("ffmpeg")
            .arg("--downloader-args")
            .arg("ffmpeg_o:-progress pipe:1 -nostats")
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
                let _ = fs::remove_dir_all(&job_dir);
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

        let (line_tx, line_rx) = std::sync::mpsc::channel::<ReaderMessage>();
        let mut readers = Vec::new();
        if let Some(stdout) = stdout {
            let line_tx = line_tx.clone();
            readers.push(thread::spawn(move || {
                for line in BufReader::new(stdout)
                    .lines()
                    .map_while(std::result::Result::ok)
                {
                    if line_tx.send(ReaderMessage::Line(false, line)).is_err() {
                        return;
                    }
                }
                let _ = line_tx.send(ReaderMessage::Eof(false));
            }));
        }
        if let Some(stderr) = stderr {
            let line_tx = line_tx.clone();
            readers.push(thread::spawn(move || {
                for line in BufReader::new(stderr)
                    .lines()
                    .map_while(std::result::Result::ok)
                {
                    if line_tx.send(ReaderMessage::Line(true, line)).is_err() {
                        return;
                    }
                }
                let _ = line_tx.send(ReaderMessage::Eof(true));
            }));
        }
        drop(line_tx);

        let mut cancel_tree_sent = false;
        let mut announced_fetching = false;
        let mut accumulator = FfmpegProgressAccumulator::default();
        let mut eof_count = 2usize.saturating_sub(readers.len());
        let mut process_status = None;
        let mut stage = DownloadStage::Fetching;
        let mut drain_started = None;
        loop {
            if worker_cancel.load(Ordering::SeqCst) {
                if process_status.is_none() && !cancel_tree_sent {
                    kill_process_tree(child_id);
                    cancel_tree_sent = true;
                }
                if let Ok(mut shared) = worker_child.lock()
                    && let Some(child) = shared.as_mut()
                {
                    let _ = child.kill();
                }
            }
            match line_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(ReaderMessage::Eof(_is_stderr)) => eof_count += 1,
                Ok(ReaderMessage::Line(is_stderr, line)) => {
                    if let Some((key, value)) = parse_ffmpeg_progress_field(&line) {
                        if key == "progress" {
                            if matches!(value, "continue" | "end")
                                && let Some(snapshot) = accumulator.commit(value == "end")
                            {
                                announced_fetching = true;
                                let _ = tx.send(DownloadEvent::Progress(DownloadProgress {
                                    stage,
                                    elapsed_seconds: snapshot.elapsed,
                                    target_seconds: Some(target_seconds),
                                    output_bytes: snapshot.bytes,
                                    detail: if value == "end" {
                                        Some("FFmpeg section output ended".into())
                                    } else {
                                        None
                                    },
                                }));
                            }
                        } else {
                            accumulator.update(key, value);
                        }
                    } else if is_postprocessing_log(&line) {
                        if stage != DownloadStage::Finalizing {
                            stage = DownloadStage::Finalizing;
                            let _ = tx.send(DownloadEvent::Progress(DownloadProgress::stage(
                                DownloadStage::Finalizing,
                                Some(target_seconds),
                            )));
                        }
                    } else if is_stderr {
                        push_diagnostic(&worker_diagnostics, line);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => eof_count = 2,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => (),
            }
            if process_status.is_none() {
                let poll_result = match worker_child.lock() {
                    Ok(mut shared) => match shared.as_mut() {
                        Some(child) => child.try_wait(),
                        None => Ok(None),
                    },
                    Err(error) => Err(std::io::Error::other(format!(
                        "download process lock was poisoned: {error}"
                    ))),
                };
                match poll_result {
                    Ok(status) => process_status = status,
                    Err(error) => {
                        kill_process_tree(child_id);
                        if let Ok(mut shared) = worker_child.lock() {
                            if let Some(child) = shared.as_mut() {
                                let _ = child.kill();
                                let _ = child.wait();
                            }
                            *shared = None;
                        }
                        for reader in readers.drain(..) {
                            let _ = reader.join();
                        }
                        let _ = fs::remove_dir_all(&job_dir);
                        let _ = tx.send(DownloadEvent::Failed(format!(
                            "Could not wait for yt-dlp to finish: {error}"
                        )));
                        return;
                    }
                }
                if process_status.is_some() {
                    drain_started = Some(Instant::now());
                }
            }
            if process_status.is_some()
                && eof_count < 2
                && drain_started.is_some_and(|started| started.elapsed() > Duration::from_secs(12))
            {
                if let Ok(mut shared) = worker_child.lock() {
                    if let Some(child) = shared.as_mut() {
                        let _ = child.kill();
                    }
                    *shared = None;
                }
                let _ = fs::remove_dir_all(&job_dir);
                let _ = tx.send(DownloadEvent::Failed(
                    "yt-dlp output streams did not close after the process exited.".into(),
                ));
                return;
            }
            if process_status.is_some() && eof_count >= 2 {
                let status = process_status.take().expect("checked status");
                if let Ok(mut shared) = worker_child.lock() {
                    *shared = None;
                }
                for reader in readers.drain(..) {
                    let _ = reader.join();
                }
                if worker_cancel.load(Ordering::SeqCst) {
                    let _ = fs::remove_dir_all(&job_dir);
                    let _ = tx.send(DownloadEvent::Cancelled);
                } else if status.success() {
                    if !announced_fetching {
                        let _ = tx.send(DownloadEvent::Progress(DownloadProgress {
                            stage,
                            elapsed_seconds: None,
                            target_seconds: Some(target_seconds),
                            output_bytes: None,
                            detail: Some("Waiting for section output".into()),
                        }));
                    }
                    if stage != DownloadStage::Finalizing {
                        let _ = tx.send(DownloadEvent::Progress(DownloadProgress::stage(
                            DownloadStage::Finalizing,
                            Some(target_seconds),
                        )));
                    }
                    if worker_cancel.load(Ordering::SeqCst) {
                        let _ = fs::remove_dir_all(&job_dir);
                        let _ = tx.send(DownloadEvent::Cancelled);
                        return;
                    }
                    let _ = tx.send(DownloadEvent::Progress(DownloadProgress::stage(
                        DownloadStage::Verifying,
                        Some(target_seconds),
                    )));
                    match verify_download_cancellable(
                        &temp_path,
                        start,
                        end,
                        &worker_child,
                        &worker_cancel,
                    ) {
                        Ok(()) => {
                            if worker_cancel.load(Ordering::SeqCst) {
                                let _ = fs::remove_dir_all(&job_dir);
                                let _ = tx.send(DownloadEvent::Cancelled);
                                return;
                            }
                            let gate = worker_completion_gate.lock();
                            if worker_cancel.load(Ordering::SeqCst) || gate.is_err() {
                                let _ = fs::remove_dir_all(&job_dir);
                                let _ = tx.send(DownloadEvent::Cancelled);
                                return;
                            }
                            match publish_download(&temp_path, &output_path) {
                                Ok(()) => {
                                    let _ =
                                        tx.send(DownloadEvent::Progress(DownloadProgress::stage(
                                            DownloadStage::Finished,
                                            Some(target_seconds),
                                        )));
                                    let _ = tx.send(DownloadEvent::Finished(output_path.clone()));
                                }
                                Err(error) => {
                                    let _ = tx.send(DownloadEvent::Failed(error.to_string()));
                                }
                            }
                        }
                        Err(_) if worker_cancel.load(Ordering::SeqCst) => {
                            let _ = tx.send(DownloadEvent::Cancelled);
                        }
                        Err(error) => {
                            let _ = tx.send(DownloadEvent::Failed(error.to_string()));
                        }
                    }
                } else {
                    let _ = fs::remove_dir_all(&job_dir);
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
                let _ = fs::remove_dir_all(&job_dir);
                return;
            }
        }
    });
    Ok(DownloadJob {
        events,
        cancel,
        child: child_handle,
        completion_gate,
    })
}

fn push_diagnostic(log: &Arc<Mutex<VecDeque<String>>>, line: String) {
    let line: String = line.trim().chars().take(1024).collect();
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

enum ReaderMessage {
    Line(bool, String),
    Eof(bool),
}

#[derive(Default)]
struct FfmpegProgressAccumulator {
    elapsed: Option<f64>,
    bytes: Option<u64>,
    pending_elapsed: Option<f64>,
    pending_elapsed_priority: u8,
    pending_bytes: Option<u64>,
}

struct ProgressSnapshot {
    elapsed: Option<f64>,
    bytes: Option<u64>,
}

impl FfmpegProgressAccumulator {
    fn update(&mut self, key: &str, value: &str) {
        let parsed_time = match key {
            "out_time_us" => value
                .parse::<f64>()
                .ok()
                .map(|value| (value / 1_000_000.0, 3)),
            "out_time_ms" => value
                .parse::<f64>()
                .ok()
                .map(|value| (value / 1_000_000.0, 2)),
            "out_time" => parse_clock(value).map(|seconds| (seconds, 1)),
            _ => None,
        };
        if let Some((seconds, priority)) = parsed_time
            && seconds.is_finite()
            && seconds >= 0.0
            && priority >= self.pending_elapsed_priority
        {
            self.pending_elapsed = Some(seconds);
            self.pending_elapsed_priority = priority;
        }
        if key == "total_size"
            && let Ok(bytes) = value.parse::<u64>()
        {
            self.pending_bytes = Some(bytes);
        }
    }

    fn commit(&mut self, _ended: bool) -> Option<ProgressSnapshot> {
        if let Some(elapsed) = self.pending_elapsed.take() {
            self.elapsed = Some(elapsed);
        }
        self.pending_elapsed_priority = 0;
        if let Some(bytes) = self.pending_bytes.take() {
            self.bytes = Some(bytes);
        }
        // FFmpeg may report N/A before its first output frame. Still expose the
        // protocol snapshot at each boundary without inventing a percentage.
        Some(ProgressSnapshot {
            elapsed: self.elapsed,
            bytes: self.bytes,
        })
    }
}

fn parse_ffmpeg_progress_field(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.trim().split_once('=')?;
    matches!(
        key,
        "out_time_us" | "out_time_ms" | "out_time" | "total_size" | "progress"
    )
    .then_some((key, value))
}

fn is_postprocessing_log(line: &str) -> bool {
    [
        "[Merger]",
        "[VideoRemuxer]",
        "[VideoConvertor]",
        "[Fixup",
        "[Metadata",
        "[EmbedSubtitle",
    ]
    .iter()
    .any(|marker| line.contains(marker))
}

fn unique_job_dir(parent: &Path) -> Result<PathBuf> {
    static NEXT_ID: OnceLock<std::sync::atomic::AtomicU64> = OnceLock::new();
    let counter = NEXT_ID.get_or_init(|| std::sync::atomic::AtomicU64::new(1));
    for _ in 0..32 {
        let id = counter.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".youtube-job-{}-{id}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).context("Could not create a private download directory");
            }
        }
    }
    Err(anyhow!("Could not allocate a unique download directory."))
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

fn verify_download_cancellable(
    temp: &Path,
    expected_start: f64,
    expected_end: f64,
    child_slot: &Arc<Mutex<Option<Child>>>,
    cancel: &Arc<AtomicBool>,
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
        .arg(temp)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_console(&mut command);
    let mut probe = command
        .spawn()
        .context("Could not start ffprobe to verify the downloaded clip")?;
    let stdout = probe.stdout.take();
    let stderr = probe.stderr.take();
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
    if let Ok(mut slot) = child_slot.lock() {
        *slot = Some(probe);
    }
    let started = Instant::now();
    let status = loop {
        if cancel.load(Ordering::SeqCst) || started.elapsed() > Duration::from_secs(15) {
            if let Ok(mut slot) = child_slot.lock() {
                if let Some(process) = slot.as_mut() {
                    kill_process_tree(process.id());
                    let _ = process.kill();
                    let _ = process.wait();
                }
                *slot = None;
            }
            if let Some(reader) = stdout_reader {
                let _ = reader.join();
            }
            if let Some(reader) = stderr_reader {
                let _ = reader.join();
            }
            return Err(anyhow!(if cancel.load(Ordering::SeqCst) {
                "Download cancelled."
            } else {
                "ffprobe timed out while verifying the clip."
            }));
        }
        let result = child_slot.lock().ok().and_then(|mut slot| {
            slot.as_mut()
                .and_then(|child| child.try_wait().ok().flatten())
        });
        if let Some(status) = result {
            break status;
        }
        thread::sleep(Duration::from_millis(60));
    };
    if let Ok(mut slot) = child_slot.lock() {
        *slot = None;
    }
    let stdout = stdout_reader
        .and_then(|reader| reader.join().ok())
        .and_then(|(result, bytes)| result.ok().map(|_| bytes))
        .unwrap_or_default();
    let _stderr = stderr_reader.and_then(|reader| reader.join().ok());
    if cancel.load(Ordering::SeqCst) {
        return Err(anyhow!("Download cancelled."));
    }
    if !status.success() {
        return Err(anyhow!("ffprobe could not read the downloaded clip."));
    }
    let duration: f64 = String::from_utf8_lossy(&stdout)
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
    Ok(())
}

fn publish_download(temp: &Path, output: &Path) -> Result<()> {
    match fs::rename(temp, output) {
        Ok(()) => {}
        Err(error) if output.exists() => {
            // A parallel job may have published the same range. Keep its
            // completed cache entry; the caller already validated its own clip.
            let _ = error;
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Could not move clip into cache: {}", output.display()));
        }
    }
    Ok(())
}

fn parse_clock(value: &str) -> Option<f64> {
    let mut parts = value.split(':');
    let hours = parts.next()?.parse::<f64>().ok()?;
    let minutes = parts.next()?.parse::<f64>().ok()?;
    let seconds = parts.next()?.parse::<f64>().ok()?;
    if parts.next().is_some()
        || !hours.is_finite()
        || !minutes.is_finite()
        || !seconds.is_finite()
        || hours < 0.0
        || !(0.0..60.0).contains(&minutes)
        || !(0.0..60.0).contains(&seconds)
    {
        return None;
    }
    let result = hours * 3600.0 + minutes * 60.0 + seconds;
    (result.is_finite() && result >= 0.0).then_some(result)
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

    struct TestCacheCleanup(PathBuf);

    impl Drop for TestCacheCleanup {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

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
    fn ffmpeg_progress_parser_uses_output_timestamps_and_bytes_only() {
        let mut accumulator = FfmpegProgressAccumulator::default();
        accumulator.update("out_time_us", "1500000");
        accumulator.update("out_time", "00:00:01.000");
        accumulator.update("total_size", "4096");
        assert!(parse_ffmpeg_progress_field("[download] 99.9% of 1GiB").is_none());
        assert_eq!(
            accumulator.commit(false).map(|s| (s.elapsed, s.bytes)),
            Some((Some(1.5), Some(4096)))
        );
        assert_eq!(
            accumulator.commit(false).map(|s| (s.elapsed, s.bytes)),
            Some((Some(1.5), Some(4096)))
        );
        assert_eq!(
            parse_ffmpeg_progress_field("progress=end"),
            Some(("progress", "end"))
        );
        assert_eq!(parse_clock("00:00:02.250"), Some(2.25));
        assert_eq!(parse_clock("00:00:-1.000"), None);
        assert_eq!(parse_clock("00:60:00.000"), None);
        assert_eq!(parse_clock("00:00:01.000:extra"), None);
    }

    #[test]
    #[ignore = "opt-in network acceptance: requires yt-dlp/FFmpeg/Deno sidecars and public YouTube access"]
    fn downloads_a_fresh_youtube_section_and_reports_real_ffmpeg_progress() {
        let url = "https://www.youtube.com/watch?v=jNQXAC9IVRw".to_owned();
        let test_video_id = "animref-test-fresh-progress-jNQXAC9IVRw-4-6";
        let expected_cache = range_cache_path(test_video_id, 4.0, 6.0, "best");
        let _cleanup = TestCacheCleanup(expected_cache.clone());
        // This deterministic test-only key cannot point at a user's normal cache entry.
        let _ = fs::remove_file(&expected_cache);

        let job = begin_download(url, test_video_id.to_owned(), 4.0, 6.0, "best".to_owned())
            .expect("start selected-section download job");
        let deadline = Instant::now() + Duration::from_secs(180);
        let mut last_stage = None;
        let mut saw_real_time_and_bytes = false;
        let mut logged_real_progress = false;
        let mut saw_verifying = false;
        let mut saw_finished_stage = false;
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
                DownloadEvent::Progress(progress) => {
                    let stage_order = match progress.stage {
                        DownloadStage::Preparing => 0,
                        DownloadStage::Fetching => 1,
                        DownloadStage::Finalizing => 2,
                        DownloadStage::Verifying => 3,
                        DownloadStage::Finished => 4,
                    };
                    if let Some(last) = last_stage {
                        assert!(stage_order >= last, "download stage regressed");
                    }
                    last_stage = Some(stage_order);
                    saw_real_time_and_bytes |= progress.elapsed_seconds.is_some_and(|s| s > 0.0)
                        && progress.output_bytes.is_some_and(|bytes| bytes > 0);
                    if !logged_real_progress
                        && let (Some(seconds), Some(bytes)) =
                            (progress.elapsed_seconds, progress.output_bytes)
                        && seconds > 0.0
                        && bytes > 0
                    {
                        println!(
                            "[yt-progress] stage={:?} section_output={seconds:.3}s output_bytes={bytes}",
                            progress.stage
                        );
                        logged_real_progress = true;
                    }
                    saw_verifying |= progress.stage == DownloadStage::Verifying;
                    saw_finished_stage |= progress.stage == DownloadStage::Finished;
                }
                DownloadEvent::Finished(path) => break path,
                DownloadEvent::Failed(error) => panic!("section download failed: {error}"),
                DownloadEvent::Cancelled => panic!("section download was unexpectedly cancelled"),
            }
        };
        assert!(
            saw_real_time_and_bytes,
            "missing FFmpeg output timestamp/size progress"
        );
        assert!(saw_verifying, "missing verification stage");
        assert!(saw_finished_stage, "missing finished progress stage");
        assert_eq!(
            path, expected_cache,
            "download did not publish to the isolated test cache key"
        );
        assert!(path.is_file());
        let mut probe = Command::new(tool_path("ffprobe"));
        probe
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
            ])
            .arg(&path);
        hide_console(&mut probe);
        let output = probe.output().expect("start ffprobe clip duration check");
        assert!(
            output.status.success(),
            "ffprobe could not read the test clip"
        );
        let duration: f64 = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .expect("ffprobe clip duration");
        assert!(
            (duration - 2.0).abs() <= 0.75,
            "unexpected clip duration {duration:.3}s"
        );

        // A valid cache entry must finish without starting a new fetch. This
        // deliberately unusable URL makes an accidental download fail quickly.
        let cached_job = begin_download(
            "not-a-valid-url".into(),
            test_video_id.to_owned(),
            4.0,
            6.0,
            "best".to_owned(),
        )
        .expect("start cache reuse check");
        let cache_deadline = Instant::now() + Duration::from_secs(10);
        let cached_path = loop {
            let remaining = cache_deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "validated cache reuse timed out");
            match cached_job
                .events
                .recv_timeout(remaining.min(Duration::from_secs(2)))
            {
                Ok(DownloadEvent::Progress(progress)) => assert_ne!(
                    progress.stage,
                    DownloadStage::Fetching,
                    "cache reuse unexpectedly started a fresh fetch"
                ),
                Ok(DownloadEvent::Finished(cached_path)) => break cached_path,
                Ok(DownloadEvent::Failed(error)) => {
                    panic!("validated cache was not reusable: {error}")
                }
                Ok(DownloadEvent::Cancelled) => panic!("cache reuse was unexpectedly cancelled"),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    panic!("cache reuse event stream disconnected")
                }
            }
        };
        assert_eq!(cached_path, expected_cache);
    }
}
