//! Animation frame manager: loads GIF files and serves per-frame RGBA pixel data
//! and base64-encoded PNG strings for SVG <image> embedding.

use std::io::Cursor;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use image::{DynamicImage, Frame, ImageFormat, codecs::gif::GifDecoder};
use image::AnimationDecoder;

const DEFAULT_FRAME_DELAY_MS: u64 = 100;

/// A single decoded animation frame.
struct AnimFrame {
    /// Per-frame delay.
    delay: Duration,
    /// RGBA8 pixel data (width * height * 4 bytes).
    pixels: Vec<u8>,
    /// Width and height.
    width: u32,
    height: u32,
}

/// Eagerly-loaded animation: all frames decoded and held in memory.
/// Provides `frame_at(elapsed)` for time-based lookup with automatic looping.
pub struct AnimationSource {
    frames: Vec<AnimFrame>,
    total_duration: Duration,
}

impl AnimationSource {
    /// Load a GIF file from disk, decode all frames eagerly.
    pub fn load(path: &Path) -> Result<Self> {
        let data = std::fs::read(path)
            .with_context(|| format!("Failed to read animation file: {}", path.display()))?;

        let decoder = GifDecoder::new(Cursor::new(&data))
            .context("Failed to decode GIF")?;

        let frames_iter = decoder.into_frames();
        let mut frames: Vec<AnimFrame> = Vec::new();

        for frame_result in frames_iter {
            let frame: Frame = frame_result.context("Failed to decode GIF frame")?;
            let delay_ms = {
                let (numer, denom) = frame.delay().numer_denom_ms();
                let ms = if denom > 0 { numer as u64 / denom as u64 } else { DEFAULT_FRAME_DELAY_MS };
                if ms == 0 { DEFAULT_FRAME_DELAY_MS } else { ms }
            };
            let img = DynamicImage::ImageRgba8(frame.into_buffer());
            let width = img.width();
            let height = img.height();
            let pixels = img.into_rgba8().into_raw();
            frames.push(AnimFrame {
                delay: Duration::from_millis(delay_ms),
                pixels,
                width,
                height,
            });
        }

        anyhow::ensure!(!frames.is_empty(), "GIF has no frames");

        let total_duration = frames.iter().map(|f| f.delay).sum();
        Ok(Self { frames, total_duration })
    }

    /// Number of frames in the animation.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Total animation duration (sum of all frame delays).
    pub fn total_duration(&self) -> Duration {
        self.total_duration
    }

    /// Native playback rate in frames per second (from average frame delay).
    pub fn native_fps(&self) -> f64 {
        if self.frames.is_empty() {
            return 0.0;
        }
        let avg_delay_secs = self.total_duration.as_secs_f64() / self.frames.len() as f64;
        if avg_delay_secs > 0.0 { 1.0 / avg_delay_secs } else { 10.0 }
    }

    /// Return the RGBA pixel data for the frame at `elapsed` time (loops automatically).
    /// Returns `None` only if there are no frames.
    pub fn frame_at(&self, elapsed: Duration) -> Option<&[u8]> {
        if self.frames.is_empty() {
            return None;
        }
        // Wrap elapsed into [0, total_duration)
        let total_ns = self.total_duration.as_nanos();
        let elapsed_ns = if total_ns > 0 {
            elapsed.as_nanos() % total_ns
        } else {
            0
        };

        let mut accum_ns: u128 = 0;
        for frame in &self.frames {
            accum_ns += frame.delay.as_nanos();
            if elapsed_ns < accum_ns {
                return Some(&frame.pixels);
            }
        }
        // Fallback: last frame
        Some(&self.frames.last().unwrap().pixels)
    }

    /// Return the frame at `elapsed` as a base64-encoded PNG string.
    /// Suitable for embedding in SVG: `<image href="data:image/png;base64,{result}">`.
    pub fn base64_frame_at(&self, elapsed: Duration) -> Option<String> {
        let frame = self.frames.get(self.frame_index_at(elapsed)?)?;
        // Encode RGBA pixels to PNG
        let img = image::RgbaImage::from_raw(frame.width, frame.height, frame.pixels.clone())?;
        let mut png_bytes: Vec<u8> = Vec::new();
        DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut png_bytes), ImageFormat::Png)
            .ok()?;
        Some(BASE64.encode(&png_bytes))
    }

    /// Return the 0-indexed frame index for the given elapsed time.
    fn frame_index_at(&self, elapsed: Duration) -> Option<usize> {
        if self.frames.is_empty() {
            return None;
        }
        let total_ns = self.total_duration.as_nanos();
        let elapsed_ns = if total_ns > 0 {
            elapsed.as_nanos() % total_ns
        } else {
            0
        };

        let mut accum_ns: u128 = 0;
        for (i, frame) in self.frames.iter().enumerate() {
            accum_ns += frame.delay.as_nanos();
            if elapsed_ns < accum_ns {
                return Some(i);
            }
        }
        Some(self.frames.len() - 1)
    }
}
