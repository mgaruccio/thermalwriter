// Background image decoding and resizing for the LCD compositing pipeline.
//
// Backgrounds are decoded once (at startup or on D-Bus SetBackground) into a
// premultiplied-RGBA tiny_skia::Pixmap sized to 480×480. Subsequent compositing
// is a straight blit with no per-tick resize or decode cost.
//
// Premultiplication note: tiny_skia::Pixmap stores premultiplied RGBA.
// For fully-opaque images (alpha=255 everywhere) premultiplied == straight, so
// the math is a no-op numerically. We premultiply unconditionally to handle
// PNGs that carry an alpha channel.

use anyhow::{Context, Result, anyhow, bail};
use image::imageops::FilterType;
use image::{ImageReader, Limits};
use tiny_skia::{IntSize, Pixmap};

/// Target LCD dimensions. Backgrounds are resized to this at decode time so
/// subsequent compositing is a straight blit — Lanczos3 chosen for quality at
/// the LCD's low resolution where upscaling artifacts are visible.
const LCD_W: u32 = 480;
const LCD_H: u32 = 480;

const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024; // 8 MB file-size pre-check
const MAX_DIM_PX: u32 = 8192; // max decoded width or height
const MAX_ALLOC_BYTES: u64 = 256 * 1024 * 1024; // 256 MB decoded allocation cap

/// Decode a background image from raw bytes (PNG/JPEG) into a 480×480
/// premultiplied-RGBA Pixmap ready for compositing under a layout.
pub fn decode_to_pixmap(bytes: &[u8]) -> Result<Pixmap> {
    let cursor = std::io::Cursor::new(bytes);
    let mut reader = ImageReader::new(cursor)
        .with_guessed_format()
        .context("Failed to identify background image format")?;

    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIM_PX);
    limits.max_image_height = Some(MAX_DIM_PX);
    limits.max_alloc = Some(MAX_ALLOC_BYTES);
    reader.limits(limits);

    let img = reader
        .decode()
        .context("Failed to decode background image (unsupported format or limit exceeded)")?;

    let resized = image::imageops::resize(&img.into_rgba8(), LCD_W, LCD_H, FilterType::Lanczos3);

    // Premultiply alpha. For fully-opaque images (alpha=255) this is a no-op
    // numerically, but we run it unconditionally to handle PNGs with alpha.
    let mut data = resized.into_raw();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        px[0] = ((px[0] as u32 * a) / 255) as u8;
        px[1] = ((px[1] as u32 * a) / 255) as u8;
        px[2] = ((px[2] as u32 * a) / 255) as u8;
    }

    let size =
        IntSize::from_wh(LCD_W, LCD_H).ok_or_else(|| anyhow!("invalid LCD size constants"))?;
    Pixmap::from_vec(data, size).ok_or_else(|| anyhow!("Pixmap::from_vec rejected RGBA buffer"))
}

/// Decode a background image from a file path. Used by the daemon at startup
/// and on D-Bus SetBackground calls.
pub fn decode_from_file(path: &std::path::Path) -> Result<Pixmap> {
    let file_len = std::fs::metadata(path)
        .with_context(|| format!("Failed to stat background file: {}", path.display()))?
        .len();

    if file_len > MAX_FILE_BYTES {
        bail!("background file too large: {} bytes (max 8 MB)", file_len);
    }

    let bytes = std::fs::read(path)
        .with_context(|| format!("Failed to read background file: {}", path.display()))?;
    decode_to_pixmap(&bytes)
}
