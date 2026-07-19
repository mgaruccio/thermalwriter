// SPDX-License-Identifier: GPL-3.0-or-later
//
// Frame encode path: wire-angle rotation + JPEG/RGB565 payload generation.

//! Encode a `RawFrame` into an `EncodedFrame` for a negotiated device.

use anyhow::{Context, Result, bail};
use image::{ImageBuffer, Rgb};

use crate::render::RawFrame;

use super::{DeviceInfo, EncodedFrame, FrameEncoding, wire_angle};

fn validate_raw_rgb(data: &[u8], width: u32, height: u32) -> Result<()> {
    let expected_len = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(3))
        .context("raw RGB frame size overflow")?;
    if data.len() != expected_len {
        bail!(
            "raw RGB payload length {} does not match {}x{} frame ({} bytes)",
            data.len(),
            width,
            height,
            expected_len
        );
    }
    Ok(())
}

/// Rotate raw RGB pixel data by the given degrees (0, 90, 180, 270).
/// Returns (new_data, new_width, new_height).
pub fn rotate_pixels(
    data: &[u8],
    width: u32,
    height: u32,
    degrees: u16,
) -> Result<(Vec<u8>, u32, u32)> {
    validate_raw_rgb(data, width, height)?;
    let w = width as usize;
    let h = height as usize;
    let pixel_count = w * h;

    let rotated = match degrees {
        0 => (data.to_vec(), width, height),
        180 => {
            let mut out = vec![0u8; data.len()];
            for i in 0..pixel_count {
                let src = i * 3;
                let dst = (pixel_count - 1 - i) * 3;
                out[dst..dst + 3].copy_from_slice(&data[src..src + 3]);
            }
            (out, width, height)
        }
        90 => {
            let mut out = vec![0u8; data.len()];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * 3;
                    let dst = (x * h + (h - 1 - y)) * 3;
                    out[dst..dst + 3].copy_from_slice(&data[src..src + 3]);
                }
            }
            (out, height, width)
        }
        270 => {
            let mut out = vec![0u8; data.len()];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * 3;
                    let dst = ((w - 1 - x) * h + y) * 3;
                    out[dst..dst + 3].copy_from_slice(&data[src..src + 3]);
                }
            }
            (out, height, width)
        }
        _ => {
            log::warn!("Unsupported rotation {}, using 0", degrees);
            (data.to_vec(), width, height)
        }
    };
    Ok(rotated)
}

/// Encode `frame` for `info` at the given user `rotation` and JPEG `quality`.
///
/// Requires the input frame dimensions to match the user-oriented canvas
/// (`oriented_dimensions(native_w, native_h, rotation)`). Applies the wire
/// angle once, then encodes JPEG or RGB565 at the resulting wire dimensions.
pub fn encode_frame(
    frame: &RawFrame,
    info: &DeviceInfo,
    rotation: u16,
    quality: u8,
) -> Result<EncodedFrame> {
    let (expect_w, expect_h) = super::oriented_dimensions(info.width(), info.height(), rotation)?;
    if frame.width != expect_w || frame.height != expect_h {
        bail!(
            "encode_frame input is {}x{}, expected oriented {}x{} for native {}x{} at rotation {}",
            frame.width,
            frame.height,
            expect_w,
            expect_h,
            info.width(),
            info.height(),
            rotation
        );
    }

    let angle = wire_angle(&info.profile, rotation)?;
    let (rotated, out_w, out_h) = rotate_pixels(&frame.data, frame.width, frame.height, angle)?;

    let encoding = info.encoding();
    let data = match encoding {
        FrameEncoding::Jpeg => encode_jpeg_bytes(&rotated, out_w, out_h, quality)?,
        FrameEncoding::Rgb565Le => encode_rgb565(&rotated, false),
        FrameEncoding::Rgb565Be => encode_rgb565(&rotated, true),
    };

    validate_encoded(&data, out_w, out_h, encoding)?;

    Ok(EncodedFrame {
        data,
        width: out_w,
        height: out_h,
        encoding,
    })
}

pub(crate) fn encode_jpeg_bytes(
    rgb: &[u8],
    width: u32,
    height: u32,
    quality: u8,
) -> Result<Vec<u8>> {
    let img: ImageBuffer<Rgb<u8>, _> = ImageBuffer::from_raw(width, height, rgb.to_vec())
        .ok_or_else(|| anyhow::anyhow!("Failed to create image buffer for JPEG encode"))?;
    let mut buf = std::io::Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    image::DynamicImage::ImageRgb8(img)
        .write_with_encoder(encoder)
        .context("JPEG encode failed")?;
    Ok(buf.into_inner())
}

fn encode_rgb565(rgb: &[u8], big_endian: bool) -> Vec<u8> {
    let pixels = rgb.len() / 3;
    let mut out = Vec::with_capacity(pixels * 2);
    for i in 0..pixels {
        let r = rgb[i * 3] as u16;
        let g = rgb[i * 3 + 1] as u16;
        let b = rgb[i * 3 + 2] as u16;
        let v = ((r & 0xF8) << 8) | ((g & 0xFC) << 3) | (b >> 3);
        if big_endian {
            out.extend_from_slice(&v.to_be_bytes());
        } else {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out
}

fn validate_encoded(data: &[u8], width: u32, height: u32, encoding: FrameEncoding) -> Result<()> {
    match encoding {
        FrameEncoding::Jpeg => {
            if data.len() < 2 || data[0] != 0xFF || data[1] != 0xD8 {
                bail!("JPEG encode produced invalid payload (missing SOI)");
            }
        }
        FrameEncoding::Rgb565Le | FrameEncoding::Rgb565Be => {
            let expected = (width as usize)
                .checked_mul(height as usize)
                .and_then(|n| n.checked_mul(2))
                .ok_or_else(|| anyhow::anyhow!("RGB565 size overflow for {width}x{height}"))?;
            if data.len() != expected {
                bail!(
                    "RGB565 payload length {} != expected {} for {}x{}",
                    data.len(),
                    expected,
                    width,
                    height
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{WireProtocol, build_device_info};

    fn solid_frame(w: u32, h: u32, rgb: [u8; 3]) -> RawFrame {
        let mut data = Vec::with_capacity((w * h * 3) as usize);
        for _ in 0..(w * h) {
            data.extend_from_slice(&rgb);
        }
        RawFrame {
            data,
            width: w,
            height: h,
        }
    }

    #[test]
    fn encode_jpeg_for_bulk_square() {
        let info = build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, Some(72)).unwrap();
        let frame = solid_frame(480, 480, [255, 0, 0]);
        let enc = encode_frame(&frame, &info, 0, 85).unwrap();
        assert!(enc.encoding.is_jpeg());
        assert_eq!((enc.width, enc.height), (480, 480));
        assert_eq!(&enc.data[..2], &[0xFF, 0xD8]);
    }

    #[test]
    fn encode_rgb565_be_for_scsi() {
        let info =
            build_device_info(WireProtocol::Scsi, 0x87cd, 0x70db, 100, 0, Some(100)).unwrap();
        let frame = solid_frame(320, 320, [0, 255, 0]);
        let enc = encode_frame(&frame, &info, 0, 85).unwrap();
        assert_eq!(enc.encoding, FrameEncoding::Rgb565Be);
        assert_eq!(enc.data.len(), 320 * 320 * 2);
    }

    #[test]
    fn rejects_mismatched_input_dimensions() {
        let info = build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, Some(72)).unwrap();
        let frame = solid_frame(320, 320, [0, 0, 0]);
        assert!(encode_frame(&frame, &info, 0, 85).is_err());
    }

    #[test]
    fn rejects_malformed_raw_rgb_before_nonzero_wire_rotation() {
        let info = build_device_info(WireProtocol::Scsi, 0x87cd, 0x70db, 50, 0, Some(50)).unwrap();
        let exact_len = 320 * 240 * 3;
        for invalid_len in [exact_len - 1, exact_len + 1] {
            let frame = RawFrame {
                data: vec![0; invalid_len],
                width: 320,
                height: 240,
            };
            let error = encode_frame(&frame, &info, 0, 85).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&format!("raw RGB payload length {invalid_len}")),
                "{error:#}"
            );
        }
    }

    #[test]
    fn rejects_raw_rgb_size_overflow() {
        let frame = RawFrame {
            data: Vec::new(),
            width: u32::MAX,
            height: u32::MAX,
        };
        let error = rotate_pixels(&frame.data, frame.width, frame.height, 0).unwrap_err();
        assert!(error.to_string().contains("size overflow"), "{error:#}");
    }
    const RED: [u8; 3] = [240, 24, 24];
    const GREEN: [u8; 3] = [24, 240, 24];
    const BLUE: [u8; 3] = [24, 24, 240];
    const YELLOW: [u8; 3] = [240, 240, 24];

    fn corner_frame(w: u32, h: u32) -> RawFrame {
        let mut data = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                let color = match (x < w / 2, y < h / 2) {
                    (true, true) => RED,
                    (false, true) => GREEN,
                    (true, false) => BLUE,
                    (false, false) => YELLOW,
                };
                data.extend_from_slice(&color);
            }
        }
        RawFrame {
            data,
            width: w,
            height: h,
        }
    }

    fn expected_corners(angle: u16) -> [[u8; 3]; 4] {
        match angle {
            0 => [RED, GREEN, BLUE, YELLOW],
            90 => [BLUE, RED, YELLOW, GREEN],
            180 => [YELLOW, BLUE, GREEN, RED],
            270 => [GREEN, YELLOW, RED, BLUE],
            _ => panic!("invalid test angle {angle}"),
        }
    }

    fn rgb565_corners(encoded: &EncodedFrame) -> [[u8; 3]; 4] {
        let pixel = |x: u32, y: u32| {
            let offset = ((y * encoded.width + x) * 2) as usize;
            let bytes = [encoded.data[offset], encoded.data[offset + 1]];
            let value = match encoded.encoding {
                FrameEncoding::Rgb565Be => u16::from_be_bytes(bytes),
                FrameEncoding::Rgb565Le => u16::from_le_bytes(bytes),
                FrameEncoding::Jpeg => panic!("expected RGB565"),
            };
            [
                ((value >> 11) as u8 & 0x1f) << 3,
                ((value >> 5) as u8 & 0x3f) << 2,
                (value as u8 & 0x1f) << 3,
            ]
        };
        [
            pixel(0, 0),
            pixel(encoded.width - 1, 0),
            pixel(0, encoded.height - 1),
            pixel(encoded.width - 1, encoded.height - 1),
        ]
    }

    fn quantized(color: [u8; 3]) -> [u8; 3] {
        [color[0] & 0xf8, color[1] & 0xfc, color[2] & 0xf8]
    }

    #[test]
    fn rgb565_rotate_panel_maps_all_user_rotation_corners_exactly() {
        let info = build_device_info(WireProtocol::Scsi, 0x87cd, 0x70db, 50, 0, Some(50)).unwrap();
        for (rotation, wire) in [(0, 90), (90, 0), (180, 270), (270, 180)] {
            let (w, h) =
                super::super::oriented_dimensions(info.width(), info.height(), rotation).unwrap();
            let encoded = encode_frame(&corner_frame(w, h), &info, rotation, 100).unwrap();
            assert_eq!(
                rgb565_corners(&encoded),
                expected_corners(wire).map(quantized),
                "rotation={rotation}"
            );
        }
    }

    fn jpeg_corners(encoded: &EncodedFrame) -> [[u8; 3]; 4] {
        let image = image::load_from_memory(&encoded.data).unwrap().to_rgb8();
        assert_eq!(image.dimensions(), (encoded.width, encoded.height));
        let inset_x = (encoded.width / 20).max(1);
        let inset_y = (encoded.height / 20).max(1);
        [
            image.get_pixel(inset_x, inset_y).0,
            image.get_pixel(encoded.width - 1 - inset_x, inset_y).0,
            image.get_pixel(inset_x, encoded.height - 1 - inset_y).0,
            image
                .get_pixel(encoded.width - 1 - inset_x, encoded.height - 1 - inset_y)
                .0,
        ]
    }

    fn assert_jpeg_close(actual: [[u8; 3]; 4], expected: [[u8; 3]; 4], label: &str) {
        for (index, (actual, expected)) in actual.into_iter().zip(expected).enumerate() {
            for channel in 0..3 {
                assert!(
                    actual[channel].abs_diff(expected[channel]) <= 35,
                    "{label} corner={index} channel={channel}: {actual:?} != {expected:?}"
                );
            }
        }
    }

    #[test]
    fn jpeg_square_and_widescreen_map_all_user_rotation_corners() {
        let cases = [
            (
                build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, Some(72)).unwrap(),
                [(0, 0), (90, 270), (180, 180), (270, 90)],
                "square",
            ),
            (
                build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 64, 0, Some(114)).unwrap(),
                [(0, 180), (90, 90), (180, 0), (270, 270)],
                "widescreen",
            ),
        ];
        for (info, rotations, label) in cases {
            for (rotation, wire) in rotations {
                let (w, h) =
                    super::super::oriented_dimensions(info.width(), info.height(), rotation)
                        .unwrap();
                let encoded = encode_frame(&corner_frame(w, h), &info, rotation, 100).unwrap();
                assert_jpeg_close(
                    jpeg_corners(&encoded),
                    expected_corners(wire),
                    &format!("{label} rotation={rotation}"),
                );
            }
        }
    }
}
