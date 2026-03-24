// Rendering pipeline: HTML/CSS template parsing, layout computation, and pixmap drawing.
// Converts HTML/CSS templates into 480x480 JPEG frames for the cooler LCD.

pub mod parser;
pub mod layout;
pub mod draw;
pub mod svg;
pub mod components;
pub mod frontmatter;

#[cfg(feature = "blitz")]
pub mod blitz;

use std::collections::HashMap;
use anyhow::Result;
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
        let img: ImageBuffer<Rgb<u8>, _> = ImageBuffer::from_raw(
            self.width, self.height, self.data.clone()
        ).ok_or_else(|| anyhow::anyhow!("Failed to create image buffer"))?;
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
}

/// Renders HTML/CSS templates with sensor data substitution.
pub struct TemplateRenderer {
    template: String,
    width: u32,
    height: u32,
}

impl TemplateRenderer {
    pub fn new(template: &str, width: u32, height: u32) -> Result<Self> {
        Ok(Self {
            template: template.to_string(),
            width,
            height,
        })
    }

    pub fn set_template(&mut self, template: &str) {
        self.template = template.to_string();
    }
}

impl FrameSource for TemplateRenderer {
    fn render(&mut self, sensors: &SensorData) -> Result<RawFrame> {
        // Step 1: Template substitution via tera
        let mut context = tera::Context::new();
        for (key, value) in sensors {
            context.insert(key, value);
        }
        let rendered = tera::Tera::one_off(&self.template, &context, false)?;

        // Step 2: Parse HTML
        let root = parser::parse_html(&rendered)?;

        // Step 3: Compute layout
        let nodes = layout::compute_layout(&root, self.width as f32, self.height as f32)?;

        // Step 4: Render to pixmap
        let pixmap = draw::render_nodes(&nodes, self.width, self.height)?;
        Ok(RawFrame::from_pixmap(&pixmap))
    }

    fn name(&self) -> &str {
        "template"
    }
}
