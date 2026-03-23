use std::collections::HashMap;
use std::time::Duration;

pub struct HistoryConfig {
    pub duration: Duration,
    pub sample_hz: Option<f64>,
}

pub struct LayoutFrontmatter {
    pub history_configs: HashMap<String, HistoryConfig>,
    pub animation_fps: Option<u32>,
    pub animation_decode: Option<String>,
}

impl LayoutFrontmatter {
    pub fn parse(template: &str) -> Self {
        let mut fm = Self {
            history_configs: HashMap::new(),
            animation_fps: None,
            animation_decode: None,
        };

        for line in template.lines() {
            let trimmed = line.trim();
            if let Some(inner) = trimmed.strip_prefix("{#").and_then(|s| s.strip_suffix("#}")) {
                let inner = inner.trim();
                if let Some(rest) = inner.strip_prefix("history:") {
                    fm.parse_history(rest.trim());
                } else if let Some(rest) = inner.strip_prefix("animation:") {
                    fm.parse_animation(rest.trim());
                }
            }
        }

        fm
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
}
