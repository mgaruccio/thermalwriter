use std::collections::HashMap;
use std::time::Duration;

const MAX_CANVAS_DIMENSION: u32 = 8192;
const MAX_CANVAS_ALLOC_BYTES: u64 = 256 * 1024 * 1024;
const CANVAS_BYTES_PER_PIXEL: u64 = 8; // logical RGBA buffer plus conversion buffer

pub struct HistoryConfig {
    pub duration: Duration,
    pub sample_hz: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct VariableDecl {
    pub var_type: String, // "color", "text", "sensor", "number"
    pub default: String,
    pub help: String,
    // Optional bounds for "number" vars, parsed from `number(min,max,step)`.
    // None for all other types (and for `number` declared without bounds).
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanvasMode {
    Responsive,
    Fixed { width: u32, height: u32 },
}

pub struct LayoutFrontmatter {
    pub history_configs: HashMap<String, HistoryConfig>,
    pub animation_fps: Option<u32>,
    pub animation_decode: Option<String>,
    pub variables: HashMap<String, VariableDecl>,
    pub canvas: Option<CanvasMode>,
}

impl LayoutFrontmatter {
    pub fn parse(template: &str) -> Self {
        let mut fm = Self {
            history_configs: HashMap::new(),
            animation_fps: None,
            animation_decode: None,
            variables: HashMap::new(),
            canvas: None,
        };

        let mut accumulating: Option<Vec<String>> = None;

        for line in template.lines() {
            let trimmed = line.trim();

            if let Some(ref mut block_lines) = accumulating {
                // We're inside a multi-line block — collect until closing #}
                if let Some(before_close) = trimmed.find("#}") {
                    // End of block — include content before the closing marker
                    let tail = trimmed[..before_close].trim();
                    if !tail.is_empty() {
                        block_lines.push(tail.to_string());
                    }
                    let block = block_lines.join("\n");
                    fm.dispatch_block(&block);
                    accumulating = None;
                } else {
                    block_lines.push(trimmed.to_string());
                }
            } else if let Some(after_open) = trimmed.strip_prefix("{#") {
                if let Some(inner) = after_open.strip_suffix("#}") {
                    // Single-line block: {# directive: spec #}
                    fm.dispatch_block(inner.trim());
                } else {
                    // Multi-line block opening: {# directive:\n...\n#}
                    let first_line = after_open.trim();
                    let mut block_lines = Vec::new();
                    if !first_line.is_empty() {
                        block_lines.push(first_line.to_string());
                    }
                    accumulating = Some(block_lines);
                }
            }
        }

        fm
    }

    fn dispatch_block(&mut self, inner: &str) {
        if let Some(rest) = inner.strip_prefix("history:") {
            self.parse_history(rest.trim());
        } else if let Some(rest) = inner.strip_prefix("animation:") {
            self.parse_animation(rest.trim());
        } else if let Some(rest) = inner.strip_prefix("vars:") {
            self.parse_vars(rest.trim());
        } else if let Some(rest) = inner.strip_prefix("canvas:") {
            self.parse_canvas(rest.trim());
        }
    }

    fn parse_canvas(&mut self, spec: &str) {
        let spec = spec.trim();
        if spec.eq_ignore_ascii_case("responsive") {
            self.canvas = Some(CanvasMode::Responsive);
            return;
        }
        // WIDTHxHEIGHT
        if let Some((w, h)) = spec.split_once('x')
            && let (Ok(w), Ok(h)) = (w.trim().parse::<u32>(), h.trim().parse::<u32>())
            && Self::valid_canvas_dimensions(w, h)
        {
            self.canvas = Some(CanvasMode::Fixed {
                width: w,
                height: h,
            });
        }
    }

    fn valid_canvas_dimensions(width: u32, height: u32) -> bool {
        let bytes = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|pixels| pixels.checked_mul(CANVAS_BYTES_PER_PIXEL));
        let Some(bytes) = bytes else {
            return false;
        };

        width > 0
            && height > 0
            && width <= MAX_CANVAS_DIMENSION
            && height <= MAX_CANVAS_DIMENSION
            && bytes <= MAX_CANVAS_ALLOC_BYTES
    }

    fn parse_history(&mut self, spec: &str) {
        // Format: "cpu_temp=60s, cpu_util=120s, net_rx=300s@0.2hz"
        for part in spec.split(',') {
            let part = part.trim();
            if let Some((key, rest)) = part.split_once('=') {
                let key = key.trim();
                let rest = rest.trim();
                let (duration_str, hz) = if let Some((d, h)) = rest.split_once('@') {
                    (
                        d.trim(),
                        h.trim()
                            .strip_suffix("hz")
                            .and_then(|s| s.parse::<f64>().ok()),
                    )
                } else {
                    (rest, None)
                };
                if let Some(secs_str) = duration_str.strip_suffix('s')
                    && let Ok(secs) = secs_str.parse::<u64>()
                {
                    self.history_configs.insert(
                        key.to_string(),
                        HistoryConfig {
                            duration: Duration::from_secs(secs),
                            sample_hz: hz,
                        },
                    );
                }
            }
        }
    }

    fn parse_animation(&mut self, spec: &str) {
        // Format: "fps=15, decode=stream"
        for part in spec.split(',') {
            let part = part.trim();
            if let Some((key, val)) = part.split_once('=') {
                match key.trim() {
                    "fps" => self.animation_fps = val.trim().parse().ok(),
                    "decode" => self.animation_decode = Some(val.trim().to_string()),
                    _ => {}
                }
            }
        }
    }

    fn parse_vars(&mut self, spec: &str) {
        // Format (one var per line):
        //   name: type = "default" "help text"
        // Validation:
        //   - name: [a-z_][a-z0-9_]*
        //   - type: "color" | "text" | "sensor" | "number" | "number(min,max,step)"
        //   - color default: ^#[0-9a-fA-F]{6,8}$
        //   - text default: must not contain {{ }} {% %}
        //   - number default: must parse as f64; optional (min,max,step) slider bounds
        for line in spec.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match Self::parse_var_line(line) {
                Some(parsed) => {
                    self.variables.insert(parsed.0, parsed.1);
                }
                None => log::warn!(
                    "frontmatter: skipping malformed or invalid var line: {:?}",
                    line
                ),
            }
        }
    }

    fn parse_var_line(line: &str) -> Option<(String, VariableDecl)> {
        // Parse: `name: type = "default" "help text"`
        let (name_type, rest) = line.split_once('=')?;
        let rest = rest.trim();

        // Split name and type: `name: type`
        let (name, var_type_raw) = name_type.split_once(':')?;
        let name = name.trim().to_string();
        let var_type_raw = var_type_raw.trim();

        // Validate name: [a-z_][a-z0-9_]*
        if !is_valid_var_name(&name) {
            return None;
        }

        // Split off optional `(min,max,step)` bounds from the type token.
        let (var_type, bounds) = match var_type_raw.split_once('(') {
            Some((base, params)) => {
                let inner = params.strip_suffix(')')?;
                (base.trim().to_string(), Some(parse_bounds(inner)?))
            }
            None => (var_type_raw.to_string(), None),
        };

        // Parse two quoted strings: "default" "help"
        let (default, help) = parse_two_quoted_strings(rest)?;

        // Validate by type. Only "number" accepts bounds; bounds on any other
        // type is a malformed declaration.
        let (min, max, step) = match var_type.as_str() {
            "color" => {
                if bounds.is_some() || !is_valid_color(&default) {
                    return None;
                }
                (None, None, None)
            }
            "text" => {
                if bounds.is_some() || contains_template_syntax(&default) {
                    return None;
                }
                (None, None, None)
            }
            "sensor" => {
                if bounds.is_some() {
                    return None;
                }
                (None, None, None)
            }
            "number" => {
                if default.parse::<f64>().is_err() {
                    return None;
                }
                bounds.unwrap_or((None, None, None))
            }
            _ => return None,
        };

        Some((
            name,
            VariableDecl {
                var_type,
                default,
                help,
                min,
                max,
                step,
            },
        ))
    }
}

/// Parse the inner text of a `number(min,max,step)` bounds spec. Each field is
/// optional (empty → None), but at most three comma-separated fields are allowed
/// and every present field must parse as f64.
fn parse_bounds(inner: &str) -> Option<(Option<f64>, Option<f64>, Option<f64>)> {
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    if parts.len() > 3 {
        return None;
    }
    // Each present (non-empty) field must parse as f64; empty/absent → None.
    let parse_at = |i: usize| -> Option<Option<f64>> {
        match parts.get(i) {
            None | Some(&"") => Some(None),
            Some(p) => p.parse::<f64>().ok().map(Some),
        }
    };
    Some((parse_at(0)?, parse_at(1)?, parse_at(2)?))
}

fn is_valid_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some('a'..='z' | '_') => {}
        _ => return false,
    }
    chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_'))
}

fn is_valid_color(s: &str) -> bool {
    if !s.starts_with('#') {
        return false;
    }
    let hex = &s[1..];
    let len = hex.len();
    if len != 6 && len != 8 {
        return false;
    }
    hex.chars().all(|c| c.is_ascii_hexdigit())
}

fn contains_template_syntax(s: &str) -> bool {
    s.contains("{{") || s.contains("}}") || s.contains("{%") || s.contains("%}")
}

/// Parse two consecutive quoted strings from a trimmed input, e.g.:
///   `"#00ff88" "Primary accent color"`
/// Returns (first, second) with quotes stripped.
fn parse_two_quoted_strings(s: &str) -> Option<(String, String)> {
    let s = s.trim();
    // First string starts with "
    if !s.starts_with('"') {
        return None;
    }
    let s = &s[1..]; // strip leading "
    // Find the closing " of the first string (not escaped)
    let first_end = find_closing_quote(s)?;
    let first = s[..first_end].to_string();
    let rest = s[first_end + 1..].trim(); // skip closing " and whitespace

    // Second string
    if !rest.starts_with('"') {
        return None;
    }
    let rest = &rest[1..];
    let second_end = find_closing_quote(rest)?;
    let second = rest[..second_end].to_string();

    Some((first, second))
}

/// Find the index of the closing unescaped `"` in a string that starts right after an opening `"`.
fn find_closing_quote(s: &str) -> Option<usize> {
    let mut escape = false;
    for (i, c) in s.char_indices() {
        if escape {
            escape = false;
        } else if c == '\\' {
            escape = true;
        } else if c == '"' {
            return Some(i);
        }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_canvas_accepts_bounded_dimensions() {
        let frontmatter = LayoutFrontmatter::parse("{# canvas: 4096x4096 #}");
        assert_eq!(
            frontmatter.canvas,
            Some(CanvasMode::Fixed {
                width: 4096,
                height: 4096,
            })
        );
    }

    #[test]
    fn fixed_canvas_rejects_dimension_and_area_limits() {
        for spec in [
            "{# canvas: 8193x1 #}",
            "{# canvas: 1x8193 #}",
            "{# canvas: 8192x4097 #}",
            "{# canvas: 4294967295x4294967295 #}",
        ] {
            assert_eq!(
                LayoutFrontmatter::parse(spec).canvas,
                None,
                "oversized canvas must not construct CanvasMode::Fixed: {spec}"
            );
        }
    }

    #[test]
    fn fixed_canvas_rejects_zero_dimensions() {
        assert_eq!(LayoutFrontmatter::parse("{# canvas: 0x480 #}").canvas, None);
        assert_eq!(LayoutFrontmatter::parse("{# canvas: 480x0 #}").canvas, None);
    }

    #[test]
    fn fixed_canvas_area_check_handles_u32_overflow() {
        assert!(!LayoutFrontmatter::valid_canvas_dimensions(
            u32::MAX,
            u32::MAX
        ));
    }
}
