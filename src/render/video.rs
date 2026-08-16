// SPDX-License-Identifier: GPL-3.0-or-later
//
// Looping video background decoded through an external `ffmpeg` CLI pipe.
//
// ffmpeg does the work that would otherwise need a native codec stack:
// looping (`-stream_loop -1`), no audio (`-an`), rate capping (`-r`), and
// fit (`scale` + `crop`/`pad`), emitting raw RGB24 frames on stdout at the
// target canvas size. A dedicated decode thread keeps the most recent frame
// in shared state so the render path stays non-blocking: the video advances
// at its capped fps while the LCD is updated at the daemon's tick rate.
//
// The module has no build-time dependencies; it only requires an `ffmpeg`
// binary at runtime when a video background is actually configured.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tiny_skia::Pixmap;

use crate::render::RawFrame;

/// How a video frame is fitted to the canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoFit {
    /// Scale to cover the canvas, cropping the overflow. Default — matches
    /// the static image background behavior.
    #[default]
    Cover,
    /// Scale to fit inside the canvas, letterboxing the remainder in black.
    Contain,
}

impl VideoFit {
    /// Parse a config value (`"cover"` or `"contain"`).
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "cover" => Ok(VideoFit::Cover),
            "contain" => Ok(VideoFit::Contain),
            other => bail!("video background fit '{other}' must be 'cover' or 'contain'"),
        }
    }

    /// The `-vf` filter chain that fits source video to `width`x`height`.
    pub fn filter(self, width: u32, height: u32) -> String {
        match self {
            VideoFit::Cover => format!(
                "scale={width}:{height}:force_original_aspect_ratio=increase:flags=bilinear,crop={width}:{height}"
            ),
            VideoFit::Contain => format!(
                "scale={width}:{height}:force_original_aspect_ratio=decrease:flags=bilinear,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=black"
            ),
        }
    }
}

struct Shared {
    latest: Option<RawFrame>,
    /// Number of frames decoded so far (monotonic; used by diagnostics and
    /// tests to observe progress across respawns).
    frame_count: u64,
    /// The live ffmpeg child, if any. `stop()` kills through this handle to
    /// unblock a read that is stuck on the pipe.
    child: Option<Child>,
    /// Consecutive spawn/decode failures; bounded so a bad file cannot spin.
    failures: u32,
}

const MAX_CONSECUTIVE_FAILURES: u32 = 5;
const RESPAWN_BACKOFF: Duration = Duration::from_millis(500);
const STOP_POLL_STEP: Duration = Duration::from_millis(50);

/// A looping, muted video background at a fixed canvas size.
///
/// Owns a decode thread that supervises the ffmpeg child (bounded respawn
/// on unexpected exit). Dropping the background stops playback.
pub struct VideoBackground {
    path: PathBuf,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl VideoBackground {
    /// Start decoding `path` at up to `fps` frames per second, fitted to a
    /// `width`x`height` canvas.
    ///
    /// Fails fast with a clear error when the file is missing, the fps is
    /// out of range, or `ffmpeg` is not on `PATH`. Playback itself is
    /// supervised: if the child exits mid-stream it is respawned until
    /// `MAX_CONSECUTIVE_FAILURES` consecutive failures occur.
    pub fn start(
        path: impl AsRef<Path>,
        fps: u32,
        fit: VideoFit,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if !path.is_file() {
            bail!("video background file not found: {}", path.display());
        }
        if fps == 0 || fps > 60 {
            bail!("video background fps {fps} out of range [1, 60]");
        }
        if which_ffmpeg().is_none() {
            bail!(
                "ffmpeg not found on PATH; install it to use video backgrounds \
                 (the `video` feature shells out to the ffmpeg CLI)"
            );
        }

        let shared = Arc::new(Mutex::new(Shared {
            latest: None,
            frame_count: 0,
            child: None,
            failures: 0,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let label = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "video".to_owned());
        let handle = thread::Builder::new()
            .name(format!("thermalwriter-video-{label}"))
            .spawn({
                let shared = Arc::clone(&shared);
                let stop = Arc::clone(&stop);
                let path = path.clone();
                move || decode_loop(&path, fps, fit, width, height, shared, stop)
            })
            .context("failed to spawn video decode thread")?;

        Ok(Self {
            path,
            shared,
            stop,
            handle: Some(handle),
        })
    }

    /// Path of the file being played (for logging and diagnostics).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The most recently decoded frame, if any. Non-blocking: returns the
    /// current frame or `None` while the decoder warms up / has given up.
    pub fn latest_frame(&self) -> Option<RawFrame> {
        let shared = self.shared.lock().ok()?;
        shared.latest.clone()
    }
    /// Number of frames decoded so far. Monotonically increasing while
    /// playback is healthy (across respawns).
    pub fn frame_count(&self) -> u64 {
        self.shared.lock().map(|s| s.frame_count).unwrap_or(0)
    }

    /// Stop playback: signal the decode thread, kill the child (unblocking
    /// any in-flight read), and join the thread. Idempotent.
    pub fn stop(&mut self) {
        if self.handle.is_none() {
            return;
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(mut child) = self.shared.lock().ok().and_then(|mut s| s.child.take()) {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for VideoBackground {
    fn drop(&mut self) {
        self.stop();
    }
}

fn decode_loop(
    path: &Path,
    fps: u32,
    fit: VideoFit,
    width: u32,
    height: u32,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
) {
    let frame_bytes = (width as usize) * (height as usize) * 3;
    let filter = fit.filter(width, height);

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        let mut child = match spawn_ffmpeg(path, fps, &filter) {
            Ok(child) => child,
            Err(e) => {
                let failed = {
                    let mut s = shared.lock().expect("video shared lock poisoned");
                    s.failures += 1;
                    s.failures >= MAX_CONSECUTIVE_FAILURES
                };
                if failed {
                    log::error!(
                        "video background '{}': giving up after {MAX_CONSECUTIVE_FAILURES} consecutive failures: {e}",
                        path.display()
                    );
                    break;
                }
                log::warn!(
                    "video background '{}': spawn failed: {e} (retrying)",
                    path.display()
                );
                sleep_respecting_stop(RESPAWN_BACKOFF, &stop);
                continue;
            }
        };

        let mut stdout = child.stdout.take().expect("stdout is piped");
        {
            let mut s = shared.lock().expect("video shared lock poisoned");
            s.child = Some(child);
            s.failures = 0;
        }

        let mut buf = vec![0u8; frame_bytes];
        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            match stdout.read_exact(&mut buf) {
                Ok(()) => {
                    let frame = RawFrame {
                        data: buf.clone(),
                        width,
                        height,
                    };
                    let mut s = shared.lock().expect("video shared lock poisoned");
                    s.latest = Some(frame);
                    s.frame_count += 1;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                // The pipe writer closed: the child exited. With
                // `-stream_loop -1` that means a real decode failure —
                // fall through and respawn (bounded).
                Err(_) => break,
            }
        }

        // Reap the child. Exactly one of this cleanup or `stop()` takes the
        // handle from shared state, so the process is waited on once.
        if let Some(mut child) = shared
            .lock()
            .expect("video shared lock poisoned")
            .child
            .take()
        {
            if stop.load(Ordering::Relaxed) {
                let _ = child.kill();
            }
            let _ = child.wait();
        }

        if stop.load(Ordering::Relaxed) {
            break;
        }
        log::info!(
            "video background '{}' child exited; respawning in {:?}",
            path.display(),
            RESPAWN_BACKOFF
        );
        sleep_respecting_stop(RESPAWN_BACKOFF, &stop);
    }

    // Final sweep in case stop() lost the race for the child handle.
    if let Some(mut child) = shared
        .lock()
        .expect("video shared lock poisoned")
        .child
        .take()
    {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn spawn_ffmpeg(path: &Path, fps: u32, filter: &str) -> Result<Child> {
    Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-stream_loop",
            "-1",
            "-i",
        ])
        .arg(path)
        .args([
            "-an",
            "-vf",
            filter,
            "-r",
            &fps.to_string(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn ffmpeg for {}", path.display()))
}

/// Probe `PATH` for an `ffmpeg` binary. Cheap; used only to fail fast with a
/// clear message instead of a bare `NotFound` from `spawn`.
fn which_ffmpeg() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("ffmpeg"))
            .find(|candidate| candidate.is_file())
    })
}

fn sleep_respecting_stop(duration: Duration, stop: &AtomicBool) {
    let mut remaining = duration;
    while remaining > Duration::ZERO {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let step = remaining.min(STOP_POLL_STEP);
        thread::sleep(step);
        remaining = remaining.saturating_sub(step);
    }
}

/// Convert a straight-RGB frame to an opaque tiny-skia pixmap.
///
/// Video frames are fully opaque (alpha 255), so premultiplied storage
/// equals straight storage — a plain channel copy.
pub fn rgb_to_pixmap(frame: &RawFrame) -> Result<Pixmap> {
    let expected = (frame.width as usize) * (frame.height as usize) * 3;
    if frame.data.len() != expected {
        bail!(
            "video frame data length {} does not match {}x{} RGB24 (expected {expected})",
            frame.data.len(),
            frame.width,
            frame.height
        );
    }
    let mut pixmap = Pixmap::new(frame.width, frame.height).ok_or_else(|| {
        anyhow::anyhow!(
            "invalid video frame dimensions {}x{}",
            frame.width,
            frame.height
        )
    })?;
    let data = pixmap.data_mut();
    for (dst, src) in data.chunks_exact_mut(4).zip(frame.data.chunks_exact(3)) {
        dst[0] = src[0];
        dst[1] = src[1];
        dst[2] = src[2];
        dst[3] = 255;
    }
    Ok(pixmap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn ffmpeg_available() -> bool {
        which_ffmpeg().is_some()
    }

    #[test]
    fn fit_parse_accepts_known_values() {
        assert_eq!(VideoFit::parse("cover").unwrap(), VideoFit::Cover);
        assert_eq!(VideoFit::parse("COVER").unwrap(), VideoFit::Cover);
        assert_eq!(VideoFit::parse(" contain ").unwrap(), VideoFit::Contain);
        assert!(VideoFit::parse("stretch").is_err());
        assert!(VideoFit::parse("").is_err());
    }

    #[test]
    fn fit_filter_targets_canvas_for_both_modes() {
        let cover = VideoFit::Cover.filter(480, 480);
        assert!(cover.contains("force_original_aspect_ratio=increase"));
        assert!(cover.contains("crop=480:480"));

        let contain = VideoFit::Contain.filter(480, 480);
        assert!(contain.contains("force_original_aspect_ratio=decrease"));
        assert!(contain.contains("pad=480:480"));
    }

    #[test]
    fn rgb_to_pixmap_is_opaque_and_lossless() {
        let frame = RawFrame {
            data: vec![1, 2, 3, 4, 5, 6, 250, 251, 252],
            width: 3,
            height: 1,
        };
        let pixmap = rgb_to_pixmap(&frame).unwrap();
        assert_eq!(pixmap.width(), 3);
        assert_eq!(pixmap.height(), 1);
        let data = pixmap.data();
        assert_eq!(data, &[1, 2, 3, 255, 4, 5, 6, 255, 250, 251, 252, 255]);
    }

    #[test]
    fn rgb_to_pixmap_rejects_mismatched_length() {
        let frame = RawFrame {
            data: vec![1, 2, 3],
            width: 2,
            height: 1,
        };
        assert!(rgb_to_pixmap(&frame).is_err());
    }

    fn write_test_video(path: &Path) -> bool {
        // 2s, 10fps, 160x120 gradient — small and fast to decode.
        Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "gradients=size=160x120:rate=10:duration=2",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-y",
            ])
            .arg(path)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[test]
    fn spawn_requires_existing_file() {
        assert!(
            VideoBackground::start("/nonexistent/video.mp4", 15, VideoFit::Cover, 480, 480)
                .is_err()
        );
    }

    #[test]
    fn spawn_rejects_out_of_range_fps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.mp4");
        std::fs::write(&path, b"not a video").unwrap();
        assert!(VideoBackground::start(&path, 0, VideoFit::Cover, 480, 480).is_err());
        assert!(VideoBackground::start(&path, 61, VideoFit::Cover, 480, 480).is_err());
    }

    #[test]
    fn decodes_frames_and_stops_cleanly() {
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.mp4");
        assert!(write_test_video(&path), "could not generate test video");

        let mut video = VideoBackground::start(&path, 10, VideoFit::Contain, 160, 120).unwrap();

        // The decode thread produces frames at ~10fps; wait up to 5s for
        // at least two decoded frames (playback is advancing).
        let deadline = Instant::now() + Duration::from_secs(5);
        while video.frame_count() < 2 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        let first = video.latest_frame().expect("no frame decoded within 5s");
        assert!(video.frame_count() >= 2, "playback did not advance");
        assert_eq!((first.width, first.height), (160, 120));
        assert_eq!(first.data.len(), 160 * 120 * 3);

        // Stop must return promptly even while the child is alive.
        let started = Instant::now();
        video.stop();
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "stop() took too long: {:?}",
            started.elapsed()
        );
        // Idempotent.
        video.stop();
        drop(video);
    }

    #[test]
    fn respawn_recovers_from_mid_stream_exit() {
        if !ffmpeg_available() {
            eprintln!("skipping: ffmpeg not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.mp4");
        assert!(write_test_video(&path), "could not generate test video");

        let video = VideoBackground::start(&path, 10, VideoFit::Cover, 160, 120).unwrap();
        // Wait for the first frame (confirms the initial child is producing).
        let deadline = Instant::now() + Duration::from_secs(5);
        while video.frame_count() == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(video.frame_count() > 0);
        let count_before_kill = video.frame_count();

        // Kill the child mid-stream; supervision should respawn it and
        // resume producing frames.
        let child = video
            .shared
            .lock()
            .expect("shared lock poisoned")
            .child
            .as_ref()
            .expect("expected a live child")
            .id();
        // SAFETY: plain Unix signal to our own spawned child.
        unsafe {
            libc::kill(child as i32, libc::SIGKILL);
        }

        // Frame production must resume after the backoff window.
        let deadline = Instant::now() + Duration::from_secs(8);
        while video.frame_count() <= count_before_kill && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(
            video.frame_count() > count_before_kill,
            "frames did not resume after the child was killed"
        );
        drop(video);
    }
}
