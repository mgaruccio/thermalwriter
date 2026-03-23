// SVG rendering: uses Tera for template substitution and resvg for rasterization.
// Renders SVG templates with sensor data into 480x480 pixmaps for the cooler LCD.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use resvg::usvg;
use tera::Tera;
use tiny_skia::{Pixmap, Transform};

use crate::sensor::history::SensorHistory;
use crate::theme::ThemePalette;
use super::{FrameSource, SensorData};

// Font file is named JetBrainsMono but is actually DejaVu Sans Mono
const EMBEDDED_FONT: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");
const EMBEDDED_FONT_FAMILY: &str = "DejaVu Sans Mono";

/// Number of history samples to inject per metric (60 ≈ 30s at 2FPS).
const DEFAULT_HISTORY_SAMPLE_COUNT: usize = 60;

/// Renders SVG templates with sensor data substitution via Tera + resvg.
pub struct SvgRenderer<'a> {
    tera: Tera,
    template_name: String,
    width: u32,
    height: u32,
    options: usvg::Options<'a>,
    history: Option<Arc<Mutex<SensorHistory>>>,
    theme: Option<ThemePalette>,
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

        let mut tera = Tera::default();
        tera.autoescape_on(vec![]); // Disable autoescaping for SVG
        super::components::register_all(&mut tera);
        tera.add_raw_template("layout", template)
            .context("Failed to add template to Tera")?;

        Ok(Self {
            tera,
            template_name: "layout".to_string(),
            width,
            height,
            options,
            history: None,
            theme: None,
        })
    }

    /// Set the sensor history source for context injection.
    pub fn set_history(&mut self, history: Arc<Mutex<SensorHistory>>) {
        self.history = Some(history);
    }

    /// Set the theme palette for context injection.
    pub fn set_theme(&mut self, theme: ThemePalette) {
        self.theme = Some(theme);
    }
}

impl FrameSource for SvgRenderer<'static> {
    fn render(&mut self, sensors: &SensorData) -> Result<Pixmap> {
        // Step 1: Build Tera context from sensors
        let mut context = tera::Context::new();
        for (key, value) in sensors {
            context.insert(key, value);
        }

        // Inject theme colors if configured
        if let Some(ref theme) = self.theme {
            theme.inject_into_context(&mut context);
        }

        // Inject history arrays if configured
        if let Some(ref history) = self.history {
            if let Ok(hist) = history.lock() {
                hist.inject_into_context(&mut context, DEFAULT_HISTORY_SAMPLE_COUNT);
            }
        }

        // Step 2: Tera template substitution
        let svg_string = self.tera.render(&self.template_name, &context)
            .context("Tera template substitution failed")?;

        // Step 3: Parse SVG with usvg
        let tree = usvg::Tree::from_str(&svg_string, &self.options)
            .context("Failed to parse SVG")?;

        // Step 4: Render to pixmap at target size
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
        // Re-add template to the persistent Tera instance
        let _ = self.tera.add_raw_template(&self.template_name, template);
    }
}
