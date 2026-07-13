// Rendering pipeline: HTML/CSS template parsing, layout computation, and pixmap drawing.
// Converts HTML/CSS templates into 480x480 JPEG frames for the cooler LCD.

pub mod background;
pub mod components;
pub mod draw;
pub mod frontmatter;
pub mod layout;
pub mod parser;
pub mod svg;
#[cfg(feature = "daemon")]
pub mod xvfb;

#[cfg(feature = "blitz")]
pub mod blitz;

use crate::render::background::BackgroundImage;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tiny_skia::Pixmap;

/// Sensor data: flat map of key → string value.
pub type SensorData = HashMap<String, String>;

/// A rendered frame as raw RGB pixel data (3 bytes per pixel, row-major).
#[derive(Debug, Clone)]
pub struct RawFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl RawFrame {
    /// Convert a tiny_skia Pixmap (premultiplied RGBA) to RawFrame (straight RGB).
    #[allow(unknown_lints)]
    #[allow(clippy::manual_checked_ops)]
    pub fn from_pixmap(pixmap: &Pixmap) -> Self {
        let data = pixmap.data();
        let pixel_count = (pixmap.width() * pixmap.height()) as usize;
        let mut rgb = Vec::with_capacity(pixel_count * 3);
        for chunk in data.chunks(4) {
            let a = chunk[3] as u16;
            if a == 0 {
                rgb.extend_from_slice(&[0, 0, 0]);
            } else {
                let r = ((chunk[0] as u16 * 255) / a).min(255) as u8;
                let g = ((chunk[1] as u16 * 255) / a).min(255) as u8;
                let b = ((chunk[2] as u16 * 255) / a).min(255) as u8;
                rgb.extend_from_slice(&[r, g, b]);
            }
        }
        Self {
            data: rgb,
            width: pixmap.width(),
            height: pixmap.height(),
        }
    }

    /// Save frame as PNG (convenience for examples/debugging).
    pub fn save_png(&self, path: &str) -> anyhow::Result<()> {
        use image::{ImageBuffer, Rgb};
        let img: ImageBuffer<Rgb<u8>, _> =
            ImageBuffer::from_raw(self.width, self.height, self.data.clone())
                .ok_or_else(|| anyhow::anyhow!("Failed to create image buffer"))?;
        img.save(path)?;
        Ok(())
    }
}

/// A source that produces frames for the display.
pub trait FrameSource: Send {
    fn render(&mut self, sensors: &SensorData) -> Result<RawFrame>;
    fn name(&self) -> &str;
    /// Hot-swap the template. Default no-op for frame sources that don't use templates.
    fn set_template(&mut self, _template: &str) {}
    /// Set or clear the global background image. Default no-op — only SvgRenderer overrides.
    fn set_background(&mut self, _bg: Option<Arc<BackgroundImage>>) -> anyhow::Result<()> {
        Ok(())
    }
    /// Returns true when this source is an Xvfb capture stream.
    /// Used by the tick loop to decide whether to dump the last frame to tmpfs
    /// for GUI preview.  Default false — only XvfbSource overrides.
    fn is_streaming(&self) -> bool {
        false
    }
}

/// Renders HTML/CSS templates with sensor data substitution.
pub struct TemplateRenderer {
    template: String,
    width: u32,
    height: u32,
    render_width: u32,
    render_height: u32,
}

fn template_canvas_dimensions(template: &str, width: u32, height: u32) -> (u32, u32) {
    match frontmatter::LayoutFrontmatter::parse(template).canvas {
        Some(frontmatter::CanvasMode::Fixed { width, height }) => (width, height),
        Some(frontmatter::CanvasMode::Responsive) => (width, height),
        None => (480, 480),
    }
}

fn contain_pixmap(source: &Pixmap, width: u32, height: u32) -> Result<Pixmap> {
    let mut output = Pixmap::new(width, height)
        .ok_or_else(|| anyhow::anyhow!("invalid target canvas {width}x{height}"))?;
    output.fill(tiny_skia::Color::from_rgba8(8, 8, 15, 255));
    let scale = (width as f32 / source.width() as f32).min(height as f32 / source.height() as f32);
    let dx = (width as f32 - source.width() as f32 * scale) * 0.5;
    let dy = (height as f32 - source.height() as f32 * scale) * 0.5;
    let paint = tiny_skia::PixmapPaint {
        quality: tiny_skia::FilterQuality::Bicubic,
        ..Default::default()
    };
    output.draw_pixmap(
        0,
        0,
        source.as_ref(),
        &paint,
        tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, dx, dy),
        None,
    );
    Ok(output)
}

impl TemplateRenderer {
    pub fn new(template: &str, width: u32, height: u32) -> Result<Self> {
        let (render_width, render_height) = template_canvas_dimensions(template, width, height);
        Ok(Self {
            template: template.to_string(),
            width,
            height,
            render_width,
            render_height,
        })
    }

    pub fn set_template(&mut self, template: &str) {
        self.template = template.to_string();
        (self.render_width, self.render_height) =
            template_canvas_dimensions(template, self.width, self.height);
    }
}

impl FrameSource for TemplateRenderer {
    fn render(&mut self, sensors: &SensorData) -> Result<RawFrame> {
        // Step 1: Template substitution via tera
        let mut context = tera::Context::new();
        for (key, value) in sensors {
            context.insert(key, value);
        }
        context.insert("width", &self.render_width);
        context.insert("height", &self.render_height);
        let aspect = if self.render_height > 0 {
            f64::from(self.render_width) / f64::from(self.render_height)
        } else {
            1.0
        };
        context.insert("aspect", &aspect);
        if let Ok(shape) =
            crate::display_geometry::display_shape(self.render_width, self.render_height)
        {
            context.insert("shape", shape.as_str());
            context.insert(
                "is_portrait",
                &(shape == crate::display_geometry::DisplayShape::Portrait),
            );
            context.insert(
                "is_square",
                &(shape == crate::display_geometry::DisplayShape::Square),
            );
            context.insert(
                "is_landscape",
                &(shape == crate::display_geometry::DisplayShape::Landscape),
            );
            context.insert(
                "is_wide",
                &(shape == crate::display_geometry::DisplayShape::Wide),
            );
            context.insert(
                "is_ultrawide",
                &(shape == crate::display_geometry::DisplayShape::Ultrawide),
            );
        }
        for (key, value) in svg::responsive_tokens(self.render_width, self.render_height) {
            context.insert(key, &value);
        }
        let rendered = tera::Tera::one_off(&self.template, &context, false)?;

        // Step 2: Parse HTML
        let root = parser::parse_html(&rendered)?;

        // Step 3: Compute layout
        let nodes =
            layout::compute_layout(&root, self.render_width as f32, self.render_height as f32)?;

        // Step 4: Render at the declared canvas. Fixed canvases are uniformly
        // contained and centered into the native device resolution.
        let pixmap = draw::render_nodes(&nodes, self.render_width, self.render_height)?;
        let output = if self.render_width == self.width && self.render_height == self.height {
            pixmap
        } else {
            contain_pixmap(&pixmap, self.width, self.height)?
        };
        Ok(RawFrame::from_pixmap(&output))
    }

    fn name(&self) -> &str {
        "template"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb_at(frame: &RawFrame, x: u32, y: u32) -> [u8; 3] {
        let offset = ((y * frame.width + x) * 3) as usize;
        frame.data[offset..offset + 3].try_into().unwrap()
    }

    #[test]
    fn legacy_html_defaults_to_centered_480_square_canvas() {
        let template = r#"<div style="width: 480px; height: 480px; background: #ff0000;"></div>"#;
        let mut renderer = TemplateRenderer::new(template, 854, 480).unwrap();
        let frame = renderer.render(&SensorData::new()).unwrap();

        assert_eq!((frame.width, frame.height), (854, 480));
        assert_eq!(rgb_at(&frame, 100, 240), [8, 8, 15]);
        assert_eq!(rgb_at(&frame, 200, 240), [255, 0, 0]);
        assert_eq!(rgb_at(&frame, 650, 240), [255, 0, 0]);
        assert_eq!(rgb_at(&frame, 754, 240), [8, 8, 15]);
    }

    #[test]
    fn fixed_html_uses_logical_geometry_before_containing() {
        let template = r#"{# canvas: 320x240 #}
<div style="width: {{ width }}px; height: {{ height }}px; background: #0000ff;"></div>"#;
        let mut renderer = TemplateRenderer::new(template, 854, 480).unwrap();
        let frame = renderer.render(&SensorData::new()).unwrap();

        // 320x240 scales by 2 into a centered 640x480 rectangle.
        assert_eq!(rgb_at(&frame, 100, 240), [8, 8, 15]);
        assert_eq!(rgb_at(&frame, 120, 240), [0, 0, 255]);
        assert_eq!(rgb_at(&frame, 730, 240), [0, 0, 255]);
        assert_eq!(rgb_at(&frame, 754, 240), [8, 8, 15]);
    }
}
