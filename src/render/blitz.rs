// Blitz-based HTML/CSS renderer — full CSS support via Servo's Stylo engine.
// Renders HTML templates to 480x480 pixmaps using blitz-html + blitz-paint + vello_cpu.

use anyhow::Result;
use tiny_skia::Pixmap;

use anyrender::{PaintScene as _, render_to_buffer};
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::HtmlDocument;
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use peniko::Fill;
use peniko::kurbo::Rect;

use super::{FrameSource, RawFrame, SensorData, contain_pixmap, svg, template_canvas_dimensions};

/// Renders HTML/CSS layouts using Blitz (Stylo + Taffy + Vello CPU).
/// Supports the full CSS spec including border-radius, gradients, box-shadow, etc.
pub struct BlitzRenderer {
    template: String,
    width: u32,
    height: u32,
    render_width: u32,
    render_height: u32,
}

impl BlitzRenderer {
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

    /// Render HTML string (already template-substituted) to a tiny-skia Pixmap.
    fn render_html(&self, html: &str) -> Result<Pixmap> {
        let scale = 1.0_f32;
        let w = self.render_width;
        let h = self.render_height;

        // Parse HTML into a Blitz document
        let mut document = HtmlDocument::from_html(
            html,
            DocumentConfig {
                viewport: Some(Viewport::new(w, h, scale, ColorScheme::Dark)),
                ..Default::default()
            },
        );

        // Resolve styles and compute layout
        document.as_mut().resolve(0.0);

        let render_w = w;
        let render_h = h;

        // Render to RGBA buffer via vello_cpu
        let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
            |scene| {
                // Black background (matching our dark-themed layouts)
                scene.fill(
                    Fill::NonZero,
                    Default::default(),
                    peniko::Color::new([0.0, 0.0, 0.0, 1.0]),
                    Default::default(),
                    &Rect::new(0.0, 0.0, render_w as f64, render_h as f64),
                );

                // Paint the document
                paint_scene(scene, document.as_ref(), scale as f64, render_w, render_h);
            },
            render_w,
            render_h,
        );

        // Convert RGBA buffer to tiny-skia Pixmap
        let mut pixmap = Pixmap::new(w, h)
            .ok_or_else(|| anyhow::anyhow!("Failed to create {}x{} pixmap", w, h))?;

        let expected_len = (w * h * 4) as usize;
        if buffer.len() < expected_len {
            anyhow::bail!(
                "Blitz buffer too small: got {} bytes, expected {} ({}x{}x4)",
                buffer.len(),
                expected_len,
                w,
                h
            );
        }

        pixmap.data_mut().copy_from_slice(&buffer[..expected_len]);
        Ok(pixmap)
    }
}

impl FrameSource for BlitzRenderer {
    fn render(&mut self, sensors: &SensorData) -> Result<RawFrame> {
        // Step 1: Tera template substitution
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
        let html = tera::Tera::one_off(&self.template, &context, false)?;

        // Step 2: Render via Blitz
        let pixmap = self.render_html(&html)?;
        let output = if self.render_width == self.width && self.render_height == self.height {
            pixmap
        } else {
            contain_pixmap(&pixmap, self.width, self.height)?
        };
        Ok(RawFrame::from_pixmap(&output))
    }

    fn name(&self) -> &str {
        "blitz"
    }

    fn set_template(&mut self, template: &str) {
        self.template = template.to_string();
        (self.render_width, self.render_height) =
            template_canvas_dimensions(template, self.width, self.height);
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
    fn legacy_blitz_defaults_to_centered_fixed_480_canvas() {
        let template = r#"<div style="width:480px;height:480px;background:#ff0000"></div>"#;
        let mut renderer = BlitzRenderer::new(template, 854, 480).unwrap();
        let frame = renderer.render(&SensorData::new()).unwrap();

        assert_eq!((frame.width, frame.height), (854, 480));
        assert_eq!(rgb_at(&frame, 100, 240), [8, 8, 15]);
        assert_eq!(rgb_at(&frame, 240, 240), [255, 0, 0]);
        assert_eq!(rgb_at(&frame, 754, 240), [8, 8, 15]);
    }

    #[test]
    fn responsive_blitz_receives_native_geometry() {
        let template = r#"{# canvas: responsive #}
<div style="width:{{ width }}px;height:{{ height }}px;background:#0000ff"></div>"#;
        let mut renderer = BlitzRenderer::new(template, 854, 480).unwrap();
        let frame = renderer.render(&SensorData::new()).unwrap();

        assert_eq!((frame.width, frame.height), (854, 480));
        assert_eq!(rgb_at(&frame, 10, 240), [0, 0, 255]);
        assert_eq!(rgb_at(&frame, 843, 240), [0, 0, 255]);
    }
}
