use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::model::PlaybackState;

const MAX_HISTORY_FRAMES: usize = 240;
const MAX_HISTORY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Video,
    Gif,
}

#[derive(Debug, Clone)]
pub enum DecodedPixels {
    Jpeg(Vec<u8>),
    Rgba {
        width: u32,
        height: u32,
        bytes: Vec<u8>,
    },
}

#[derive(Debug, Clone)]
pub enum PlaybackEvent {
    Frame {
        epoch: u64,
        position_seconds: f64,
        frame_index: Option<u64>,
        pixels: DecodedPixels,
    },
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlaybackCommand {
    Play,
    Pause,
    Toggle,
    Seek(f64),
    StepForward,
    StepBackward,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackSnapshot {
    pub paused: bool,
    pub position_seconds: f64,
    pub duration_seconds: Option<f64>,
    pub frame_index: Option<u64>,
    pub epoch: u64,
}

pub struct PlaybackSession {
    pub events: Receiver<PlaybackEvent>,
    commands: Sender<PlaybackCommand>,
    snapshot: Arc<Mutex<PlaybackSnapshot>>,
    worker: Option<JoinHandle<()>>,
}

impl PlaybackSession {
    pub fn start(
        path: PathBuf,
        kind: MediaKind,
        duration: Option<f64>,
        pairs: Vec<[f64; 2]>,
        active: usize,
        state: PlaybackState,
    ) -> Self {
        let (event_tx, events) = bounded(2);
        let (commands, command_rx) = unbounded();
        let range = selected_range(duration, &pairs, active);
        let initial_position = clamp_position(state.position_seconds, range, duration);
        let initial = PlaybackSnapshot {
            paused: state.paused,
            position_seconds: initial_position,
            duration_seconds: duration,
            frame_index: None,
            epoch: 0,
        };
        let snapshot = Arc::new(Mutex::new(initial));
        let worker_snapshot = Arc::clone(&snapshot);
        let worker = thread::spawn(move || {
            run_session(
                path,
                kind,
                range,
                initial,
                command_rx,
                event_tx,
                worker_snapshot,
            )
        });
        Self {
            events,
            commands,
            snapshot,
            worker: Some(worker),
        }
    }

    pub fn command(&self, command: PlaybackCommand) {
        let _ = self.commands.send(command);
    }
    pub fn snapshot(&self) -> PlaybackSnapshot {
        *self.snapshot.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for PlaybackSession {
    fn drop(&mut self) {
        let _ = self.commands.send(PlaybackCommand::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Debug, Clone)]
struct Frame {
    pts: f64,
    duration: f64,
    index: u64,
    pixels: DecodedPixels,
}
impl Frame {
    fn bytes(&self) -> usize {
        match &self.pixels {
            DecodedPixels::Jpeg(bytes) => bytes.len(),
            DecodedPixels::Rgba { bytes, .. } => bytes.len(),
        }
    }
}

fn selected_range(duration: Option<f64>, pairs: &[[f64; 2]], active: usize) -> (f64, Option<f64>) {
    let Some(duration) = duration.filter(|d| d.is_finite() && *d > 0.0) else {
        return (0.0, None);
    };
    let pair = pairs.get(active).copied().unwrap_or([0.0, 100.0]);
    (
        pair[0].clamp(0.0, 100.0) * duration / 100.0,
        Some(pair[1].clamp(0.0, 100.0) * duration / 100.0),
    )
}

fn clamp_position(position: f64, range: (f64, Option<f64>), duration: Option<f64>) -> f64 {
    let lower = range.0;
    let upper = range.1.or(duration).unwrap_or(f64::MAX);
    if !position.is_finite() || position >= upper {
        lower
    } else {
        position.max(lower)
    }
}

enum PtsMessage {
    Frame(u64, f64, f64),
    Diagnostic(String),
    Eof,
}
struct Decoder {
    child: Child,
    frame_rx: Receiver<Option<(u64, DecodedPixels)>>,
    pts_rx: Receiver<PtsMessage>,
    stop: Arc<AtomicBool>,
}

fn run_session(
    path: PathBuf,
    kind: MediaKind,
    range: (f64, Option<f64>),
    mut snapshot: PlaybackSnapshot,
    commands: Receiver<PlaybackCommand>,
    events: Sender<PlaybackEvent>,
    shared: Arc<Mutex<PlaybackSnapshot>>,
) {
    if range.1.is_some_and(|end| end <= range.0) {
        let _ = events.send(PlaybackEvent::Failed("Playback range is empty.".into()));
        return;
    }
    let mut origin = snapshot.position_seconds;
    let mut decoder = match start_decoder(&path, kind, origin, range.1) {
        Ok(decoder) => decoder,
        Err(error) => {
            let _ = events.send(PlaybackEvent::Failed(error));
            return;
        }
    };
    let mut diagnostics = VecDeque::<String>::new();
    let mut frame_map = BTreeMap::<u64, DecodedPixels>::new();
    let mut pts_map = BTreeMap::<u64, f64>::new();
    let mut duration_map = BTreeMap::<u64, f64>::new();
    let mut next_index = 0u64;
    let mut frame_eof = false;
    let mut pts_eof = false;
    let mut eof_handled = false;
    let mut loop_after: Option<Instant> = None;
    let mut pending_event: Option<PlaybackEvent> = None;
    let mut history = VecDeque::<Frame>::new();
    let mut displayed_index = None::<u64>;
    let mut requested_index = None::<u64>;
    let mut base_pts = None::<f64>;
    let mut base_wall = Instant::now();

    loop {
        while let Ok(command) = commands.try_recv() {
            if command == PlaybackCommand::Stop {
                decoder.stop.store(true, Ordering::SeqCst);
                let _ = decoder.child.kill();
                let _ = decoder.child.wait();
                return;
            }
            pending_event = None;
            snapshot.epoch = snapshot.epoch.wrapping_add(1);
            let mut reemit_current = false;
            match command {
                PlaybackCommand::Play => {
                    snapshot.paused = false;
                    if let Some(pts) = history
                        .iter()
                        .find(|f| Some(f.index) == displayed_index)
                        .map(|f| f.pts)
                    {
                        base_pts = Some(pts);
                        base_wall = Instant::now();
                    }
                    eof_handled = false;
                }
                PlaybackCommand::Pause => {
                    snapshot.paused = true;
                    reemit_current = true;
                }
                PlaybackCommand::Toggle => {
                    snapshot.paused = !snapshot.paused;
                    if !snapshot.paused {
                        if let Some(pts) = history
                            .iter()
                            .find(|f| Some(f.index) == displayed_index)
                            .map(|f| f.pts)
                        {
                            base_pts = Some(pts);
                            base_wall = Instant::now();
                        }
                        eof_handled = false;
                    } else {
                        reemit_current = true;
                    }
                }
                PlaybackCommand::Seek(position) => {
                    snapshot.position_seconds =
                        clamp_position(position, range, snapshot.duration_seconds);
                    snapshot.frame_index = None;
                    origin = snapshot.position_seconds;
                    decoder.stop.store(true, Ordering::SeqCst);
                    let _ = decoder.child.kill();
                    let _ = decoder.child.wait();
                    match start_decoder(&path, kind, origin, range.1) {
                        Ok(restarted) => decoder = restarted,
                        Err(error) => {
                            let _ = events.try_send(PlaybackEvent::Failed(error));
                            return;
                        }
                    }
                    frame_map.clear();
                    pts_map.clear();
                    duration_map.clear();
                    history.clear();
                    next_index = 0;
                    frame_eof = false;
                    pts_eof = false;
                    loop_after = None;
                    displayed_index = None;
                    requested_index = None;
                    base_pts = None;
                    eof_handled = false;
                }
                PlaybackCommand::StepForward => {
                    snapshot.paused = true;
                    requested_index = Some(
                        displayed_index
                            .unwrap_or_else(|| next_index.saturating_sub(1))
                            .saturating_add(1),
                    );
                }
                PlaybackCommand::StepBackward => {
                    snapshot.paused = true;
                    let target = displayed_index.unwrap_or(0).saturating_sub(1);
                    if history.iter().any(|f| f.index == target) {
                        requested_index = Some(target);
                    } else if displayed_index.is_some_and(|current| current > 0) {
                        let target_pts = history
                            .iter()
                            .find(|f| Some(f.index) == displayed_index)
                            .map(|f| f.pts)
                            .unwrap_or(snapshot.position_seconds);
                        let seek = clamp_position(
                            (target_pts - 5.0).max(range.0),
                            range,
                            snapshot.duration_seconds,
                        );
                        decoder.stop.store(true, Ordering::SeqCst);
                        let _ = decoder.child.kill();
                        let _ = decoder.child.wait();
                        match start_decoder(&path, kind, seek, range.1) {
                            Ok(restarted) => {
                                decoder = restarted;
                                origin = seek;
                                frame_map.clear();
                                pts_map.clear();
                                duration_map.clear();
                                history.clear();
                                next_index = 0;
                                frame_eof = false;
                                pts_eof = false;
                                displayed_index = None;
                                requested_index = Some(u64::MAX);
                                base_pts = None;
                                eof_handled = false;
                                loop_after = None;
                            }
                            Err(error) => {
                                let _ = events.try_send(PlaybackEvent::Failed(error));
                                return;
                            }
                        }
                    }
                }
                PlaybackCommand::Stop => unreachable!(),
            }
            if let Ok(mut current) = shared.lock() {
                *current = snapshot;
            }
            if reemit_current
                && snapshot.paused
                && let Some(index) = displayed_index
                && let Some(frame) = history.iter().find(|f| f.index == index)
            {
                pending_event = Some(PlaybackEvent::Frame {
                    epoch: snapshot.epoch,
                    position_seconds: frame.pts,
                    frame_index: Some(frame.index),
                    pixels: frame.pixels.clone(),
                });
            }
        }

        if let Some(event) = pending_event.take()
            && let Err(crossbeam_channel::TrySendError::Full(event)) = events.try_send(event)
        {
            pending_event = Some(event);
        }

        // Drain a bounded lookahead when paused; channels back-pressure FFmpeg at most two
        // video/GIF frames ahead. Playback itself consumes the stream paced by source PTS.
        let can_drain = displayed_index.is_none()
            || requested_index.is_some()
            || (snapshot.paused && displayed_index.is_some() && requested_index.is_some())
            || (!snapshot.paused && next_index <= displayed_index.unwrap_or(0).saturating_add(1));
        if can_drain {
            if let Ok(message) = decoder.frame_rx.try_recv() {
                match message {
                    Some((index, pixels)) => {
                        frame_map.insert(index, pixels);
                    }
                    None => frame_eof = true,
                }
            }
            if let Ok(message) = decoder.pts_rx.try_recv() {
                match message {
                    PtsMessage::Frame(index, pts, duration) => {
                        pts_map.insert(index, pts);
                        duration_map.insert(index, duration);
                    }
                    PtsMessage::Diagnostic(line) => {
                        if diagnostics.len() == 8 {
                            diagnostics.pop_front();
                        }
                        diagnostics.push_back(line);
                    }
                    PtsMessage::Eof => pts_eof = true,
                }
            }
        }
        while frame_map.contains_key(&next_index)
            && pts_map.contains_key(&next_index)
            && duration_map.contains_key(&next_index)
        {
            let pixels = frame_map.remove(&next_index).expect("checked frame map");
            let pts = pts_map.remove(&next_index).expect("checked PTS map");
            let duration = duration_map.remove(&next_index).unwrap_or(0.1);
            let frame = Frame {
                pts: origin + pts,
                duration,
                index: next_index,
                pixels,
            };
            next_index += 1;
            if frame.pts.is_finite() && range.1.is_none_or(|end| frame.pts < end + 0.1) {
                if frame.bytes() > MAX_HISTORY_BYTES {
                    let _ = events.try_send(PlaybackEvent::Failed(
                        "Decoded frame exceeds the 64 MiB history limit.".into(),
                    ));
                    return;
                }
                while history.len() >= MAX_HISTORY_FRAMES
                    || history.iter().map(Frame::bytes).sum::<usize>() + frame.bytes()
                        > MAX_HISTORY_BYTES
                {
                    history.pop_front();
                }
                history.push_back(frame);
            }
        }

        // A distant backstep decodes from an earlier seek; select the previous real PTS,
        // never a synthetic frame interval.
        if requested_index == Some(u64::MAX)
            && let Some(last_pts) = history.back().map(|f| f.pts)
            && (last_pts >= snapshot.position_seconds || (frame_eof && pts_eof))
        {
            requested_index = history
                .iter()
                .rev()
                .find(|f| f.pts < snapshot.position_seconds)
                .map(|f| f.index)
                .or_else(|| history.front().map(|f| f.index));
        }
        let desired = if let Some(index) = requested_index {
            history.iter().find(|f| f.index == index).cloned()
        } else if displayed_index.is_none() {
            history.front().cloned()
        } else if !snapshot.paused {
            let next = displayed_index.unwrap_or(0).saturating_add(1);
            history.iter().find(|f| f.index == next).cloned()
        } else {
            None
        };
        if let Some(frame) = desired {
            let due = base_pts.is_none_or(|base| {
                Instant::now()
                    >= base_wall + Duration::from_secs_f64((frame.pts - base).clamp(0.0, 3600.0))
            });
            if requested_index.is_some() || displayed_index.is_none() || due {
                snapshot.position_seconds = frame.pts;
                snapshot.frame_index = Some(frame.index);
                pending_event = Some(PlaybackEvent::Frame {
                    epoch: snapshot.epoch,
                    position_seconds: frame.pts,
                    frame_index: Some(frame.index),
                    pixels: frame.pixels.clone(),
                });
                if let Ok(mut current) = shared.lock() {
                    *current = snapshot;
                }
                displayed_index = Some(frame.index);
                requested_index = None;
                if base_pts.is_none() {
                    base_pts = Some(frame.pts);
                    base_wall = Instant::now();
                }
            }
        }

        let status = decoder.child.try_wait().ok().flatten();
        if status.is_some() && frame_eof && pts_eof {
            if history.is_empty() {
                let detail = diagnostics.iter().cloned().collect::<Vec<_>>().join(" | ");
                let message = if detail.is_empty() {
                    format!("FFmpeg decoded no frames from {}", path.display())
                } else {
                    format!("FFmpeg could not decode {}: {detail}", path.display())
                };
                let _ = events.try_send(PlaybackEvent::Failed(message));
                return;
            }
            if status.as_ref().is_some_and(|s| !s.success()) {
                let detail = diagnostics.iter().cloned().collect::<Vec<_>>().join(" | ");
                let _ = events.try_send(PlaybackEvent::Failed(if detail.is_empty() {
                    format!("FFmpeg failed to decode {}", path.display())
                } else {
                    detail
                }));
                return;
            }
            let complete = history
                .back()
                .is_none_or(|f| displayed_index.is_some_and(|i| i >= f.index));
            if !snapshot.paused && complete && !eof_handled {
                let hold_seconds = history
                    .back()
                    .map(|frame| {
                        range
                            .1
                            .map(|end| (end - frame.pts).max(0.0))
                            .filter(|remaining| *remaining > 0.0)
                            .unwrap_or(frame.duration)
                            .clamp(0.01, 10.0)
                    })
                    .unwrap_or(0.1);
                let hold = Duration::from_secs_f64(hold_seconds);
                loop_after = Some(Instant::now() + hold);
                eof_handled = true;
            }
            if !snapshot.paused
                && complete
                && loop_after.is_some_and(|deadline| Instant::now() >= deadline)
            {
                decoder.stop.store(true, Ordering::SeqCst);
                origin = range.0;
                frame_map.clear();
                pts_map.clear();
                duration_map.clear();
                history.clear();
                next_index = 0;
                frame_eof = false;
                pts_eof = false;
                loop_after = None;
                eof_handled = false;
                displayed_index = None;
                requested_index = None;
                base_pts = None;
                match start_decoder(&path, kind, origin, range.1) {
                    Ok(next) => decoder = next,
                    Err(error) => {
                        let _ = events.try_send(PlaybackEvent::Failed(error));
                        return;
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(2));
    }
}

fn start_decoder(
    path: &Path,
    kind: MediaKind,
    seek: f64,
    end: Option<f64>,
) -> Result<Decoder, String> {
    let gif_size = if kind == MediaKind::Gif {
        Some(probe_dimensions(path).map_err(|e| format!("Could not inspect GIF dimensions: {e}"))?)
    } else {
        None
    };
    let mut command = Command::new(crate::youtube::tool_path("ffmpeg"));
    command
        .args(["-hide_banner", "-nostdin", "-loglevel", "info", "-ss"])
        .arg(format!("{seek:.6}"))
        .arg("-i")
        .arg(path);
    if let Some(end) = end {
        command
            .arg("-t")
            .arg(format!("{:.6}", (end - seek).max(0.0)));
    }
    command.arg("-vf").arg(if kind == MediaKind::Video {
        "scale='min(1280,iw)':-2,showinfo"
    } else {
        "showinfo"
    });
    command.args(["-fps_mode", "passthrough", "-an"]);
    if kind == MediaKind::Gif {
        command.args(["-pix_fmt", "rgba", "-f", "rawvideo", "pipe:1"]);
    } else {
        command.args([
            "-q:v",
            "6",
            "-f",
            "image2pipe",
            "-vcodec",
            "mjpeg",
            "pipe:1",
        ]);
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    hide_console(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| format!("Could not start FFmpeg: {e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "FFmpeg stdout unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "FFmpeg stderr unavailable".to_string())?;
    let (frame_tx, frame_rx) = bounded(2);
    let (pts_tx, pts_rx) = bounded(4);
    let stop = Arc::new(AtomicBool::new(false));
    spawn_frame_reader(stdout, frame_tx, Arc::clone(&stop), kind, gif_size);
    spawn_pts_reader(stderr, pts_tx, Arc::clone(&stop));
    Ok(Decoder {
        child,
        frame_rx,
        pts_rx,
        stop,
    })
}

fn spawn_frame_reader(
    mut stdout: std::process::ChildStdout,
    tx: Sender<Option<(u64, DecodedPixels)>>,
    stop: Arc<AtomicBool>,
    kind: MediaKind,
    gif_size: Option<(u32, u32)>,
) {
    thread::spawn(move || {
        let mut index = 0u64;
        if kind == MediaKind::Gif {
            let (width, height) = gif_size.unwrap_or((0, 0));
            let Some(size) = (width as usize)
                .checked_mul(height as usize)
                .and_then(|pixels| pixels.checked_mul(4))
            else {
                let _ = tx.send(None);
                return;
            };
            if size == 0 || size > MAX_HISTORY_BYTES {
                let _ = tx.send(None);
                return;
            }
            let mut buffer = vec![0u8; size];
            loop {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                if stdout.read_exact(&mut buffer).is_err() {
                    break;
                }
                let frame = DecodedPixels::Rgba {
                    width,
                    height,
                    bytes: std::mem::replace(&mut buffer, vec![0u8; size]),
                };
                if tx.send(Some((index, frame))).is_err() {
                    return;
                }
                index += 1;
            }
        } else {
            let mut input = [0u8; 32768];
            let mut frame = Vec::with_capacity(128 * 1024);
            let mut inside = false;
            let mut previous = 0u8;
            loop {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let count = match stdout.read(&mut input) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                for &byte in &input[..count] {
                    if !inside {
                        if previous == 0xff && byte == 0xd8 {
                            inside = true;
                            frame.clear();
                            frame.extend_from_slice(&[0xff, 0xd8]);
                        }
                    } else {
                        frame.push(byte);
                        if previous == 0xff && byte == 0xd9 {
                            if tx
                                .send(Some((
                                    index,
                                    DecodedPixels::Jpeg(std::mem::take(&mut frame)),
                                )))
                                .is_err()
                            {
                                return;
                            }
                            index += 1;
                            inside = false;
                            frame = Vec::with_capacity(128 * 1024);
                        }
                    }
                    previous = byte;
                }
            }
        }
        if !stop.load(Ordering::SeqCst) {
            let _ = tx.send(None);
        }
    });
}

fn spawn_pts_reader(
    stderr: std::process::ChildStderr,
    tx: Sender<PtsMessage>,
    stop: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if let (Some(index), Some(pts)) = (
                field_after(&line, " n:").and_then(|v| v.parse::<u64>().ok()),
                field_after(&line, " pts_time:").and_then(|v| v.parse::<f64>().ok()),
            ) {
                let duration = field_after(&line, " duration_time:")
                    .and_then(|v| v.parse::<f64>().ok())
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .unwrap_or(0.1);
                if tx.send(PtsMessage::Frame(index, pts, duration)).is_err() {
                    return;
                }
            } else if line.contains("Error") || line.contains("error") {
                let _ = tx.send(PtsMessage::Diagnostic(line));
            }
        }
        if !stop.load(Ordering::SeqCst) {
            let _ = tx.send(PtsMessage::Eof);
        }
    });
}

fn field_after<'a>(line: &'a str, field: &str) -> Option<&'a str> {
    let start = line.find(field)? + field.len();
    Some(
        line[start..]
            .split_whitespace()
            .next()?
            .trim_end_matches(','),
    )
}

pub fn probe_duration(path: &Path) -> anyhow::Result<f64> {
    let value = probe_value(
        path,
        "format=duration",
        "default=noprint_wrappers=1:nokey=1",
    )?;
    let duration: f64 = value.parse()?;
    if !duration.is_finite() || duration <= 0.0 {
        anyhow::bail!("Video duration is invalid");
    }
    Ok(duration)
}
pub fn probe_dimensions(path: &Path) -> anyhow::Result<(u32, u32)> {
    let value = probe_value(path, "stream=width,height", "csv=s=x:p=0")?;
    let (w, h) = value
        .split_once('x')
        .ok_or_else(|| anyhow::anyhow!("ffprobe returned invalid media dimensions"))?;
    let (width, height) = (w.parse::<u32>()?, h.parse::<u32>()?);
    if width == 0 || height == 0 {
        anyhow::bail!("Media dimensions must be positive");
    }
    Ok((width, height))
}
fn probe_value(path: &Path, entries: &str, format: &str) -> anyhow::Result<String> {
    let mut command = Command::new(crate::youtube::tool_path("ffprobe"));
    command
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            entries,
            "-of",
            format,
        ])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hide_console(&mut command);
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("ffprobe timed out after 5 seconds");
        }
        thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        anyhow::bail!("ffprobe could not read media");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
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
    use tempfile::TempDir;

    fn make_vfr_fixture() -> (TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("temporary VFR fixture directory");
        let path = directory.path().join("vfr-review.mp4");
        let mut command = Command::new(crate::youtube::tool_path("ffmpeg"));
        command
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=240x150:rate=10:duration=1.5",
                "-vf",
                "select='eq(n,0)+eq(n,1)+eq(n,4)+eq(n,6)+eq(n,12)'",
                "-fps_mode",
                "vfr",
                "-an",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-y",
            ])
            .arg(&path)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        hide_console(&mut command);
        let output = command.output().expect("run FFmpeg VFR fixture generator");
        assert!(
            output.status.success(),
            "VFR fixture generation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        (directory, path)
    }
    #[test]
    fn converts_percent_range_to_seconds() {
        assert_eq!(
            selected_range(Some(120.0), &[[20.0, 35.0]], 0),
            (24.0, Some(42.0))
        );
    }
    #[test]
    fn clamps_out_of_range_seek_to_loop_start() {
        assert_eq!(clamp_position(42.0, (24.0, Some(42.0)), Some(120.0)), 24.0);
    }
    fn next_frame_at_epoch(session: &PlaybackSession, epoch: u64, after: f64) -> f64 {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match session.events.recv_timeout(Duration::from_millis(200)) {
                Ok(PlaybackEvent::Frame {
                    epoch: got,
                    position_seconds,
                    ..
                }) if got == epoch && position_seconds > after + 0.001 => return position_seconds,
                Ok(_) | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("frame event stream closed: {error}"),
            }
        }
        panic!("timed out waiting for next real frame after {after}")
    }

    fn frame_near_at_epoch(session: &PlaybackSession, epoch: u64, target: f64) -> DecodedPixels {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match session.events.recv_timeout(Duration::from_millis(200)) {
                Ok(PlaybackEvent::Frame {
                    epoch: got,
                    position_seconds,
                    pixels,
                    ..
                }) if got == epoch && (position_seconds - target).abs() < 0.03 => return pixels,
                Ok(_) | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("frame event stream closed: {error}"),
            }
        }
        panic!("timed out waiting for source frame at {target}")
    }

    #[test]
    #[ignore = "requires the FFmpeg sidecar"]
    fn paused_vfr_sessions_step_by_source_pts_and_are_independent() {
        let (_directory, path) = make_vfr_fixture();
        let state = PlaybackState {
            paused: true,
            position_seconds: 0.0,
        };
        let first =
            PlaybackSession::start(path.clone(), MediaKind::Video, Some(1.3), vec![], 0, state);
        let second = PlaybackSession::start(path, MediaKind::Video, Some(1.3), vec![], 0, state);
        let initial = first
            .events
            .recv_timeout(Duration::from_secs(10))
            .expect("initial paused frame");
        let initial_time = match initial {
            PlaybackEvent::Frame {
                position_seconds, ..
            } => position_seconds,
            PlaybackEvent::Failed(e) => panic!("decode failed: {e}"),
        };
        assert!(
            initial_time.abs() < 0.02,
            "unexpected first source PTS {initial_time}"
        );
        let second_initial = second
            .events
            .recv_timeout(Duration::from_secs(10))
            .expect("second session initial frame");
        assert!(matches!(second_initial, PlaybackEvent::Frame { .. }));

        let expected = [0.1, 0.4, 0.6, 1.2];
        let mut current = initial_time;
        for wanted in expected {
            let epoch = first.snapshot().epoch + 1;
            first.command(PlaybackCommand::StepForward);
            current = next_frame_at_epoch(&first, epoch, current);
            assert!(
                (current - wanted).abs() < 0.03,
                "wanted source PTS {wanted}, got {current}"
            );
        }
        assert_eq!(
            second.snapshot().frame_index,
            Some(0),
            "commands for one session must not affect another session using the same file"
        );
        let epoch = first.snapshot().epoch + 1;
        first.command(PlaybackCommand::StepBackward);
        let _ = frame_near_at_epoch(&first, epoch, 0.6);
    }

    #[test]
    #[ignore = "requires the FFmpeg sidecar"]
    fn unpaused_decoder_loops_only_after_normal_eof() {
        let (_directory, path) = make_vfr_fixture();
        let session = PlaybackSession::start(
            path,
            MediaKind::Video,
            Some(1.3),
            vec![],
            0,
            PlaybackState::default(),
        );
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut saw_end = false;
        let mut saw_restart = false;
        while Instant::now() < deadline && !saw_restart {
            if let Ok(PlaybackEvent::Frame {
                position_seconds, ..
            }) = session.events.recv_timeout(Duration::from_millis(250))
            {
                saw_end |= position_seconds > 1.1;
                saw_restart |= saw_end && position_seconds < 0.03;
            }
        }
        assert!(
            saw_end && saw_restart,
            "normal EOF should retain final frame duration then loop"
        );
    }

    #[test]
    #[ignore = "requires the FFmpeg sidecar"]
    fn pause_holds_the_last_presented_frame_and_snapshot() {
        let (_directory, path) = make_vfr_fixture();
        let session = PlaybackSession::start(
            path,
            MediaKind::Video,
            Some(1.3),
            vec![],
            0,
            PlaybackState::default(),
        );
        let first = session
            .events
            .recv_timeout(Duration::from_secs(10))
            .expect("first playing frame");
        let first_time = match first {
            PlaybackEvent::Frame {
                position_seconds, ..
            } => position_seconds,
            PlaybackEvent::Failed(error) => panic!("decoder failed: {error}"),
        };
        assert!(first_time.is_finite());
        let pause_epoch = session.snapshot().epoch + 1;
        session.command(PlaybackCommand::Pause);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !session.snapshot().paused {
            thread::sleep(Duration::from_millis(5));
        }
        let paused_time = session.snapshot().position_seconds;
        let _ = frame_near_at_epoch(&session, pause_epoch, paused_time);
        thread::sleep(Duration::from_millis(500));
        let snapshot = session.snapshot();
        assert!(snapshot.paused);
        assert_eq!(snapshot.position_seconds, paused_time);
    }

    #[test]
    #[ignore = "requires the FFmpeg sidecar"]
    fn dropping_live_session_joins_worker_and_reaps_decoder() {
        let (_directory, path) = make_vfr_fixture();
        let session = PlaybackSession::start(
            path,
            MediaKind::Video,
            Some(1.3),
            vec![],
            0,
            PlaybackState::default(),
        );
        let event = session
            .events
            .recv_timeout(Duration::from_secs(10))
            .expect("decoder should start before the session is dropped");
        assert!(matches!(event, PlaybackEvent::Frame { .. }));

        let started = Instant::now();
        drop(session);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "dropping a live session should stop and join its decoder promptly"
        );
    }

    #[test]
    #[ignore = "requires the FFmpeg sidecar"]
    fn gif_session_preserves_alpha_and_uses_real_frame_delay() {
        let directory = tempfile::tempdir().expect("temporary GIF directory");
        let path = directory.path().join("timed-transparent.gif");
        let mut first = image::RgbaImage::from_pixel(8, 8, image::Rgba([255, 0, 0, 255]));
        first.put_pixel(0, 0, image::Rgba([0, 0, 0, 0]));
        let mut second = image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 255, 0, 255]));
        second.put_pixel(0, 0, image::Rgba([0, 0, 0, 0]));
        let frames = vec![
            image::Frame::from_parts(first, 0, 0, image::Delay::from_numer_denom_ms(100, 1)),
            image::Frame::from_parts(second, 0, 0, image::Delay::from_numer_denom_ms(350, 1)),
        ];
        image::codecs::gif::GifEncoder::new(std::fs::File::create(&path).unwrap())
            .encode_frames(frames)
            .expect("write variable-delay transparent GIF");
        let session = PlaybackSession::start(
            path,
            MediaKind::Gif,
            None,
            vec![],
            0,
            PlaybackState {
                paused: true,
                position_seconds: 0.0,
            },
        );
        let first_event = session
            .events
            .recv_timeout(Duration::from_secs(10))
            .expect("initial GIF frame");
        let (first_pts, first_pixels) = match first_event {
            PlaybackEvent::Frame {
                position_seconds,
                pixels,
                ..
            } => (position_seconds, pixels),
            PlaybackEvent::Failed(error) => panic!("GIF decode failed: {error}"),
        };
        assert!(first_pts.abs() < 0.02);
        match first_pixels {
            DecodedPixels::Rgba {
                width,
                height,
                bytes,
            } => {
                assert_eq!((width, height), (8, 8));
                assert_eq!(bytes[3], 0, "transparent GIF pixel must retain alpha");
            }
            DecodedPixels::Jpeg(_) => panic!("GIF session must emit alpha-capable RGBA"),
        }
        let epoch = session.snapshot().epoch + 1;
        session.command(PlaybackCommand::StepForward);
        let second_pts = next_frame_at_epoch(&session, epoch, first_pts);
        assert!(
            (second_pts - 0.1).abs() < 0.03,
            "100 ms delay should set next PTS to 0.1s, got {second_pts}"
        );
    }
}
