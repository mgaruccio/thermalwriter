// HTML/CSS subset parser: converts HTML template strings into an element tree.
// Only handles the subset needed for thermalrighter layouts:
//   - Elements: div, span, and similar block/inline tags
//   - Attributes: style="" only
//   - CSS properties: flex layout, colors, font, dimensions

/// A parsed CSS color.
#[derive(Debug, Clone, PartialEq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub fn from_hex(hex: &str) -> Option<Self> {
        let hex = hex.trim_start_matches('#');
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(Self { r, g, b, a: 255 })
        } else if hex.len() == 3 {
            // Shorthand #rgb → #rrggbb
            let r = u8::from_str_radix(&hex[0..1], 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2], 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3], 16).ok()?;
            Some(Self { r: r * 17, g: g * 17, b: b * 17, a: 255 })
        } else {
            None
        }
    }

    pub fn white() -> Self { Self { r: 255, g: 255, b: 255, a: 255 } }
    pub fn black() -> Self { Self { r: 0, g: 0, b: 0, a: 255 } }
    pub fn transparent() -> Self { Self { r: 0, g: 0, b: 0, a: 0 } }
}

/// Parsed inline styles relevant to our subset.
#[derive(Debug, Clone, Default)]
pub struct ElementStyle {
    pub display: Option<String>,           // "flex", "block"
    pub flex_direction: Option<String>,    // "row", "column"
    pub justify_content: Option<String>,   // "center", "space-between", etc.
    pub align_items: Option<String>,       // "center", "flex-start", etc.
    pub gap: Option<f32>,                  // px
    pub padding: Option<f32>,             // px (uniform for now)
    pub margin: Option<f32>,              // px (uniform for now)
    pub font_size: Option<f32>,           // px
    pub font_family: Option<String>,
    pub color: Option<Color>,
    pub background: Option<Color>,
    pub text_align: Option<String>,       // "left", "center", "right"
    pub border_radius: Option<f32>,       // px
    pub width: Option<f32>,               // px
    pub height: Option<f32>,              // px
}

/// A node in the parsed element tree.
#[derive(Debug, Clone)]
pub struct Element {
    pub tag: String,
    pub style: ElementStyle,
    pub text: Option<String>,
    pub children: Vec<Element>,
}

/// Parse an HTML string (our subset) into an element tree.
pub fn parse_html(html: &str) -> anyhow::Result<Element> {
    let html = html.trim();
    let mut parser = HtmlParser::new(html);
    parser.parse_element()
}

/// Parse a CSS inline style string into an ElementStyle.
pub fn parse_style(style_str: &str) -> ElementStyle {
    let mut style = ElementStyle::default();
    for decl in style_str.split(';') {
        let decl = decl.trim();
        if decl.is_empty() { continue; }
        let mut parts = decl.splitn(2, ':');
        let prop = parts.next().unwrap_or("").trim();
        let val = parts.next().unwrap_or("").trim();
        match prop {
            "display" => style.display = Some(val.to_string()),
            "flex-direction" => style.flex_direction = Some(val.to_string()),
            "justify-content" => style.justify_content = Some(val.to_string()),
            "align-items" => style.align_items = Some(val.to_string()),
            "text-align" => style.text_align = Some(val.to_string()),
            "font-family" => style.font_family = Some(val.to_string()),
            "gap" => style.gap = parse_px(val),
            "padding" => style.padding = parse_px(val),
            "margin" => style.margin = parse_px(val),
            "font-size" => style.font_size = parse_px(val),
            "border-radius" => style.border_radius = parse_px(val),
            "width" => style.width = parse_px(val),
            "height" => style.height = parse_px(val),
            "color" => style.color = Color::from_hex(val),
            "background" => style.background = Color::from_hex(val),
            _ => {} // Ignore unknown properties
        }
    }
    style
}

fn parse_px(val: &str) -> Option<f32> {
    val.trim_end_matches("px").trim().parse().ok()
}

struct HtmlParser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> HtmlParser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn remaining(&self) -> &'a str {
        &self.input[self.pos..]
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.input.len()
            && self.input.as_bytes()[self.pos].is_ascii_whitespace()
        {
            self.pos += 1;
        }
    }

    fn starts_with(&self, s: &str) -> bool {
        self.remaining().starts_with(s)
    }

    fn parse_element(&mut self) -> anyhow::Result<Element> {
        self.skip_whitespace();
        anyhow::ensure!(
            self.starts_with("<"),
            "Expected '<', got {:?}",
            &self.remaining()[..20.min(self.remaining().len())]
        );

        // Parse opening tag
        self.pos += 1; // skip '<'
        let tag = self.parse_tag_name();
        let style = self.parse_attributes();
        self.skip_whitespace();

        // Skip '>'
        anyhow::ensure!(self.starts_with(">"), "Expected '>'");
        self.pos += 1;

        // Parse children and text
        let mut children = Vec::new();
        let mut text_parts = Vec::new();

        loop {
            self.skip_whitespace();
            if self.starts_with(&format!("</{}", tag)) {
                // Closing tag
                self.pos += 2 + tag.len(); // skip '</' + tag
                self.skip_whitespace();
                if self.starts_with(">") { self.pos += 1; }
                break;
            } else if self.starts_with("<") {
                // Child element
                children.push(self.parse_element()?);
            } else if self.pos < self.input.len() {
                // Text content
                let start = self.pos;
                while self.pos < self.input.len() && !self.starts_with("<") {
                    self.pos += 1;
                }
                let t = self.input[start..self.pos].trim();
                if !t.is_empty() {
                    text_parts.push(t.to_string());
                }
            } else {
                break;
            }
        }

        let text = if text_parts.is_empty() { None } else { Some(text_parts.join(" ")) };

        Ok(Element { tag, style, text, children })
    }

    fn parse_tag_name(&mut self) -> String {
        let start = self.pos;
        while self.pos < self.input.len() {
            let ch = self.input.as_bytes()[self.pos];
            if ch.is_ascii_alphanumeric() || ch == b'-' {
                self.pos += 1;
            } else {
                break;
            }
        }
        self.input[start..self.pos].to_string()
    }

    fn parse_attributes(&mut self) -> ElementStyle {
        self.skip_whitespace();
        let mut style = ElementStyle::default();

        while self.pos < self.input.len() && !self.starts_with(">") {
            self.skip_whitespace();
            if self.starts_with(">") { break; }

            let attr_name = self.parse_tag_name();
            if attr_name.is_empty() {
                // Skip unknown character and advance
                self.pos += 1;
                continue;
            }
            self.skip_whitespace();
            if self.starts_with("=") {
                self.pos += 1; // skip '='
                self.skip_whitespace();
                let value = self.parse_attr_value();
                if attr_name == "style" {
                    style = parse_style(&value);
                }
            }
        }

        style
    }

    fn parse_attr_value(&mut self) -> String {
        self.skip_whitespace();
        if self.pos >= self.input.len() {
            return String::new();
        }
        let quote = self.input.as_bytes()[self.pos];
        if quote == b'"' || quote == b'\'' {
            self.pos += 1;
            let start = self.pos;
            while self.pos < self.input.len() && self.input.as_bytes()[self.pos] != quote {
                self.pos += 1;
            }
            let val = self.input[start..self.pos].to_string();
            if self.pos < self.input.len() { self.pos += 1; } // skip closing quote
            val
        } else {
            let start = self.pos;
            while self.pos < self.input.len()
                && !self.input.as_bytes()[self.pos].is_ascii_whitespace()
                && self.input.as_bytes()[self.pos] != b'>'
            {
                self.pos += 1;
            }
            self.input[start..self.pos].to_string()
        }
    }
}
