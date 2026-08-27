// SPDX-License-Identifier: GPL-3.0-or-later
//
// Deterministic validation test cards encoded via the production frame path.

#![allow(clippy::too_many_arguments)]

use std::path::Path;

use anyhow::{Context, Result};

use crate::render::RawFrame;

use super::super::encode::encode_frame;
use super::super::profile::{DeviceInfo, oriented_dimensions};
use super::super::{EncodedFrame, PROTOCOL_CHUNK_BYTES};

/// Three visual validation cards plus metadata for prompts and expected PNGs.
#[derive(Debug, Clone)]
pub struct TestCardBundle {
    pub run_id: String,
    pub vid_pid_label: String,
    pub target_marker: RawFrame,
    pub orientation: RawFrame,
    pub colors: RawFrame,
}

/// Build user-oriented RGB frames at the negotiated native resolution.
pub fn generate_test_cards(
    info: &DeviceInfo,
    vid: u16,
    pid: u16,
    run_id: &str,
    rotation: u16,
) -> Result<TestCardBundle> {
    let (width, height) = oriented_dimensions(info.width(), info.height(), rotation)?;
    let vid_pid_label = format!("{vid:04X}:{pid:04X}");
    Ok(TestCardBundle {
        run_id: run_id.to_string(),
        vid_pid_label: vid_pid_label.clone(),
        target_marker: draw_target_marker(width, height, run_id, &vid_pid_label),
        orientation: draw_orientation_card(width, height),
        colors: draw_color_card(width, height),
    })
}

/// Encode each card with [`encode_frame`] and write expected PNG previews.
pub fn encode_and_save_expected(
    bundle: &TestCardBundle,
    info: &DeviceInfo,
    rotation: u16,
    quality: u8,
    output_dir: &Path,
) -> Result<Vec<EncodedFrame>> {
    let frames = [
        ("expected-target-marker.png", &bundle.target_marker),
        ("expected-orientation.png", &bundle.orientation),
        ("expected-colors.png", &bundle.colors),
    ];
    let mut encoded = Vec::with_capacity(frames.len());
    for (name, frame) in frames {
        let path = output_dir.join(name);
        frame
            .save_png(path.to_str().context("expected card png path")?)
            .with_context(|| format!("write {}", path.display()))?;
        encoded.push(encode_frame(frame, info, rotation, quality)?);
    }
    Ok(encoded)
}

/// Pad JPEG/RGB payload to HID report chunk multiples for write_chunked.
pub fn pad_to_hid_chunks(payload: &[u8]) -> Vec<u8> {
    if payload.is_empty() {
        return vec![0; PROTOCOL_CHUNK_BYTES];
    }
    let rem = payload.len() % PROTOCOL_CHUNK_BYTES;
    if rem == 0 {
        return payload.to_vec();
    }
    let mut padded = payload.to_vec();
    padded.resize(payload.len() + (PROTOCOL_CHUNK_BYTES - rem), 0);
    padded
}

fn draw_target_marker(width: u32, height: u32, run_id: &str, vid_pid: &str) -> RawFrame {
    let mut data = vec![0x22_u8; usize::try_from(width * height * 3).unwrap_or(0)];
    fill_rect(
        &mut data,
        width,
        height,
        0,
        0,
        width,
        height / 8,
        0xFF,
        0xFF,
        0xFF,
    );
    let label = format!("{run_id} {vid_pid}");
    draw_label_stripes(&mut data, width, height, &label, height / 4);
    fill_rect(
        &mut data,
        width,
        height,
        width / 4,
        height / 2,
        width / 2,
        height / 8,
        0xFF,
        0x00,
        0x88,
    );
    RawFrame {
        data,
        width,
        height,
    }
}

fn draw_orientation_card(width: u32, height: u32) -> RawFrame {
    let mut data = vec![0x11_u8; usize::try_from(width * height * 3).unwrap_or(0)];
    let marker = (width.min(height) / 6).max(8);
    fill_rect(
        &mut data, width, height, 0, 0, width, marker, 0xFF, 0xFF, 0xFF,
    );
    let corners = [
        (0, 0, 0xFF, 0x00, 0x00),
        (width.saturating_sub(marker), 0, 0x00, 0xFF, 0x00),
        (0, height.saturating_sub(marker), 0x00, 0x00, 0xFF),
        (
            width.saturating_sub(marker),
            height.saturating_sub(marker),
            0xFF,
            0xFF,
            0x00,
        ),
    ];
    for (x, y, r, g, b) in corners {
        fill_rect(&mut data, width, height, x, y, marker, marker, r, g, b);
    }
    draw_label_stripes(&mut data, width, height, "TOP", marker + 4);
    RawFrame {
        data,
        width,
        height,
    }
}

fn draw_color_card(width: u32, height: u32) -> RawFrame {
    let mut data = vec![0_u8; usize::try_from(width * height * 3).unwrap_or(0)];
    let blocks = [
        (0xFF, 0x00, 0x00),
        (0x00, 0xFF, 0x00),
        (0x00, 0x00, 0xFF),
        (0xFF, 0xFF, 0xFF),
        (0x00, 0x00, 0x00),
        (0x80, 0x80, 0x80),
    ];
    let cols: u32 = 3;
    let rows: u32 = 2;
    let cell_w = width / cols;
    let cell_h = height / rows;
    for (index, (r, g, b)) in blocks.iter().enumerate() {
        let col = (index as u32) % cols;
        let row = (index as u32) / cols;
        fill_rect(
            &mut data,
            width,
            height,
            col * cell_w,
            row * cell_h,
            cell_w,
            cell_h,
            *r,
            *g,
            *b,
        );
    }
    RawFrame {
        data,
        width,
        height,
    }
}

fn fill_rect(
    data: &mut [u8],
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    r: u8,
    g: u8,
    b: u8,
) {
    let w = width.min(x.saturating_add(w)).saturating_sub(x);
    let h = height.min(y.saturating_add(h)).saturating_sub(y);
    for row in y..y.saturating_add(h) {
        for col in x..x.saturating_add(w) {
            let idx = ((row * width + col) * 3) as usize;
            if let Some(slice) = data.get_mut(idx..idx + 3) {
                slice.copy_from_slice(&[r, g, b]);
            }
        }
    }
}

fn draw_label_stripes(data: &mut [u8], width: u32, height: u32, label: &str, y: u32) {
    let stripe_h = 6;
    let mut x: u32 = 8;
    for ch in label.chars() {
        if ch == ' ' {
            x = x.saturating_add(8);
            continue;
        }
        draw_char_block(data, width, height, x, y, stripe_h, ch);
        x = x.saturating_add(stripe_h * 4 + 4);
        if x + stripe_h * 4 >= width {
            break;
        }
    }
}

fn draw_char_block(data: &mut [u8], width: u32, height: u32, x: u32, y: u32, h: u32, ch: char) {
    let pattern = char_pattern(ch);
    let scale = h.max(4);
    for (row, bits) in pattern.iter().enumerate() {
        for col in 0..5 {
            if bits & (1 << (4 - col)) != 0 {
                fill_rect(
                    data,
                    width,
                    height,
                    x + col as u32 * scale / 2,
                    y + row as u32 * scale / 2,
                    scale / 2 + 1,
                    scale / 2 + 1,
                    0x00,
                    0x00,
                    0x00,
                );
            }
        }
    }
}

fn char_pattern(ch: char) -> [u8; 7] {
    match ch.to_ascii_uppercase() {
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x06, 0x08, 0x10, 0x1F],
        '3' => [0x1E, 0x01, 0x01, 0x0E, 0x01, 0x01, 0x1E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        ':' => [0x00, 0x04, 0x00, 0x00, 0x04, 0x00, 0x00],
        _ => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::profile::{WireProtocol, build_device_info};

    #[test]
    fn pad_to_hid_chunks_rounds_up() {
        let padded = pad_to_hid_chunks(&vec![1; 600]);
        assert_eq!(padded.len() % PROTOCOL_CHUNK_BYTES, 0);
        assert_eq!(padded.len(), 1024);
    }

    #[test]
    fn generate_cards_matches_oriented_dimensions() {
        let info = build_device_info(WireProtocol::HidType2, 0x0416, 0x5302, 58, 0, None).unwrap();
        let bundle = generate_test_cards(&info, 0x0416, 0x5302, "RUN1", 0).unwrap();
        assert_eq!(bundle.target_marker.width, 320);
        assert_eq!(bundle.target_marker.height, 240);
        assert!(bundle.target_marker.data.len() == 320 * 240 * 3);
    }

    #[test]
    fn encode_cards_uses_production_path() {
        let info = build_device_info(WireProtocol::Bulk, 0x87ad, 0x70db, 4, 5, None).unwrap();
        let bundle = generate_test_cards(&info, 0x87ad, 0x70db, "T1", 180).unwrap();
        let encoded = encode_frame(&bundle.colors, &info, 180, 85).unwrap();
        assert!(encoded.data.len() > 64);
    }
}
