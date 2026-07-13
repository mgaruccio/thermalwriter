// SVG rendering: uses Tera for template substitution and resvg for rasterization.
// Renders SVG templates with sensor data into 480x480 pixmaps for the cooler LCD.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result};
use resvg::usvg;
use tera::Tera;
use tiny_skia::{Color, Pixmap, PixmapPaint, Transform};

use super::background::BackgroundImage;
use super::frontmatter::{CanvasMode, LayoutFrontmatter};
use super::{FrameSource, RawFrame, SensorData};
use crate::sensor::history::SensorHistory;
use crate::theme::ThemePalette;

// Font file is named JetBrainsMono but is actually DejaVu Sans Mono
const EMBEDDED_FONT: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");
const EMBEDDED_FONT_FAMILY: &str = "DejaVu Sans Mono";

// Shared fontdb — system font scan runs once at startup; all SvgRenderer instances share
// the same Arc<Database>. Construction is a pure refcount bump — no Database::clone.
//
// Two fontdb variants are cached: slim (embedded font only, ~400KB RSS) and full
// (embedded + system fonts, ~3MB RSS). The slim variant is used when the template
// only references the embedded font or generic families that map to it. Custom
// layouts with system fonts automatically get the full fontdb.
static SLIM_FONTDB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
static FULL_FONTDB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();

fn slim_fontdb() -> Arc<usvg::fontdb::Database> {
    Arc::clone(SLIM_FONTDB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_font_data(EMBEDDED_FONT.to_vec());
        db.set_monospace_family(EMBEDDED_FONT_FAMILY);
        Arc::new(db)
    }))
}

fn full_fontdb() -> Arc<usvg::fontdb::Database> {
    Arc::clone(FULL_FONTDB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_font_data(EMBEDDED_FONT.to_vec());
        db.load_system_fonts();
        db.set_monospace_family(EMBEDDED_FONT_FAMILY);
        Arc::new(db)
    }))
}

/// Scan a template for font-family references. Returns true if any font-family
/// value is NOT the embedded font or "monospace". Conservative: if parsing fails
/// or unknown syntax is encountered, returns true (load system fonts).
fn template_needs_system_fonts(template: &str) -> bool {
    // Only these two font families are available without system fonts:
    // - "DejaVu Sans Mono": the embedded font
    // - "monospace": mapped to the embedded font via set_monospace_family
    // All other generics (serif, sans-serif, cursive, fantasy, system-ui) require
    // system fonts because we don't map them to the embedded font.
    fn is_known_family(family: &str) -> bool {
        let family = family.trim();
        family.eq_ignore_ascii_case(EMBEDDED_FONT_FAMILY)
            || family.eq_ignore_ascii_case("monospace")
    }

    // Check a font-family value (comma-separated list)
    fn check_font_list(value: &str) -> bool {
        for family in value.split(',') {
            let family = family.trim().trim_matches('"').trim_matches('\'').trim();
            if family.is_empty() {
                continue;
            }
            // If it contains a Tera variable, conservatively assume it needs system fonts
            if family.contains("{{") || family.contains("}}") {
                return true;
            }
            if !is_known_family(family) {
                return true;
            }
        }
        false
    }

    // Conservative: scan for ALL occurrences of "font-family" (case-insensitive).
    // For each occurrence:
    // - If followed by '=' (with optional whitespace), parse the attribute value
    // - Otherwise (CSS in <style> blocks, unknown syntax), return true
    let template_lower = template.to_ascii_lowercase();
    let mut search_start = 0;

    while let Some(pos) = template_lower[search_start..].find("font-family") {
        let abs_pos = search_start + pos;
        let after = &template[abs_pos + 11..]; // Skip "font-family"
        let after_trimmed = after.trim_start();

        if let Some(rest) = after_trimmed.strip_prefix('=') {
            // Attribute form: font-family="..." or font-family='...'
            let rest = rest.trim_start();
            if let Some(quoted) = rest.strip_prefix('"') {
                if let Some(end) = quoted.find('"') {
                    if check_font_list(&quoted[..end]) {
                        return true;
                    }
                } else {
                    // Unclosed quote
                    return true;
                }
            } else if let Some(quoted) = rest.strip_prefix('\'') {
                if let Some(end) = quoted.find('\'') {
                    if check_font_list(&quoted[..end]) {
                        return true;
                    }
                } else {
                    return true;
                }
            } else {
                // Unquoted or unknown syntax
                return true;
            }
        } else if let Some(raw_value) = after_trimmed.strip_prefix(':') {
            // CSS property form: font-family: ... (in <style> blocks or inline style attributes).
            let raw_value = raw_value.trim_start();
            // Extract until the CSS declaration ends. For inline style attributes
            // without a trailing semicolon, quote/< boundaries stop before the
            // surrounding SVG markup. Quotes at the start of a family item are CSS
            // quotes and do not end the whole font-family list.
            let mut end = raw_value.len();
            let mut quoted_family = None;
            for (idx, ch) in raw_value.char_indices() {
                if let Some(quote) = quoted_family {
                    if ch == quote {
                        quoted_family = None;
                    }
                    continue;
                }

                match ch {
                    '"' | '\'' => {
                        let before = raw_value[..idx].trim_end();
                        if before.is_empty() || before.ends_with(',') {
                            quoted_family = Some(ch);
                        } else {
                            end = idx;
                            break;
                        }
                    }
                    ';' | '}' | '<' => {
                        end = idx;
                        break;
                    }
                    _ => {}
                }
            }
            if quoted_family.is_some() {
                return true;
            }

            let value = raw_value[..end].trim();
            if value.is_empty() && !raw_value.is_empty() {
                return true;
            }
            if check_font_list(value) {
                return true;
            }
        } else {
            // font-family not followed by = or :, unknown context (e.g., in a comment)
            // Conservatively return true
            return true;
        }

        search_start = abs_pos + 11; // Move past this "font-family"
    }

    false
}

fn shared_fontdb_for_template(template: &str) -> Arc<usvg::fontdb::Database> {
    if template_needs_system_fonts(template) {
        full_fontdb()
    } else {
        slim_fontdb()
    }
}

fn options_for_template(template: &str) -> usvg::Options<'static> {
    usvg::Options {
        font_family: EMBEDDED_FONT_FAMILY.to_string(),
        // Use slim fontdb (no system fonts) when the template only references
        // the embedded font. Custom layouts with system fonts get the full fontdb.
        fontdb: shared_fontdb_for_template(template),
        ..Default::default()
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Number of history samples to inject per metric (60 ≈ 30s at 2FPS).
const DEFAULT_HISTORY_SAMPLE_COUNT: usize = 60;
const DEFAULT_THEME_BACKGROUND: &str = "#08080f";

/// Renders SVG templates with sensor data substitution via Tera + resvg.
pub struct SvgRenderer<'a> {
    tera: Tera,
    template_name: String,
    width: u32,
    height: u32,
    logical_width: u32,
    logical_height: u32,
    options: usvg::Options<'a>,
    history: Option<Arc<Mutex<SensorHistory>>>,
    theme: Option<ThemePalette>,
    variable_defaults: HashMap<String, String>,
    variable_overrides: HashMap<String, String>,
    background_source: Option<Arc<BackgroundImage>>,
    background: Option<Arc<Pixmap>>,
}

fn logical_canvas_dimensions(
    frontmatter: &LayoutFrontmatter,
    width: u32,
    height: u32,
) -> (u32, u32) {
    match frontmatter.canvas {
        Some(CanvasMode::Responsive) => (width, height),
        Some(CanvasMode::Fixed { width, height }) => (width, height),
        None => (480, 480),
    }
}

impl<'a> SvgRenderer<'a> {
    pub fn new(template: &str, width: u32, height: u32) -> Result<Self> {
        let options = options_for_template(template);

        let mut tera = Tera::default();
        tera.autoescape_on(vec![]); // Disable autoescaping for SVG
        super::components::register_all(&mut tera);
        tera.add_raw_template("layout", template)
            .context("Failed to add template to Tera")?;

        let frontmatter = LayoutFrontmatter::parse(template);
        let (logical_width, logical_height) =
            logical_canvas_dimensions(&frontmatter, width, height);
        let variable_defaults = frontmatter
            .variables
            .iter()
            .map(|(name, decl)| (name.clone(), decl.default.clone()))
            .collect();

        Ok(Self {
            tera,
            template_name: "layout".to_string(),
            width,
            height,
            logical_width,
            logical_height,
            options,
            history: None,
            theme: None,
            variable_defaults,
            variable_overrides: HashMap::new(),
            background_source: None,
            background: None,
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

    /// Set per-layout variable overrides. These are injected after frontmatter
    /// defaults and theme values, so user overrides win.
    pub fn set_layout_vars(&mut self, vars: HashMap<String, String>) {
        self.variable_overrides = vars;
    }

    /// Set or clear the global background image.
    /// Rasterizes once to this renderer's dimensions (centered cover) and caches
    /// the premultiplied pixmap. Failures leave prior background state unchanged.
    pub fn set_background(&mut self, bg: Option<Arc<BackgroundImage>>) -> anyhow::Result<()> {
        let unchanged = match (&self.background_source, &bg) {
            (None, None) => true,
            (Some(current), Some(next)) => Arc::ptr_eq(current, next),
            _ => false,
        };
        if unchanged {
            return Ok(());
        }

        match bg {
            None => {
                self.background_source = None;
                self.background = None;
                Ok(())
            }
            Some(src) => {
                let pixmap = src.to_pixmap(self.width, self.height)?;
                self.background_source = Some(src);
                self.background = Some(Arc::new(pixmap));
                Ok(())
            }
        }
    }
    fn resolved_theme_background_value(&self) -> &str {
        self.variable_overrides
            .get("theme_background")
            .or_else(|| self.variable_defaults.get("theme_background"))
            .or_else(|| self.theme.as_ref().map(|theme| &theme.background))
            .map(String::as_str)
            .unwrap_or(DEFAULT_THEME_BACKGROUND)
    }

    fn fallback_background_color(&self) -> Color {
        parse_hex_color(self.resolved_theme_background_value()).unwrap_or_else(|| {
            parse_hex_color(DEFAULT_THEME_BACKGROUND).expect("valid fallback color")
        })
    }
}

fn parse_hex_color(value: &str) -> Option<Color> {
    let hex = value.trim().strip_prefix('#')?;
    if hex.len() != 6 && hex.len() != 8 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    let a = if hex.len() == 8 {
        u8::from_str_radix(&hex[6..8], 16).ok()?
    } else {
        255
    };
    Some(Color::from_rgba8(r, g, b, a))
}

// ---------------------------------------------------------------------------
// Render pipeline sub-stages, extracted so `benches/` can time each in
// isolation. Hidden from public docs — internal to the crate's bench/example
// surface, not a supported API. `SvgRenderer::render` below is a composition
// of these; behavior must stay byte-identical to the pre-extraction version.
// ---------------------------------------------------------------------------

/// Nearest-round checked responsive design tokens from the short axis.
pub fn responsive_tokens(width: u32, height: u32) -> HashMap<&'static str, u32> {
    let short = u64::from(width.min(height));
    let nearest = |num: u64, den: u64, floor: u32| -> u32 {
        let v = (short * num + den / 2) / den;
        (v as u32).max(floor)
    };
    let mut m = HashMap::new();
    m.insert("token_margin", ((short + 15) / 30).max(8) as u32);
    m.insert("token_gap", ((short + 20) / 40).max(6) as u32);
    m.insert("token_label", nearest(14, 480, 10));
    m.insert("token_small", nearest(16, 480, 12));
    m.insert("token_medium", nearest(24, 480, 18));
    m.insert("token_hero", nearest(64, 480, 40));
    m
}

impl<'a> SvgRenderer<'a> {
    /// Build the Tera context from sensor data, frontmatter defaults, theme,
    /// variable overrides, and history — in the layering order layouts rely on.
    #[doc(hidden)]
    pub fn build_context(&self, sensors: &SensorData) -> tera::Context {
        let mut context = tera::Context::new();
        for (key, value) in sensors {
            context.insert(key, &xml_escape(value));
        }

        // Inject variable defaults declared by the layout frontmatter.
        for (key, value) in &self.variable_defaults {
            context.insert(key, &xml_escape(value));
        }

        // Inject theme colors if configured
        if let Some(ref theme) = self.theme {
            theme.inject_into_context(&mut context);
        }

        // Keep theme_background aligned with the no-image fallback fill:
        // override -> frontmatter default -> theme palette -> hard fallback.
        let resolved = self.resolved_theme_background_value();
        let theme_background_context = if parse_hex_color(resolved).is_some() {
            resolved
        } else {
            DEFAULT_THEME_BACKGROUND
        };
        context.insert("theme_background", &xml_escape(theme_background_context));

        // Inject user overrides last so saved GUI choices win. theme_background
        // was inserted above after resolving/sanitizing the fallback color.
        for (key, value) in &self.variable_overrides {
            if key != "theme_background" {
                context.insert(key, &xml_escape(value));
            }
        }

        // Inject history arrays if configured
        if let Some(ref history) = self.history
            && let Ok(hist) = history.lock()
        {
            hist.inject_into_context(&mut context, DEFAULT_HISTORY_SAMPLE_COUNT);
        }

        // Canvas geometry is reserved runtime state. Insert it last so sensor
        // names, frontmatter variables, saved overrides, and history keys
        // cannot corrupt responsive layout dimensions.
        context.insert("width", &self.logical_width);
        context.insert("height", &self.logical_height);
        let aspect = if self.logical_height > 0 {
            f64::from(self.logical_width) / f64::from(self.logical_height)
        } else {
            1.0
        };
        context.insert("aspect", &aspect);
        if let Ok(shape) =
            crate::display_geometry::display_shape(self.logical_width, self.logical_height)
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
        for (key, value) in responsive_tokens(self.logical_width, self.logical_height) {
            context.insert(key, &value);
        }

        context
    }

    /// Tera template substitution against a pre-built context.
    #[doc(hidden)]
    pub fn render_template(&self, context: &tera::Context) -> Result<String> {
        self.tera
            .render(&self.template_name, context)
            .context("Tera template substitution failed")
    }

    /// The usvg parse options (embedded font family + shared fontdb) used by
    /// `render()`. Exposed so benches can call `parse_svg` with the same
    /// options the real pipeline uses.
    #[doc(hidden)]
    pub fn options(&self) -> &usvg::Options<'a> {
        &self.options
    }
}

/// Parse the substituted SVG string into a `usvg::Tree`.
#[doc(hidden)]
pub fn parse_svg(svg_string: &str, options: &usvg::Options) -> Result<usvg::Tree> {
    usvg::Tree::from_str(svg_string, options).context("Failed to parse SVG")
}

/// Rasterize a parsed SVG tree, scaled to fit the target canvas.
#[doc(hidden)]
pub fn rasterize(tree: &usvg::Tree, width: u32, height: u32) -> Result<Pixmap> {
    let mut layout_pixmap = Pixmap::new(width, height).context("Failed to create pixmap")?;

    let svg_size = tree.size();
    // Uniform contain: scale by min axis, center (never distort/crop).
    let sx = width as f32 / svg_size.width();
    let sy = height as f32 / svg_size.height();
    let scale = sx.min(sy);
    let dx = (width as f32 - svg_size.width() * scale) * 0.5;
    let dy = (height as f32 - svg_size.height() * scale) * 0.5;
    let transform = Transform::from_row(scale, 0.0, 0.0, scale, dx, dy);

    resvg::render(tree, transform, &mut layout_pixmap.as_mut());
    Ok(layout_pixmap)
}

/// Composite the rasterized layout over the background image (if set) or a
/// flat fallback fill.
#[doc(hidden)]
pub fn composite(
    layout_pixmap: &Pixmap,
    background: Option<&Pixmap>,
    width: u32,
    height: u32,
    fallback_color: Color,
) -> Result<Pixmap> {
    let mut composed = if let Some(bg) = background {
        if bg.width() != width || bg.height() != height {
            anyhow::bail!(
                "background dimensions {}x{} do not match canvas {}x{}",
                bg.width(),
                bg.height(),
                width,
                height
            );
        }
        bg.clone()
    } else {
        let mut fill = Pixmap::new(width, height).context("Failed to create pixmap")?;
        fill.fill(fallback_color);
        fill
    };
    composed.draw_pixmap(
        0,
        0,
        layout_pixmap.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    Ok(composed)
}

impl FrameSource for SvgRenderer<'static> {
    fn render(&mut self, sensors: &SensorData) -> Result<RawFrame> {
        let context = self.build_context(sensors);
        let svg_string = self.render_template(&context)?;
        let tree = parse_svg(&svg_string, &self.options)?;
        let layout_pixmap = rasterize(&tree, self.width, self.height)?;
        let final_pixmap = composite(
            &layout_pixmap,
            self.background.as_deref(),
            self.width,
            self.height,
            self.fallback_background_color(),
        )?;

        Ok(RawFrame::from_pixmap(&final_pixmap))
    }

    fn name(&self) -> &str {
        "svg"
    }

    fn set_template(&mut self, template: &str) {
        // Re-add template to the persistent Tera instance. Keep the active options
        // and defaults unchanged if Tera rejects the replacement template.
        if let Err(e) = self.tera.add_raw_template(&self.template_name, template) {
            log::warn!("Failed to update template: {}", e);
            return;
        }

        self.options = options_for_template(template);
        let frontmatter = LayoutFrontmatter::parse(template);
        (self.logical_width, self.logical_height) =
            logical_canvas_dimensions(&frontmatter, self.width, self.height);
        self.variable_defaults = frontmatter
            .variables
            .iter()
            .map(|(name, decl)| (name.clone(), decl.default.clone()))
            .collect();
    }

    fn set_background(&mut self, bg: Option<Arc<BackgroundImage>>) -> anyhow::Result<()> {
        SvgRenderer::set_background(self, bg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_layout_uses_slim_fontdb() {
        // Built-in layouts only reference "DejaVu Sans Mono, monospace"
        let template = r#"<svg><text font-family="DejaVu Sans Mono, monospace">Test</text></svg>"#;
        assert!(
            !template_needs_system_fonts(template),
            "builtin layout should use slim fontdb"
        );
    }

    #[test]
    fn monospace_only_uses_slim_fontdb() {
        let template = r#"<svg><text font-family="monospace">Test</text></svg>"#;
        assert!(
            !template_needs_system_fonts(template),
            "monospace-only should use slim fontdb"
        );
    }

    #[test]
    fn arial_triggers_full_fontdb() {
        let template = r#"<svg><text font-family="Arial">Test</text></svg>"#;
        assert!(
            template_needs_system_fonts(template),
            "Arial should trigger full fontdb"
        );
    }

    #[test]
    fn style_attribute_triggers_full_fontdb() {
        let template = r#"<svg><text style="font-family: Arial">Test</text></svg>"#;
        assert!(
            template_needs_system_fonts(template),
            "style attribute with Arial should trigger full fontdb"
        );
    }

    #[test]
    fn style_attribute_quoted_arial_triggers_full_fontdb() {
        let template = r#"<svg><text style="font-family: 'Arial'">Test</text></svg>"#;
        assert!(
            template_needs_system_fonts(template),
            "quoted style attribute with Arial should trigger full fontdb"
        );
    }

    #[test]
    fn style_attribute_quoted_known_then_arial_triggers_full_fontdb() {
        let template =
            r#"<svg><text style="font-family: 'DejaVu Sans Mono', Arial">Test</text></svg>"#;
        assert!(
            template_needs_system_fonts(template),
            "quoted embedded font followed by Arial should trigger full fontdb"
        );
    }

    #[test]
    fn inline_style_embedded_font_without_semicolon_uses_slim_fontdb() {
        let template = r#"<svg><text style="font-family: DejaVu Sans Mono">Test</text></svg>"#;
        assert!(
            !template_needs_system_fonts(template),
            "inline style with embedded font should use slim fontdb"
        );
    }

    #[test]
    fn inline_style_monospace_without_semicolon_uses_slim_fontdb() {
        let template = r#"<svg><text style="font-family: monospace">Test</text></svg>"#;
        assert!(
            !template_needs_system_fonts(template),
            "inline style with monospace should use slim fontdb"
        );
    }

    #[test]
    fn style_block_triggers_full_fontdb() {
        let template = r#"<svg><style>text { font-family: Arial; }</style><text>Test</text></svg>"#;
        assert!(
            template_needs_system_fonts(template),
            "<style> block with Arial should trigger full fontdb"
        );
    }

    #[test]
    fn tera_variable_triggers_full_fontdb() {
        let template = r#"<svg><text font-family="{{ custom_font }}">Test</text></svg>"#;
        assert!(
            template_needs_system_fonts(template),
            "Tera variable should conservatively trigger full fontdb"
        );
    }

    #[test]
    fn mixed_known_and_unknown_triggers_full_fontdb() {
        let template = r#"<svg><text font-family="DejaVu Sans Mono, Arial">Test</text></svg>"#;
        assert!(
            template_needs_system_fonts(template),
            "mixed known+unknown should trigger full fontdb"
        );
    }

    #[test]
    fn case_insensitive_matching() {
        let template = r#"<svg><text font-family="DEJAVU SANS MONO">Test</text></svg>"#;
        assert!(
            !template_needs_system_fonts(template),
            "case-insensitive match should use slim fontdb"
        );
    }

    #[test]
    fn whitespace_tolerance() {
        let template = r#"<svg><text font-family = "DejaVu Sans Mono">Test</text></svg>"#;
        assert!(
            !template_needs_system_fonts(template),
            "whitespace around = should be tolerated"
        );
    }

    #[test]
    fn responsive_tokens_match_canonical_480_axis() {
        let tokens = responsive_tokens(854, 480);
        assert_eq!(tokens["token_margin"], 16);
        assert_eq!(tokens["token_gap"], 12);
        assert_eq!(tokens["token_label"], 14);
        assert_eq!(tokens["token_small"], 16);
        assert_eq!(tokens["token_medium"], 24);
        assert_eq!(tokens["token_hero"], 64);
    }

    #[test]
    fn neon_dash_v2_uses_short_axis_for_compact_typography() {
        let render = |width, height| {
            let template = include_str!("../../layouts/svg/neon-dash-v2.svg");
            let mut renderer =
                SvgRenderer::new(template, width, height).expect("valid neon dash v2 SVG template");
            renderer.set_theme(ThemePalette::default());
            let mut history = SensorHistory::new();
            history.configure_metric("cpu_temp", std::time::Duration::from_secs(60));
            history.configure_metric("gpu_temp", std::time::Duration::from_secs(60));
            history.configure_metric("ram_used", std::time::Duration::from_secs(120));
            renderer.set_history(Arc::new(Mutex::new(history)));
            let context = renderer.build_context(&SensorData::new());
            renderer
                .render_template(&context)
                .expect("neon dash v2 template renders")
        };

        let compact = render(960, 320);
        assert_eq!(compact.matches(r#"font-size="20""#).count(), 5, "{compact}");

        let compact_short_wide = render(640, 172);
        assert_eq!(
            compact_short_wide.matches(r#"font-size="20""#).count(),
            5,
            "{compact_short_wide}"
        );

        let canonical = render(854, 480);
        let panel_14 = [
            r#"x="36" y="46" font-family="DejaVu Sans Mono, monospace" font-size="14""#,
            r#"x="36" y="230" font-family="DejaVu Sans Mono, monospace" font-size="14""#,
        ]
        .iter()
        .map(|needle| canonical.matches(needle).count())
        .sum::<usize>();
        assert_eq!(panel_14, 2, "{canonical}");
        assert_eq!(
            canonical
                .matches(r#"y="452" font-family="DejaVu Sans Mono, monospace" font-size="16""#)
                .count(),
            3,
            "{canonical}"
        );
    }

    #[test]
    fn runtime_geometry_cannot_be_overridden_by_layout_variables() {
        let template = r#"{# canvas: responsive #}
{# vars:
width: number = "1" "reserved collision"
token_hero: number = "1" "reserved collision"
#}
<svg xmlns="http://www.w3.org/2000/svg" width="{{ width }}" height="{{ height }}">
  <text>{{ token_hero }}</text>
</svg>"#;
        let mut renderer = SvgRenderer::new(template, 854, 480).expect("valid SVG template");
        renderer.set_layout_vars(HashMap::from([
            ("width".to_string(), "2".to_string()),
            ("token_hero".to_string(), "2".to_string()),
        ]));

        let rendered = renderer
            .render_template(&renderer.build_context(&SensorData::new()))
            .unwrap();
        assert!(rendered.contains(r#"width="854""#), "{rendered}");
        assert!(rendered.contains(r#"height="480""#), "{rendered}");
        assert!(rendered.contains(">64</text>"), "{rendered}");
    }

    #[test]
    fn fixed_svg_uses_declared_logical_geometry_before_containing() {
        let template = r##"{# canvas: 320x240 #}
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {{ width }} {{ height }}"
     width="{{ width }}" height="{{ height }}">
  <rect width="{{ width }}" height="{{ height }}" fill="#0000ff"/>
</svg>"##;
        let mut renderer = SvgRenderer::new(template, 854, 480).expect("valid SVG template");
        let context = renderer.build_context(&SensorData::new());
        let rendered = renderer.render_template(&context).unwrap();
        assert!(rendered.contains(r#"viewBox="0 0 320 240""#), "{rendered}");
        assert!(rendered.contains(r#"width="320""#), "{rendered}");
        assert!(rendered.contains(r#"height="240""#), "{rendered}");

        let frame = renderer.render(&SensorData::new()).unwrap();
        assert_eq!((frame.width, frame.height), (854, 480));
    }

    #[test]
    fn setting_same_background_source_reuses_rasterized_pixmap() {
        let mut renderer =
            SvgRenderer::new("<svg xmlns=\"http://www.w3.org/2000/svg\"/>", 480, 480)
                .expect("valid SVG");
        let source = Arc::new(
            BackgroundImage::decode(include_bytes!("../../assets/backgrounds/dark-solid.png"))
                .expect("valid background fixture"),
        );

        renderer
            .set_background(Some(Arc::clone(&source)))
            .expect("initial background rasterization");
        let initial = Arc::clone(
            renderer
                .background
                .as_ref()
                .expect("rasterized background should be cached"),
        );

        renderer
            .set_background(Some(source))
            .expect("unchanged background should be accepted");
        let repeated = renderer
            .background
            .as_ref()
            .expect("rasterized background should remain cached");

        assert!(
            Arc::ptr_eq(&initial, repeated),
            "unchanged source must not replace the rasterized background"
        );
    }

    #[test]
    fn set_template_recomputes_fontdb_for_new_template() {
        let initial_template =
            r#"<svg><text font-family="DejaVu Sans Mono, monospace">Test</text></svg>"#;
        let mut renderer = SvgRenderer::new(initial_template, 480, 480).expect("valid SVG");
        assert!(
            Arc::ptr_eq(&renderer.options().fontdb, &slim_fontdb()),
            "initial embedded-font template should use slim fontdb"
        );

        let replacement_template = r#"<svg><text font-family="Arial">Test</text></svg>"#;
        renderer.set_template(replacement_template);
        assert!(
            Arc::ptr_eq(&renderer.options().fontdb, &full_fontdb()),
            "replacement template with Arial should use full fontdb"
        );
    }
}
