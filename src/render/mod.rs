// Rendering pipeline: HTML/CSS template parsing, layout computation, and pixmap drawing.
// Converts HTML/CSS templates into 480x480 JPEG frames for the cooler LCD.

pub mod parser;
pub mod layout;
pub mod draw;

use std::collections::HashMap;
use anyhow::Result;
use tiny_skia::Pixmap;

/// Sensor data: flat map of key → string value.
pub type SensorData = HashMap<String, String>;

/// A source that produces frames for the display.
pub trait FrameSource: Send {
    fn render(&mut self, sensors: &SensorData) -> Result<Pixmap>;
    fn name(&self) -> &str;
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
    fn render(&mut self, sensors: &SensorData) -> Result<Pixmap> {
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
        let pixmap = draw::render_nodes(&nodes, self.width, self.height);

        Ok(pixmap)
    }

    fn name(&self) -> &str {
        "template"
    }
}
