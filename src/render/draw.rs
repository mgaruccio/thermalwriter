// Drawing: renders positioned elements onto a tiny-skia pixmap using fontdue for text.

use tiny_skia::*;
use fontdue::{Font, FontSettings};

use super::layout::LayoutNode;
use super::parser::Color as ElementColor;

// Embed a default font at compile time
const DEFAULT_FONT_BYTES: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");

fn default_font() -> &'static Font {
    static FONT: std::sync::OnceLock<Font> = std::sync::OnceLock::new();
    FONT.get_or_init(|| {
        Font::from_bytes(DEFAULT_FONT_BYTES, FontSettings::default())
            .expect("Failed to load embedded font")
    })
}

/// Render a list of positioned layout nodes into a tiny-skia Pixmap.
pub fn render_nodes(nodes: &[LayoutNode], width: u32, height: u32) -> Pixmap {
    let mut pixmap = Pixmap::new(width, height).unwrap();

    for node in nodes {
        // Draw background
        if let Some(ref bg) = node.style.background {
            let mut paint = Paint::default();
            paint.set_color_rgba8(bg.r, bg.g, bg.b, bg.a);
            if let Some(rect) = Rect::from_xywh(node.x, node.y, node.width, node.height) {
                pixmap.fill_rect(rect, &paint, Transform::identity(), None);
            }
        }

        // Draw text
        if let Some(ref text) = node.text {
            if !text.is_empty() {
                let font_size = node.style.font_size.unwrap_or(16.0);
                let color = node.style.color.as_ref().cloned().unwrap_or(ElementColor::white());
                let text_align = node.style.text_align.as_deref().unwrap_or("left");

                draw_text(
                    &mut pixmap,
                    text,
                    node.x,
                    node.y,
                    node.width,
                    node.height,
                    font_size,
                    &color,
                    text_align,
                );
            }
        }
    }

    pixmap
}

fn draw_text(
    pixmap: &mut Pixmap,
    text: &str,
    x: f32,
    y: f32,
    container_w: f32,
    container_h: f32,
    font_size: f32,
    color: &ElementColor,
    text_align: &str,
) {
    let font = default_font();

    // Rasterize each glyph and compute total text width
    let mut glyphs: Vec<(fontdue::Metrics, Vec<u8>)> = Vec::new();
    let mut total_width = 0.0f32;

    for ch in text.chars() {
        let (metrics, bitmap) = font.rasterize(ch, font_size);
        total_width += metrics.advance_width;
        glyphs.push((metrics, bitmap));
    }

    // Compute starting X based on text-align
    let start_x = match text_align {
        "center" => x + (container_w - total_width) / 2.0,
        "right" => x + container_w - total_width,
        _ => x, // "left" or default
    };

    // Vertically center the text in the container
    let line_height = font_size;
    let start_y = y + (container_h - line_height) / 2.0;

    let mut cursor_x = start_x;
    let pixmap_w = pixmap.width() as i32;
    let pixmap_h = pixmap.height() as i32;

    for (metrics, bitmap) in &glyphs {
        let glyph_x = cursor_x + metrics.xmin as f32;
        let glyph_y = start_y + (font_size - metrics.height as f32 - metrics.ymin as f32);

        // Blit glyph bitmap onto pixmap
        for row in 0..metrics.height {
            for col in 0..metrics.width {
                let alpha = bitmap[row * metrics.width + col];
                if alpha == 0 { continue; }

                let px = (glyph_x + col as f32) as i32;
                let py = (glyph_y + row as f32) as i32;

                if px < 0 || py < 0 || px >= pixmap_w || py >= pixmap_h {
                    continue;
                }

                let idx = (py as u32 * pixmap.width() + px as u32) as usize * 4;
                let data = pixmap.data_mut();

                // Alpha blend the glyph pixel
                let a = alpha as u16;
                let inv_a = 255 - a;
                data[idx]     = ((color.r as u16 * a + data[idx]     as u16 * inv_a) / 255) as u8;
                data[idx + 1] = ((color.g as u16 * a + data[idx + 1] as u16 * inv_a) / 255) as u8;
                data[idx + 2] = ((color.b as u16 * a + data[idx + 2] as u16 * inv_a) / 255) as u8;
                data[idx + 3] = 255;
            }
        }

        cursor_x += metrics.advance_width;
    }
}
