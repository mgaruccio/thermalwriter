//! Shared names between daemon sensor keys and typed layout bindings.
//!
//! SVG/HTML layouts keep the flat `cpu_temp` contract. Typed `.layout.toml`
//! documents use namespaced keys such as `cpu.temperature`. Both names refer to
//! the same reading.

/// Legacy daemon key, typed layout binding, and picker label.
const SENSOR_ALIASES: &[(&str, &str, &str)] = &[
    ("cpu_temp", "cpu.temperature", "CPU Temperature"),
    ("cpu_util", "cpu.utilization", "CPU Utilization"),
    ("cpu_power", "cpu.power", "CPU Power"),
    ("cpu_fan", "cpu.fan", "CPU Fan"),
    ("gpu_temp", "gpu.temperature", "GPU Temperature"),
    ("gpu_util", "gpu.utilization", "GPU Utilization"),
    ("gpu_power", "gpu.power", "GPU Power"),
    ("vram_used", "gpu.memory.used", "GPU Memory Used"),
    ("vram_total", "gpu.memory.total", "GPU Memory Total"),
    ("ram_used", "memory.used", "Memory Used"),
    ("ram_total", "memory.total", "Memory Total"),
    ("net_rx", "network.receive", "Network Receive"),
    ("net_tx", "network.transmit", "Network Transmit"),
    ("fps", "game.fps", "FPS"),
    ("frametime", "game.frametime", "Frame Time"),
];

/// Map a daemon/SVG sensor key onto the typed layout binding, if any.
pub fn layout_binding_alias(key: &str) -> Option<&'static str> {
    SENSOR_ALIASES
        .iter()
        .find(|(legacy, _, _)| *legacy == key)
        .map(|(_, alias, _)| *alias)
}

/// Map a typed layout binding onto the daemon/SVG sensor key, if any.
///
/// History suffixes (`.history` / `_history`) are stripped before lookup.
pub fn sensor_key_for_layout_binding(binding: &str) -> Option<&'static str> {
    let base = binding
        .strip_suffix(".history")
        .or_else(|| binding.strip_suffix("_history"))
        .unwrap_or(binding);
    SENSOR_ALIASES
        .iter()
        .find(|(_, alias, _)| *alias == base)
        .map(|(legacy, _, _)| *legacy)
}

/// Human picker label for a typed layout binding.
pub fn layout_binding_label(binding: &str) -> Option<&'static str> {
    let base = binding
        .strip_suffix(".history")
        .or_else(|| binding.strip_suffix("_history"))
        .unwrap_or(binding);
    SENSOR_ALIASES
        .iter()
        .find(|(_, alias, _)| *alias == base)
        .map(|(_, _, label)| *label)
}

/// True when `binding` is present as itself or via the shared alias table.
pub fn layout_binding_is_known(
    binding: &str,
    declared_keys: &std::collections::HashSet<String>,
) -> bool {
    let binding = binding.trim();
    if binding.is_empty() {
        return false;
    }
    if declared_keys.contains(binding) {
        return true;
    }
    let base = binding
        .strip_suffix(".history")
        .or_else(|| binding.strip_suffix("_history"))
        .unwrap_or(binding);
    if declared_keys.contains(base) {
        return true;
    }
    sensor_key_for_layout_binding(base).is_some_and(|legacy| declared_keys.contains(legacy))
}

/// Every typed alias that should appear in a picker when `legacy_key` is live.
pub fn published_layout_aliases(
    legacy_key: &str,
) -> impl Iterator<Item = (&'static str, &'static str)> {
    SENSOR_ALIASES
        .iter()
        .filter(move |(legacy, _, _)| *legacy == legacy_key)
        .map(|(_, alias, label)| (*alias, *label))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn cpu_temp_round_trips_to_typed_binding() {
        assert_eq!(layout_binding_alias("cpu_temp"), Some("cpu.temperature"));
        assert_eq!(
            sensor_key_for_layout_binding("cpu.temperature"),
            Some("cpu_temp")
        );
        assert_eq!(
            sensor_key_for_layout_binding("cpu.temperature.history"),
            Some("cpu_temp")
        );
    }

    #[test]
    fn cpu_temperature_is_known_when_cpu_temp_is_declared() {
        let declared = HashSet::from(["cpu_temp".to_string()]);
        assert!(layout_binding_is_known("cpu.temperature", &declared));
        assert!(layout_binding_is_known(
            "cpu.temperature.history",
            &declared
        ));
        assert!(!layout_binding_is_known("unknown.temperature", &declared));
    }
}
