// SPDX-License-Identifier: GPL-3.0-or-later
//
// Background image decoding and profile-neutral cover scaling.
//
// Source pixels are retained as straight RGBA. Rasterization to a target
// canvas uses centered cover: crop first to the target aspect, then a single
// Lanczos3 resize. Premultiply only on the final target pixmap.
//
// Rasterization accounts for the retained source, the resize sampler's
// float intermediate, and the final target buffer as one live allocation
// budget. The source is borrowed throughout, so no full-size clone or crop
// buffer is needed.

use anyhow::{Context, Result, anyhow, bail};
use image::imageops::FilterType;
use image::{ImageReader, Limits, RgbaImage};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tiny_skia::{IntSize, Pixmap};

const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024; // 8 MB file-size pre-check
const MAX_DIM_PX: u32 = 8192; // max decoded width or height
const MAX_ALLOC_BYTES: u64 = 256 * 1024 * 1024; // aggregate live rasterization cap

/// Decoded background retained at source dimensions (straight RGBA).
#[derive(Clone)]
pub struct BackgroundImage {
    /// Straight RGBA8 source pixels.
    rgba: Arc<RgbaImage>,
    width: u32,
    height: u32,
    /// Optional originating path for diagnostics / config restore.
    pub source_path: Option<PathBuf>,
}

impl BackgroundImage {
    /// Decode raw image bytes into a source-dimension BackgroundImage.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
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
            .context("Failed to decode background image (unsupported format or limit exceeded)")?
            .into_rgba8();
        let width = img.width();
        let height = img.height();
        if width == 0 || height == 0 {
            bail!("background image has zero dimensions");
        }
        Ok(Self {
            rgba: Arc::new(img),
            width,
            height,
            source_path: None,
        })
    }

    /// Decode from a file path with size pre-check.
    pub fn from_file(path: &Path) -> Result<Self> {
        let file_len = std::fs::metadata(path)
            .with_context(|| format!("Failed to stat background file: {}", path.display()))?
            .len();

        if file_len > MAX_FILE_BYTES {
            bail!("background file too large: {} bytes (max 8 MB)", file_len);
        }

        let bytes = std::fs::read(path)
            .with_context(|| format!("Failed to read background file: {}", path.display()))?;
        let mut img = Self::decode(&bytes)?;
        img.source_path = Some(path.to_path_buf());
        Ok(img)
    }

    pub fn source_dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Centered-cover rasterize to `target_w`×`target_h` premultiplied pixmap.
    ///
    /// Rejects zero/over-limit targets. Crop is computed at source resolution
    /// for the target aspect, then a single Lanczos3 resize is performed —
    /// never an oversized cover intermediate.
    pub fn to_pixmap(&self, target_w: u32, target_h: u32) -> Result<Pixmap> {
        if target_w == 0 || target_h == 0 {
            bail!("invalid background target dimensions {target_w}x{target_h}");
        }

        let (crop_x, crop_y, crop_w, crop_h) =
            cover_crop(self.width, self.height, target_w, target_h)?;
        let live_bytes =
            rasterization_alloc_bytes(self.width, self.height, crop_w, crop_h, target_w, target_h)?;
        if live_bytes > MAX_ALLOC_BYTES {
            bail!("background rasterization requires {live_bytes} bytes (max {MAX_ALLOC_BYTES})");
        }
        let src = self.rgba.as_ref();
        let cropped = image::imageops::crop_imm(src, crop_x, crop_y, crop_w, crop_h);
        let resized = image::imageops::resize(&*cropped, target_w, target_h, FilterType::Lanczos3);

        let mut data = resized.into_raw();
        // Premultiply alpha once on the target buffer.
        for px in data.chunks_exact_mut(4) {
            let a = u32::from(px[3]);
            px[0] = ((u32::from(px[0]) * a) / 255) as u8;
            px[1] = ((u32::from(px[1]) * a) / 255) as u8;
            px[2] = ((u32::from(px[2]) * a) / 255) as u8;
        }

        let size = IntSize::from_wh(target_w, target_h)
            .ok_or_else(|| anyhow!("invalid target size {target_w}x{target_h}"))?;
        Pixmap::from_vec(data, size).ok_or_else(|| anyhow!("Pixmap::from_vec rejected RGBA buffer"))
    }
}

/// Estimate the retained source, Lanczos vertical-sampling intermediate, and
/// target RGBA buffers that coexist during rasterization.
fn rasterization_alloc_bytes(
    source_w: u32,
    source_h: u32,
    crop_w: u32,
    crop_h: u32,
    target_w: u32,
    target_h: u32,
) -> Result<u64> {
    let source_bytes = u64::from(source_w)
        .checked_mul(u64::from(source_h))
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| anyhow!("background source size overflow {source_w}x{source_h}"))?;
    let intermediate_bytes = if (crop_w, crop_h) == (target_w, target_h) {
        0
    } else {
        u64::from(crop_w)
            .checked_mul(u64::from(target_h))
            .and_then(|n| n.checked_mul(16))
            .ok_or_else(|| anyhow!("background resize intermediate size overflow"))?
    };
    let target_bytes = u64::from(target_w)
        .checked_mul(u64::from(target_h))
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| anyhow!("background target size overflow {target_w}x{target_h}"))?;

    source_bytes
        .checked_add(intermediate_bytes)
        .and_then(|n| n.checked_add(target_bytes))
        .ok_or_else(|| anyhow!("background rasterization allocation overflow"))
}

/// Compute centered cover crop rectangle in source pixels for target aspect.
fn cover_crop(
    src_w: u32,
    src_h: u32,
    target_w: u32,
    target_h: u32,
) -> Result<(u32, u32, u32, u32)> {
    // Compare src_w/src_h ? target_w/target_h via cross products.
    let sw = u64::from(src_w);
    let sh = u64::from(src_h);
    let tw = u64::from(target_w);
    let th = u64::from(target_h);
    // If source is relatively wider than target, crop sides; else crop top/bottom.
    // sw/sh > tw/th  <=>  sw*th > sh*tw
    let src_cross = sw
        .checked_mul(th)
        .ok_or_else(|| anyhow!("cover crop overflow"))?;
    let tgt_cross = sh
        .checked_mul(tw)
        .ok_or_else(|| anyhow!("cover crop overflow"))?;

    if src_cross > tgt_cross {
        // Source wider: full height, crop width.
        let crop_h = src_h;
        let crop_w = ((sh * tw) / th) as u32;
        let crop_w = crop_w.max(1).min(src_w);
        let crop_x = (src_w - crop_w) / 2;
        Ok((crop_x, 0, crop_w, crop_h))
    } else {
        // Source taller or equal: full width, crop height.
        let crop_w = src_w;
        let crop_h = ((sw * th) / tw) as u32;
        let crop_h = crop_h.max(1).min(src_h);
        let crop_y = (src_h - crop_h) / 2;
        Ok((0, crop_y, crop_w, crop_h))
    }
}

/// Backward-compatible helper: decode and rasterize to 480×480.
pub fn decode_to_pixmap(bytes: &[u8]) -> Result<Pixmap> {
    BackgroundImage::decode(bytes)?.to_pixmap(480, 480)
}

/// Backward-compatible helper: decode file and rasterize to 480×480.
pub fn decode_from_file(path: &Path) -> Result<Pixmap> {
    BackgroundImage::from_file(path)?.to_pixmap(480, 480)
}

/// Decode file into a reusable BackgroundImage (source dimensions retained).
pub fn load_background(path: &Path) -> Result<BackgroundImage> {
    BackgroundImage::from_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut img = image::RgbaImage::new(w, h);
        for p in img.pixels_mut() {
            *p = image::Rgba(rgba);
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    }

    #[test]
    fn cover_crop_wide_source_crops_sides() {
        let (x, y, w, h) = cover_crop(8192, 1, 320, 240).unwrap();
        assert_eq!(y, 0);
        assert_eq!(h, 1);
        assert!(w < 8192);
        assert_eq!(x, (8192 - w) / 2);
    }

    #[test]
    fn to_pixmap_rejects_zero_target() {
        let bg = BackgroundImage::decode(&solid_png(64, 64, [255, 0, 0, 255])).unwrap();
        assert!(bg.to_pixmap(0, 100).is_err());
    }

    #[test]
    fn to_pixmap_exact_non_square() {
        let bg = BackgroundImage::decode(&solid_png(100, 50, [0, 255, 0, 255])).unwrap();
        let pm = bg.to_pixmap(320, 240).unwrap();
        assert_eq!((pm.width(), pm.height()), (320, 240));
    }

    #[test]
    fn decode_from_extreme_wide_source() {
        // 8192×1 source cover-cropped to non-square target must not allocate
        // an oversized intermediate.
        let bg = BackgroundImage::decode(&solid_png(8192, 1, [0, 0, 255, 255])).unwrap();
        let pm = bg.to_pixmap(854, 480).unwrap();
        assert_eq!((pm.width(), pm.height()), (854, 480));
    }

    #[test]
    fn rasterization_budget_rejects_maximum_source_before_allocation() {
        let (_, _, crop_w, crop_h) = cover_crop(8192, 8192, 480, 480).unwrap();
        let live_bytes = rasterization_alloc_bytes(8192, 8192, crop_w, crop_h, 480, 480).unwrap();

        assert_eq!(8192_u64 * 8192 * 4, MAX_ALLOC_BYTES);
        assert!(
            live_bytes > MAX_ALLOC_BYTES,
            "source plus resize buffers must exceed the aggregate cap"
        );

        // Keep the fixture tiny: the budget check must run before touching the
        // source view, so this exercises the real `to_pixmap` rejection path
        // without allocating a 256 MiB test image.
        let bg = BackgroundImage {
            rgba: Arc::new(RgbaImage::new(1, 1)),
            width: 8192,
            height: 8192,
            source_path: None,
        };
        assert!(bg.to_pixmap(480, 480).is_err());
    }

    #[test]
    fn rasterization_budget_allows_same_size_copy_within_cap() {
        let live_bytes = rasterization_alloc_bytes(4096, 4096, 4096, 4096, 4096, 4096).unwrap();
        assert_eq!(live_bytes, 128 * 1024 * 1024);
    }

    #[test]
    fn rasterization_budget_rejects_overflow() {
        assert!(rasterization_alloc_bytes(u32::MAX, u32::MAX, 1, 1, 1, 1).is_err());
    }
}
