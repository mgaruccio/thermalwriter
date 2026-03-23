// Blitz-based HTML/CSS renderer — full CSS support via Servo's Stylo engine.
// Renders HTML templates to 480x480 pixmaps using blitz-html + blitz-paint + vello_cpu.

use anyhow::Result;
use tiny_skia::Pixmap;

use anyrender::{render_to_buffer, PaintScene as _};
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::HtmlDocument;
use blitz_paint::paint_scene;
use blitz_traits::shell::{ColorScheme, Viewport};
use peniko::Fill;
use peniko::kurbo::Rect;

use super::{FrameSource, SensorData};

/// Renders HTML/CSS layouts using Blitz (Stylo + Taffy + Vello CPU).
/// Supports the full CSS spec including border-radius, gradients, box-shadow, etc.
pub struct BlitzRenderer {
    template: String,
    width: u32,
    height: u32,
}

impl BlitzRenderer {
    pub fn new(template: &str, width: u32, height: u32) -> Result<Self> {
        Ok(Self {
            template: template.to_string(),
            width,
            height,
        })
    }

    /// Render HTML string (already template-substituted) to a tiny-skia Pixmap.
    fn render_html(&self, html: &str) -> Result<Pixmap> {
        let scale = 1.0_f32;
        let w = self.width;
        let h = self.height;

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
                paint_scene(
                    scene,
                    document.as_ref(),
                    scale as f64,
                    render_w,
                    render_h,
                );
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
    fn render(&mut self, sensors: &SensorData) -> Result<Pixmap> {
        // Step 1: Tera template substitution
        let mut context = tera::Context::new();
        for (key, value) in sensors {
            context.insert(key, value);
        }
        let html = tera::Tera::one_off(&self.template, &context, false)?;

        // Step 2: Render via Blitz
        self.render_html(&html)
    }

    fn name(&self) -> &str {
        "blitz"
    }

    fn set_template(&mut self, template: &str) {
        self.template = template.to_string();
    }
}
