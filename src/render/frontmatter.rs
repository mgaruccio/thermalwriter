use std::collections::HashMap;
use std::time::Duration;

pub struct HistoryConfig {
    pub duration: Duration,
    pub sample_hz: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct VariableDecl {
    pub var_type: String, // "color", "text", "sensor"
    pub default: String,
    pub help: String,
}

pub struct LayoutFrontmatter {
    pub history_configs: HashMap<String, HistoryConfig>,
    pub animation_fps: Option<u32>,
    pub animation_decode: Option<String>,
    pub variables: HashMap<String, VariableDecl>,
}

impl LayoutFrontmatter {
    pub fn parse(template: &str) -> Self {
        let mut fm = Self {
            history_configs: HashMap::new(),
            animation_fps: None,
            animation_decode: None,
            variables: HashMap::new(),
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
        }
    }

    fn parse_history(&mut self, spec: &str) {
        // Format: "cpu_temp=60s, cpu_util=120s, net_rx=300s@0.2hz"
        for part in spec.split(',') {
            let part = part.trim();
            if let Some((key, rest)) = part.split_once('=') {
                let key = key.trim();
                let rest = rest.trim();
                let (duration_str, hz) = if let Some((d, h)) = rest.split_once('@') {
                    (d.trim(), h.trim().strip_suffix("hz").and_then(|s| s.parse::<f64>().ok()))
                } else {
                    (rest, None)
                };
                if let Some(secs_str) = duration_str.strip_suffix('s') {
                    if let Ok(secs) = secs_str.parse::<u64>() {
                        self.history_configs.insert(key.to_string(), HistoryConfig {
                            duration: Duration::from_secs(secs),
                            sample_hz: hz,
                        });
                    }
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
        //   - type: "color" | "text" | "sensor"
        //   - color default: ^#[0-9a-fA-F]{6,8}$
        //   - text default: must not contain {{ }} {% %}
        for line in spec.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(parsed) = Self::parse_var_line(line) {
                self.variables.insert(parsed.0, parsed.1);
            }
        }
    }

    fn parse_var_line(line: &str) -> Option<(String, VariableDecl)> {
        // Parse: `name: type = "default" "help text"`
        let (name_type, rest) = line.split_once('=')?;
        let rest = rest.trim();

        // Split name and type: `name: type`
        let (name, var_type) = name_type.split_once(':')?;
        let name = name.trim().to_string();
        let var_type = var_type.trim().to_string();

        // Validate name: [a-z_][a-z0-9_]*
        if !is_valid_var_name(&name) {
            return None;
        }

        // Parse two quoted strings: "default" "help"
        let (default, help) = parse_two_quoted_strings(rest)?;

        // Validate by type
        match var_type.as_str() {
            "color" => {
                if !is_valid_color(&default) {
                    return None;
                }
            }
            "text" => {
                if contains_template_syntax(&default) {
                    return None;
                }
            }
            "sensor" => {}
            _ => return None,
        }

        Some((name, VariableDecl { var_type, default, help }))
    }
}

fn is_valid_var_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !matches!(first, 'a'..='z' | '_') {
        return false;
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
