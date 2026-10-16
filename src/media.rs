use crossbeam_channel::{Receiver, Sender, bounded};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum PlayerEvent {
    Frame(Vec<u8>),
    Failed(String),
}

pub struct VideoPlayer {
    pub events: Receiver<PlayerEvent>,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    position: Arc<AtomicU64>,
}

struct PlayerConfig {
    path: PathBuf,
    duration: Option<f64>,
    pairs: Vec<[f64; 2]>,
    active_pair: usize,
    seek_to: Option<f64>,
}

struct PlayerRuntime {
    events: Sender<PlayerEvent>,
    stop: Arc<AtomicBool>,
    child_slot: Arc<Mutex<Option<Child>>>,
    position: Arc<AtomicU64>,
}

impl VideoPlayer {
    pub fn start_at(
        path: PathBuf,
        duration: Option<f64>,
        pairs: Vec<[f64; 2]>,
        active_pair: usize,
        seek_to: Option<f64>,
    ) -> Self {
        let (tx, events) = bounded(2);
        let stop = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        let (start, end) = selected_range(duration, &pairs, active_pair);
        let initial_position = seek_to
            .filter(|s| *s >= start && end.is_none_or(|e| *s < e))
            .unwrap_or(start);
        let position = Arc::new(AtomicU64::new(initial_position.to_bits()));
        let worker_stop = Arc::clone(&stop);
        let worker_child = Arc::clone(&child);
        let worker_position = Arc::clone(&position);
        let config = PlayerConfig {
            path,
            duration,
            pairs,
            active_pair,
            seek_to,
        };
        let runtime = PlayerRuntime {
            events: tx,
            stop: worker_stop,
            child_slot: worker_child,
            position: worker_position,
        };
        thread::spawn(move || run_player(config, runtime));
        Self {
            events,
            stop,
            child,
            position,
        }
    }

    pub fn playhead(&self) -> f64 {
        f64::from_bits(self.position.load(Ordering::Relaxed))
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        kill_child(&self.child);
    }
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_player(config: PlayerConfig, runtime: PlayerRuntime) {
    let PlayerConfig {
        path,
        duration,
        pairs,
        active_pair,
        seek_to,
    } = config;
    let PlayerRuntime {
        events: tx,
        stop,
        child_slot,
        position,
    } = runtime;
    let (start, end) = selected_range(duration, &pairs, active_pair);
    if end.is_some_and(|end| end <= start) {
        let _ = tx.try_send(PlayerEvent::Failed("Video trim range is invalid.".into()));
        return;
    }
    let mut first_iteration = true;
    while !stop.load(Ordering::SeqCst) {
        let playback_start = if first_iteration {
            seek_to
                .filter(|s| *s >= start && end.is_none_or(|e| *s < e))
                .unwrap_or(start)
        } else {
            start
        };
        position.store(playback_start.to_bits(), Ordering::Relaxed);
        first_iteration = false;
        let mut command = Command::new(crate::youtube::tool_path("ffmpeg"));
        command
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-ss"])
            .arg(format!("{playback_start:.3}"))
            .arg("-i")
            .arg(&path);
        if let Some(end) = end {
            command
                .arg("-t")
                .arg(format!("{:.3}", end - playback_start));
        }
        command
            .args([
                "-vf",
                "fps=24,scale='min(1280,iw)':-2",
                "-an",
                "-q:v",
                "6",
                "-f",
                "image2pipe",
                "-vcodec",
                "mjpeg",
                "pipe:1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        hide_console(&mut command);
        let mut process = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = tx.send(PlayerEvent::Failed(format!(
                    "Could not start FFmpeg: {error}"
                )));
                return;
            }
        };
        let stdout = process.stdout.take();
        if let Ok(mut slot) = child_slot.lock() {
            *slot = Some(process);
        }
        let Some(stdout) = stdout else {
            return;
        };
        let mut reader = stdout;
        let mut input = [0_u8; 32 * 1024];
        let mut frame = Vec::with_capacity(128 * 1024);
        let mut in_frame = false;
        let mut previous = 0_u8;
        let mut frame_index = 0_u64;
        let mut first_frame_at = None;
        loop {
            if stop.load(Ordering::SeqCst) {
                kill_and_reap(&child_slot);
                return;
            }
            let count = match reader.read(&mut input) {
                Ok(0) => break,
                Ok(n) => n,
                Err(error) => {
                    if !stop.load(Ordering::SeqCst) {
                        let _ = tx.send(PlayerEvent::Failed(format!(
                            "FFmpeg frame pipe failed: {error}"
                        )));
                    }
                    break;
                }
            };
            for &byte in &input[..count] {
                if !in_frame {
                    if previous == 0xff && byte == 0xd8 {
                        frame.clear();
                        frame.extend_from_slice(&[0xff, 0xd8]);
                        in_frame = true;
                    }
                } else {
                    frame.push(byte);
                    if previous == 0xff && byte == 0xd9 {
                        let frame_clock = *first_frame_at.get_or_insert_with(Instant::now);
                        let scheduled_at =
                            frame_clock + Duration::from_secs_f64(frame_index as f64 / 24.0);
                        if !wait_until(scheduled_at, &stop) {
                            kill_and_reap(&child_slot);
                            return;
                        }
                        let frame_position = playback_start + frame_index as f64 / 24.0;
                        frame_index += 1;
                        let outgoing = std::mem::take(&mut frame);
                        match tx.try_send(PlayerEvent::Frame(outgoing)) {
                            Ok(()) => position.store(frame_position.to_bits(), Ordering::Relaxed),
                            Err(crossbeam_channel::TrySendError::Full(_)) => (),
                            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                                kill_and_reap(&child_slot);
                                return;
                            }
                        }
                        frame = Vec::with_capacity(128 * 1024);
                        in_frame = false;
                    }
                }
                previous = byte;
            }
        }
        let status = child_slot.lock().ok().and_then(|mut slot| {
            let result = slot.as_mut().and_then(|p| p.wait().ok());
            *slot = None;
            result
        });
        if stop.load(Ordering::SeqCst) {
            return;
        }
        if status.is_some_and(|s| !s.success()) {
            let _ = tx.send(PlayerEvent::Failed(format!(
                "FFmpeg could not decode {}.",
                path.display()
            )));
            return;
        }
    }
}

fn wait_until(deadline: Instant, stop: &AtomicBool) -> bool {
    loop {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return true;
        }
        thread::sleep(remaining.min(Duration::from_millis(5)));
    }
}

fn selected_range(duration: Option<f64>, pairs: &[[f64; 2]], active: usize) -> (f64, Option<f64>) {
    let Some(duration) = duration.filter(|d| d.is_finite() && *d > 0.0) else {
        return (0.0, None);
    };
    let pair = pairs.get(active).copied().unwrap_or([0.0, 100.0]);
    (
        (pair[0].clamp(0.0, 100.0) * duration / 100.0),
        Some(pair[1].clamp(0.0, 100.0) * duration / 100.0),
    )
}

fn kill_child(child: &Arc<Mutex<Option<Child>>>) {
    if let Ok(mut slot) = child.lock()
        && let Some(process) = slot.as_mut()
    {
        let _ = process.kill();
    }
}

fn kill_and_reap(child: &Arc<Mutex<Option<Child>>>) {
    if let Ok(mut slot) = child.lock()
        && let Some(mut process) = slot.take()
    {
        let _ = process.kill();
        let _ = process.wait();
    }
}

pub fn probe_duration(path: &Path) -> anyhow::Result<f64> {
    let mut command = Command::new(crate::youtube::tool_path("ffprobe"));
    command
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hide_console(&mut command);
    let output = command.output()?;
    if !output.status.success() {
        anyhow::bail!("ffprobe could not read this video");
    }
    let duration: f64 = String::from_utf8_lossy(&output.stdout).trim().parse()?;
    if !duration.is_finite() || duration <= 0.0 {
        anyhow::bail!("Video duration is invalid");
    }
    Ok(duration)
}

pub fn probe_dimensions(path: &Path) -> anyhow::Result<(u32, u32)> {
    let mut command = Command::new(crate::youtube::tool_path("ffprobe"));
    command
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=s=x:p=0",
        ])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    hide_console(&mut command);
    let output = command.output()?;
    if !output.status.success() {
        anyhow::bail!("ffprobe could not read video dimensions");
    }
    let dimensions = String::from_utf8_lossy(&output.stdout);
    let (width, height) = dimensions
        .trim()
        .split_once('x')
        .ok_or_else(|| anyhow::anyhow!("ffprobe returned invalid video dimensions"))?;
    let width = width.parse::<u32>()?;
    let height = height.parse::<u32>()?;
    if width == 0 || height == 0 {
        anyhow::bail!("Video dimensions must be greater than zero");
    }
    Ok((width, height))
}

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000000);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    #[test]
    fn converts_percent_trim_to_seconds() {
        assert_eq!(
            selected_range(Some(120.0), &[[20.0, 35.0]], 0),
            (24.0, Some(42.0))
        );
    }
    #[test]
    fn invalid_duration_falls_back_to_whole_stream() {
        assert_eq!(selected_range(None, &[], 0), (0.0, None));
    }

    #[test]
    #[ignore = "requires the FFmpeg sidecar; run explicitly for live decoder timing acceptance"]
    fn real_decoder_paces_a_one_second_clip_at_24fps() {
        let directory = tempfile::tempdir().expect("create temporary fixture directory");
        let video = directory.path().join("four-seconds.mp4");
        let mut command = Command::new(crate::youtube::tool_path("ffmpeg"));
        command
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=160x90:rate=24:duration=4",
                "-c:v",
                "mpeg4",
                "-q:v",
                "4",
                "-y",
            ])
            .arg(&video)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        hide_console(&mut command);
        let generated = command.output().expect("run FFmpeg fixture generator");
        assert!(
            generated.status.success(),
            "fixture generation failed: {}",
            String::from_utf8_lossy(&generated.stderr)
        );

        let player = VideoPlayer::start_at(video, Some(4.0), vec![[0.0, 25.0]], 0, None);
        let mut received_at = Vec::with_capacity(24);
        for _ in 0..24 {
            match player
                .events
                .recv_timeout(Duration::from_secs(3))
                .expect("receive paced decoder frame")
            {
                PlayerEvent::Frame(_) => received_at.push(Instant::now()),
                PlayerEvent::Failed(error) => panic!("decoder failed: {error}"),
            }
        }
        let elapsed = received_at[23].duration_since(received_at[0]);
        assert!(
            (Duration::from_millis(800)..Duration::from_millis(1400)).contains(&elapsed),
            "24 frames for a one-second loop arrived in {elapsed:?}"
        );
        let playhead = player.playhead();
        assert!(
            (0.90..1.0).contains(&playhead),
            "playhead should follow the sent decoder frame, got {playhead}"
        );
        player.stop();
    }
}
