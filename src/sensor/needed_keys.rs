// Layout-derived needed-key computation.
//
// Replaces the old `default_needed_keys()` fixed desktop set. Instead of always
// polling CPU/GPU/RAM/FPS regardless of what the active layout displays, we
// derive the needed key set from the layout's frontmatter (history configs +
// sensor variables) and a token scan of the template body against the known
// sensor catalog. Providers that can't contribute any needed key are
// short-circuited in `SensorHub::poll`.

use std::collections::{HashMap, HashSet};

use crate::render::frontmatter::LayoutFrontmatter;
/// Cached recipe of the active layout's sensor-relevant inputs, so the tick
/// loop can recompute needed keys when the catalog transitions from empty to
/// non-empty (pre-discovery → discovery). Updated by the mode listener on
/// every layout change and at startup.
#[derive(Clone, Debug)]
pub struct LayoutSensorRecipe {
    pub template: String,
    pub vars: HashMap<String, String>,
}

/// Minimal bootstrap set used when a layout's frontmatter is empty but the
/// sensor catalog is non-empty. These are the canonical desktop keys a user
/// expects to see even on a blank layout.
const BOOTSTRAP_KEYS: &[&str] = &["cpu_temp", "gpu_temp", "ram_used"];

/// Keys the active layout can actually display.
///
/// Sources (union):
/// 1. `frontmatter.history_configs.keys()` — history graph metrics.
/// 2. For each `frontmatter.variables` with `var_type == "sensor"`: the resolved
///    value from `layout_vars` if present, else `decl.default`, if non-empty.
/// 3. Token scan of `template`: for every key in `known_keys` ∪ `declared_keys`,
///    if the template contains the key as a Tera identifier (word-boundary match
///    where `_` is not a boundary, so `cpu_temp` matches inside `cpu_temp_history`),
///    include it. Matching `foo_history` also includes `foo` when `foo` is in
///    the scan set.
///
/// Empty result: if the union is empty (blank layout), fall back to
/// `known_keys` intersected with `BOOTSTRAP_KEYS` **only when `known_keys` is
/// non-empty**; if the catalog is empty (pre-discovery), return empty and leave
/// the hub in discovery mode (`needed = None`).
///
/// `declared_keys` is the union of provider-declared canonical keys (e.g.
/// `cpu_power` from RAPL even when temporarily unreadable). It is used only for
/// token/history scanning — never for the bootstrap fallback — so that a
/// transiently-unreadable sensor stays eligible for polling when a layout
/// references it, without keeping unavailable providers polling on blank layouts.
pub fn layout_needed_keys(
    frontmatter: &LayoutFrontmatter,
    layout_vars: &HashMap<String, String>,
    template: &str,
    known_keys: &HashSet<String>,
    declared_keys: &HashSet<String>,
) -> HashSet<String> {
    let mut needed = HashSet::new();

    // 1. History configs.
    for key in frontmatter.history_configs.keys() {
        needed.insert(key.clone());
    }

    // 2. Sensor variables.
    for (name, decl) in &frontmatter.variables {
        if decl.var_type == "sensor" {
            if let Some(v) = layout_vars.get(name) {
                if !v.is_empty() {
                    needed.insert(v.clone());
                }
            } else if !decl.default.is_empty() {
                needed.insert(decl.default.clone());
            }
        }
    }

    // 3. Token scan: for each key in known_keys ∪ declared_keys, check if the
    //    template contains it as a Tera identifier. Word boundary: the char
    //    before (if any) is not alphanumeric, and the char after (if any) is
    //    not alphanumeric. This means `cpu_temp` matches inside `cpu_temp_history`
    //    (since `_` is not alphanumeric), but not inside `cpu_temperature`.
    //    `declared_keys` lets a transiently-unreadable sensor (e.g. RAPL's
    //    `cpu_power`) stay eligible when a layout references it.
    let scan_keys: HashSet<&String> = known_keys.iter().chain(declared_keys).collect();
    for key in &scan_keys {
        if contains_key_as_identifier(template, key) {
            needed.insert((*key).clone());
        }
    }

    // 3b. Explicit `foo_history` scan: if the template references `foo_history`
    //     and `foo` is in the scan set, include `foo` (history graphs need
    //     the base sensor value even if the template only uses the history
    //     array directly).
    for key in &scan_keys {
        let history_key = format!("{}_history", key);
        if contains_key_as_identifier(template, &history_key) {
            needed.insert((*key).clone());
        }
    }

    // Empty result: bootstrap from known keys if catalog is non-empty.
    if needed.is_empty() {
        if !known_keys.is_empty() {
            for k in known_keys {
                if BOOTSTRAP_KEYS.contains(&k.as_str()) {
                    needed.insert(k.clone());
                }
            }
        }
        return needed;
    }

    needed
}

/// Check if `key` appears in `template` as a Tera identifier.
///
/// A "word-boundary match" where the boundary is defined by non-alphanumeric
/// characters (NOT including `_`). This means `cpu_temp` matches inside
/// `cpu_temp_history` (the `_` after `cpu_temp` is not alphanumeric), but not
/// inside `cpu_temperature` (the `e` after `cpu_temp` is alphanumeric).
fn contains_key_as_identifier(template: &str, key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    let tb = template.as_bytes();
    let kb = key.as_bytes();
    if tb.len() < kb.len() {
        return false;
    }

    let mut start = 0;
    while start + kb.len() <= tb.len() {
        // Find the next occurrence of key starting from `start`.
        let remaining = &tb[start..];
        let pos = remaining.windows(kb.len()).position(|w| w == kb);
        let Some(pos) = pos else {
            break;
        };
        let abs_pos = start + pos;

        // Check boundary before.
        let before_ok = abs_pos == 0 || !tb[abs_pos - 1].is_ascii_alphanumeric();
        // Check boundary after.
        let after_end = abs_pos + kb.len();
        let after_ok = after_end == tb.len() || !tb[after_end].is_ascii_alphanumeric();

        if before_ok && after_ok {
            return true;
        }
        start = abs_pos + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::frontmatter::LayoutFrontmatter;

    fn known(keys: &[&str]) -> HashSet<String> {
        keys.iter().map(|s| s.to_string()).collect()
    }

    fn declared(keys: &[&str]) -> HashSet<String> {
        keys.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn layout_needed_keys_includes_history_and_template_tokens() {
        let template = r#"
{# history: cpu_temp=60s, gpu_temp=60s, ram_used=120s #}
<svg>{{ cpu_temp }} {{ cpu_util }} {{ gpu_temp }} {{ gpu_util }} {{ ram_used }} {{ ram_total }} {{ vram_used }} {{ fps }}</svg>
"#;
        let fm = LayoutFrontmatter::parse(template);
        let known = known(&[
            "cpu_temp",
            "cpu_util",
            "cpu_power",
            "cpu_fan",
            "gpu_temp",
            "gpu_util",
            "gpu_power",
            "vram_used",
            "vram_total",
            "ram_used",
            "ram_total",
            "fps",
            "frametime",
        ]);
        let needed = layout_needed_keys(&fm, &HashMap::new(), template, &known, &declared(&[]));

        // History keys.
        assert!(needed.contains("cpu_temp"));
        assert!(needed.contains("gpu_temp"));
        assert!(needed.contains("ram_used"));
        // Template tokens.
        assert!(needed.contains("cpu_util"));
        assert!(needed.contains("gpu_util"));
        assert!(needed.contains("ram_total"));
        assert!(needed.contains("vram_used"));
        assert!(needed.contains("fps"));
        // Not present in template.
        assert!(!needed.contains("cpu_power"));
        assert!(!needed.contains("frametime"));
    }

    #[test]
    fn layout_needed_keys_token_scan_finds_history_prefix() {
        let template = r#"<svg>{{ cpu_temp_history }}</svg>"#;
        let fm = LayoutFrontmatter::parse(template);
        let known = known(&["cpu_temp", "gpu_temp"]);
        let needed = layout_needed_keys(&fm, &HashMap::new(), template, &known, &declared(&[]));
        assert!(needed.contains("cpu_temp"), "foo_history must include foo");
        assert!(!needed.contains("gpu_temp"));
    }

    #[test]
    fn layout_needed_keys_empty_template_bootstraps_minimal() {
        let template = r#"<svg></svg>"#;
        let fm = LayoutFrontmatter::parse(template);
        let known = known(&["cpu_temp", "gpu_temp", "ram_used", "fps"]);
        let needed = layout_needed_keys(&fm, &HashMap::new(), template, &known, &declared(&[]));
        assert!(needed.contains("cpu_temp"));
        assert!(needed.contains("gpu_temp"));
        assert!(needed.contains("ram_used"));
        assert!(!needed.contains("fps"), "bootstrap must be minimal");
    }

    #[test]
    fn layout_needed_keys_empty_catalog_returns_empty() {
        let template = r#"<svg></svg>"#;
        let fm = LayoutFrontmatter::parse(template);
        let needed = layout_needed_keys(
            &fm,
            &HashMap::new(),
            template,
            &HashSet::new(),
            &declared(&[]),
        );
        assert!(needed.is_empty(), "pre-discovery catalog must return empty");
    }

    #[test]
    fn layout_needed_keys_sensor_var_resolved() {
        let template = r#"<svg>{{ temp_sensor }}°C</svg>"#;
        let fm = LayoutFrontmatter::parse(template);
        // Simulate a frontmatter with a sensor var.
        // We test the var resolution path via the public API by constructing
        // a frontmatter that has a sensor variable.
        let mut fm_with_var = fm;
        // Manually add a sensor variable to the frontmatter.
        fm_with_var.variables.insert(
            "temp_sensor".to_string(),
            crate::render::frontmatter::VariableDecl {
                var_type: "sensor".to_string(),
                default: "cpu_temp".to_string(),
                help: "Temperature sensor".to_string(),
                min: None,
                max: None,
                step: None,
            },
        );
        let known = known(&["cpu_temp", "gpu_temp"]);
        let needed = layout_needed_keys(
            &fm_with_var,
            &HashMap::new(),
            template,
            &known,
            &declared(&[]),
        );
        assert!(
            needed.contains("cpu_temp"),
            "sensor var default must be needed"
        );
    }

    #[test]
    fn layout_needed_keys_sensor_var_layout_vars_override() {
        let template = r#"<svg>{{ temp_sensor }}°C</svg>"#;
        let fm = LayoutFrontmatter::parse(template);
        let mut fm_with_var = fm;
        fm_with_var.variables.insert(
            "temp_sensor".to_string(),
            crate::render::frontmatter::VariableDecl {
                var_type: "sensor".to_string(),
                default: "cpu_temp".to_string(),
                help: "Temperature sensor".to_string(),
                min: None,
                max: None,
                step: None,
            },
        );
        let known = known(&["cpu_temp", "gpu_temp"]);
        let mut vars = HashMap::new();
        vars.insert("temp_sensor".to_string(), "gpu_temp".to_string());
        let needed = layout_needed_keys(&fm_with_var, &vars, template, &known, &declared(&[]));
        assert!(
            needed.contains("gpu_temp"),
            "layout_vars must override sensor var"
        );
        assert!(
            !needed.contains("cpu_temp"),
            "default must be overridden by layout_vars"
        );
    }

    #[test]
    fn contains_key_as_identifier_matches_history_suffix() {
        // cpu_temp should match inside cpu_temp_history
        assert!(contains_key_as_identifier(
            "{{ cpu_temp_history }}",
            "cpu_temp"
        ));
        // cpu_temp should match standalone
        assert!(contains_key_as_identifier("{{ cpu_temp }}", "cpu_temp"));
        // cpu_temp should NOT match inside cpu_temperature
        assert!(!contains_key_as_identifier(
            "{{ cpu_temperature }}",
            "cpu_temp"
        ));
        // cpu_temp should NOT match inside my_cpu_temp (preceded by _)
        // Actually _ is not alphanumeric, so it WOULD match. This is an
        // acceptable over-include per the plan.
        assert!(contains_key_as_identifier("{{ my_cpu_temp }}", "cpu_temp"));
    }

    #[test]
    fn declared_keys_keeps_unreadable_sensor_eligible() {
        // Template references cpu_power (RAPL), but the runtime catalog is empty
        // (RAPL temporarily unreadable). declared_keys should still pick it up.
        let template = r#"<svg>{{ cpu_power }}W</svg>"#;
        let fm = LayoutFrontmatter::parse(template);
        let known = HashSet::new();
        let declared = declared(&["cpu_power"]);
        let needed = layout_needed_keys(&fm, &HashMap::new(), template, &known, &declared);
        assert!(
            needed.contains("cpu_power"),
            "declared key must be needed even when catalog is empty"
        );
    }

    #[test]
    fn declared_keys_do_not_bootstrap_blank_layout() {
        // Blank template: declared keys should NOT bootstrap (only known_keys do).
        let template = r#"<svg></svg>"#;
        let fm = LayoutFrontmatter::parse(template);
        let known = HashSet::new();
        let declared = declared(&["cpu_power", "gpu_temp"]);
        let needed = layout_needed_keys(&fm, &HashMap::new(), template, &known, &declared);
        assert!(
            needed.is_empty(),
            "declared keys must not bootstrap blank layout"
        );
    }
}
