//! Preview the daemon's PNG/SVG compositing path without USB hardware.
//!
//! ```sh
//! cargo run --example preview_composite -- \
//!   --background assets/backgrounds/dark-gradient.png \
//!   --overlay examples/fixtures/calibration.svg \
//!   --output target/composite-preview.png \
//!   --inspect 240,240
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
    #[arg(long)]
    background: PathBuf,
    /// Optional transparent SVG/layout rendered over the background.
    #[arg(long)]
    overlay: Option<PathBuf>,
    /// Rotated 480x480 PNG output path.
    #[arg(long)]
    output: PathBuf,
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

fn main() -> Result<()> {
    let options = Options::parse();
    let background = Arc::new(BackgroundImage::from_file(&options.background)?);
    let template = read_template(options.overlay.as_deref())?;
    let mut renderer = SvgRenderer::new(&template, WIDTH, HEIGHT)?;
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
