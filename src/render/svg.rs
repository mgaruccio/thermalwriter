// SVG rendering: uses Tera for template substitution and resvg for rasterization.
// Renders SVG templates with sensor data into 480x480 pixmaps for the cooler LCD.

use anyhow::{Context, Result};
use resvg::usvg;
use tiny_skia::{Pixmap, Transform};

use super::{FrameSource, SensorData};

// Font file is named JetBrainsMono but is actually DejaVu Sans Mono
const EMBEDDED_FONT: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");
const EMBEDDED_FONT_FAMILY: &str = "DejaVu Sans Mono";

/// Renders SVG templates with sensor data substitution via Tera + resvg.
pub struct SvgRenderer<'a> {
    template: String,
    width: u32,
    height: u32,
    options: usvg::Options<'a>,
}

impl<'a> SvgRenderer<'a> {
    pub fn new(template: &str, width: u32, height: u32) -> Result<Self> {
        let mut options = usvg::Options::default();
        // Set the embedded font as default for all text
        options.font_family = EMBEDDED_FONT_FAMILY.to_string();
        // Load the embedded monospace font so SVG <text> elements render
        options.fontdb_mut().load_font_data(EMBEDDED_FONT.to_vec());
        options.fontdb_mut().load_system_fonts();
        // Map the CSS "monospace" generic family to our embedded font
        options.fontdb_mut().set_monospace_family(EMBEDDED_FONT_FAMILY);

        Ok(Self {
            template: template.to_string(),
            width,
            height,
            options,
        })
    }
}

impl FrameSource for SvgRenderer<'static> {
    fn render(&mut self, sensors: &SensorData) -> Result<Pixmap> {
        // Step 1: Tera template substitution
        let mut context = tera::Context::new();
        for (key, value) in sensors {
            context.insert(key, value);
        }
        let svg_string = tera::Tera::one_off(&self.template, &context, false)
            .context("Tera template substitution failed")?;

        // Step 2: Parse SVG with usvg
        let tree = usvg::Tree::from_str(&svg_string, &self.options)
            .context("Failed to parse SVG")?;

        // Step 3: Render to pixmap at target size
        let mut pixmap = Pixmap::new(self.width, self.height)
            .context("Failed to create pixmap")?;

        // Scale the SVG to fit the target canvas
        let svg_size = tree.size();
        let sx = self.width as f32 / svg_size.width();
        let sy = self.height as f32 / svg_size.height();
        let transform = Transform::from_scale(sx, sy);

        resvg::render(&tree, transform, &mut pixmap.as_mut());

        Ok(pixmap)
    }

    fn name(&self) -> &str {
        "svg"
    }

    fn set_template(&mut self, template: &str) {
        self.template = template.to_string();
    }
}
