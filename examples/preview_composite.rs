//! Preview the daemon's PNG/SVG compositing path without USB hardware.
//!
//! Static image background:
//! ```sh
//! cargo run --example preview_composite -- \
//!   --background assets/backgrounds/dark-gradient.png \
//!   --overlay examples/fixtures/calibration.svg \
//!   --output target/composite-preview.png \
//!   --inspect 240,240
//! ```
//!
//! Looping video background (requires `--features video` and an `ffmpeg`
//! binary; `--output` is a directory of `frame-NNN.png`):
//! ```sh
//! cargo run --features video --example preview_composite -- \
//!   --video /path/to/clip.mp4 \
//!   --video-fps 15 \
//!   --overlay examples/fixtures/calibration.svg \
//!   --frames 10 \
//!   --output target/video-preview
//! ```

use anyhow::{Context, Result, bail};
use clap::Parser;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thermalwriter::render::background::BackgroundImage;
use thermalwriter::render::svg::SvgRenderer;
use thermalwriter::render::{RawFrame, SensorData};
use thermalwriter::transport::encode::rotate_pixels;

type Rgba = [u8; 4];

const WIDTH: u32 = 480;
const HEIGHT: u32 = 480;
const TRANSPARENT_OVERLAY: &str =
    r#"<svg xmlns="http://www.w3.org/2000/svg" width="480" height="480"/>"#;

#[derive(Debug, Parser)]
#[command(
    name = "preview-composite",
    about = "Preview Thermalright LCD background compositing"
)]
struct Options {
    /// Background PNG (or another image format accepted by the daemon).
    #[arg(long, conflicts_with = "video")]
    background: Option<PathBuf>,
    /// Optional transparent SVG/layout rendered over the background.
    #[arg(long)]
    overlay: Option<PathBuf>,
    /// Output path: a single PNG for `--background`, or a directory of
    /// `frame-NNN.png` for `--video`.
    #[arg(long)]
    output: PathBuf,
    /// Local video file used as a looping animated background. Requires the
    /// `video` build feature and an `ffmpeg` binary.
    #[arg(long, conflicts_with = "background")]
    video: Option<PathBuf>,
    /// Decode/output frame rate cap for the video (1–60).
    #[arg(long, default_value_t = 15)]
    video_fps: u32,
    /// Video fit: "cover" (default) or "contain".
    #[arg(long, default_value = "cover")]
    video_fit: String,
    /// Number of frames to render for `--video`.
    #[arg(long, default_value_t = 8)]
    frames: u32,
    /// Inspect a pixel in the rotated output, written as X,Y.
    #[arg(long, value_parser = parse_pixel)]
    inspect: Option<(u32, u32)>,
    /// LCD hardware rotation in degrees: 0, 90, 180, or 270.
    #[arg(long, default_value_t = 180, value_parser = parse_rotation)]
    rotation: u16,
}

fn parse_pixel(value: &str) -> Result<(u32, u32), String> {
    let (x, y) = value
        .split_once(',')
        .ok_or_else(|| "pixel must be X,Y".to_owned())?;
    let x = x
        .parse()
        .map_err(|_| "pixel X must be an integer".to_owned())?;
    let y = y
        .parse()
        .map_err(|_| "pixel Y must be an integer".to_owned())?;
    if x >= WIDTH || y >= HEIGHT {
        return Err(format!("pixel must be within {WIDTH}x{HEIGHT}"));
    }
    Ok((x, y))
}

fn parse_rotation(value: &str) -> Result<u16, String> {
    let rotation: u16 = value
        .parse()
        .map_err(|_| "rotation must be 0, 90, 180, or 270".to_owned())?;
    if matches!(rotation, 0 | 90 | 180 | 270) {
        Ok(rotation)
    } else {
        Err("rotation must be 0, 90, 180, or 270".to_owned())
    }
}

fn source_coordinate((x, y): (u32, u32), rotation: u16) -> (u32, u32) {
    match rotation {
        0 => (x, y),
        90 => (y, HEIGHT - 1 - x),
        180 => (WIDTH - 1 - x, HEIGHT - 1 - y),
        270 => (WIDTH - 1 - y, x),
        _ => unreachable!("rotation is validated by clap"),
    }
}

fn unpremultiply(pixel: Rgba) -> Rgba {
    let alpha = u32::from(pixel[3]);
    if alpha == 0 {
        return [0, 0, 0, 0];
    }
    [
        ((u32::from(pixel[0]) * 255) / alpha).min(255) as u8,
        ((u32::from(pixel[1]) * 255) / alpha).min(255) as u8,
        ((u32::from(pixel[2]) * 255) / alpha).min(255) as u8,
        pixel[3],
    ]
}

fn inspect_pixel(pixmap: &tiny_skia::Pixmap, output_pixel: (u32, u32), rotation: u16) {
    let (x, y) = source_coordinate(output_pixel, rotation);
    let offset = ((y * pixmap.width() + x) * 4) as usize;
    let data = pixmap.data();
    let premultiplied = [
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ];
    println!(
        "pixel ({},{}) -> composed ({},{}) premultiplied RGBA={premultiplied:?} un-premultiplied RGBA={:?}",
        output_pixel.0,
        output_pixel.1,
        x,
        y,
        unpremultiply(premultiplied)
    );
}

fn read_template(path: Option<&Path>) -> Result<String> {
    path.map(std::fs::read_to_string)
        .transpose()
        .with_context(|| "failed to read SVG overlay")
        .map(|template| template.unwrap_or_else(|| TRANSPARENT_OVERLAY.to_owned()))
}

fn save_rotated(frame: &RawFrame, output: &Path, rotation: u16) -> Result<(u32, u32)> {
    let (data, width, height) = rotate_pixels(&frame.data, frame.width, frame.height, rotation)?;
    let image = image::RgbImage::from_raw(width, height, data)
        .ok_or_else(|| anyhow::anyhow!("rotated RGB frame has invalid dimensions"))?;
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    image
        .save(output)
        .with_context(|| format!("failed to write {}", output.display()))?;
    Ok((width, height))
}

#[cfg(feature = "video")]
fn run_video_preview(renderer: &mut SvgRenderer<'static>, options: &Options) -> Result<()> {
    let output_dir = &options.output;
    std::fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "failed to create video preview dir {}",
            output_dir.display()
        )
    })?;

    // Warm up: wait for the first decoded frame before capturing so the
    // saved frames show the video, not the fallback fill.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while renderer.video_frame_count().unwrap_or(0) == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if renderer.video_frame_count().unwrap_or(0) == 0 {
        bail!("video produced no frames within 10s; check the file and ffmpeg");
    }

    let interval = std::time::Duration::from_millis(1000u64 / u64::from(options.video_fps.max(1)));
    let mut last_pixmap = None;
    for i in 1..=options.frames {
        std::thread::sleep(interval);
        let pixmap = renderer.render_pixmap(&SensorData::new())?;
        last_pixmap = Some(pixmap.clone());
        let out = output_dir.join(format!("frame-{i:03}.png"));
        RawFrame::from_pixmap(&pixmap).save_png(
            out.to_str()
                .ok_or_else(|| anyhow::anyhow!("output path is not valid UTF-8"))?,
        )?;
        println!(
            "wrote {} (video frame {i}/{})",
            out.display(),
            options.frames
        );
    }
    if let (Some(pixel), Some(pixmap)) = (options.inspect, last_pixmap.as_ref()) {
        inspect_pixel(pixmap, pixel, options.rotation);
    }
    Ok(())
}

fn main() -> Result<()> {
    let options = Options::parse();
    if options.video.is_none() && options.background.is_none() {
        bail!("provide one of --background or --video");
    }
    let template = read_template(options.overlay.as_deref())?;
    let mut renderer = SvgRenderer::new(&template, WIDTH, HEIGHT)?;

    if let Some(video_path) = &options.video {
        #[cfg(feature = "video")]
        {
            let fit = thermalwriter::render::video::VideoFit::parse(&options.video_fit)?;
            renderer.set_video_background(video_path, options.video_fps, fit)?;
            run_video_preview(&mut renderer, &options)?;
            return Ok(());
        }
        #[cfg(not(feature = "video"))]
        {
            bail!(
                "--video ({}) requires building with the `video` feature \\
                 (e.g. `cargo run --features video --example preview_composite -- ...`)",
                video_path.display()
            );
        }
    }

    let background = Arc::new(BackgroundImage::from_file(
        options.background.as_ref().expect("validated above"),
    )?);
    renderer.set_background(Some(background))?;
    let pixmap = renderer.render_pixmap(&SensorData::new())?;
    if pixmap.width() != WIDTH || pixmap.height() != HEIGHT {
        bail!(
            "composited pixmap is {}x{}, expected {WIDTH}x{HEIGHT}",
            pixmap.width(),
            pixmap.height()
        );
    }

    if let Some(pixel) = options.inspect {
        inspect_pixel(&pixmap, pixel, options.rotation);
    }
    let frame = RawFrame::from_pixmap(&pixmap);
    let (width, height) = save_rotated(&frame, &options.output, options.rotation)?;
    println!(
        "wrote {} ({}x{}, rotation {}°)",
        options.output.display(),
        width,
        height,
        options.rotation
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_bounds_pixel_coordinates() {
        assert_eq!(parse_pixel("12,34").unwrap(), (12, 34));
        assert!(parse_pixel("480,0").is_err());
        assert!(parse_pixel("12").is_err());
    }

    #[test]
    fn maps_rotated_output_coordinates_to_composed_pixels() {
        assert_eq!(source_coordinate((0, 0), 0), (0, 0));
        assert_eq!(source_coordinate((0, 0), 90), (0, 479));
        assert_eq!(source_coordinate((0, 0), 180), (479, 479));
        assert_eq!(source_coordinate((0, 0), 270), (479, 0));
    }

    #[test]
    fn unpremultiplies_transparent_and_partial_pixels() {
        assert_eq!(unpremultiply([0, 0, 0, 0]), [0, 0, 0, 0]);
        assert_eq!(unpremultiply([64, 32, 16, 128]), [127, 63, 31, 128]);
    }
}
