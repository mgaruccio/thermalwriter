// RAPL power sensor: reads CPU package power from /sys/class/powercap.
// Computes instantaneous watts from energy counter deltas between polls.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;

use super::{SensorDescriptor, SensorProvider, SensorReading};

const DEFAULT_POWERCAP_PATH: &str = "/sys/class/powercap";

pub struct RaplProvider {
    base_path: PathBuf,
    // Cached at construction — max_energy_range_uj changes only on kernel/hardware change.
    // On rollover, if this is None we skip the tick rather than substituting u64::MAX
    // which would produce ~18 TW spurious readings.
    max_energy_uj: Option<u64>,
    last_energy_uj: Option<u64>,
    last_poll: Option<Instant>,
    access_warned: bool,
    needed_keys: Option<HashSet<String>>,
}

impl Default for RaplProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl RaplProvider {
    pub fn new() -> Self {
        let base_path = PathBuf::from(DEFAULT_POWERCAP_PATH);
        let max_energy_uj = Self::read_max_at(&base_path);
        Self {
            base_path,
            max_energy_uj,
            last_energy_uj: None,
            last_poll: None,
            access_warned: false,
            needed_keys: None,
        }
    }

    pub fn with_base_path(base: PathBuf) -> Self {
        let max_energy_uj = Self::read_max_at(&base);
        Self {
            base_path: base,
            max_energy_uj,
            last_energy_uj: None,
            last_poll: None,
            access_warned: false,
            needed_keys: None,
        }
    }

    fn read_max_at(base: &Path) -> Option<u64> {
        let path = base.join("intel-rapl:0/max_energy_range_uj");
        fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    fn read_energy_uj(&self) -> Option<u64> {
        // intel-rapl:0 is the CPU package (works on both Intel and AMD)
        let path = self.base_path.join("intel-rapl:0/energy_uj");
        fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }
    pub fn last_energy_uj(&self) -> Option<u64> {
        self.last_energy_uj
    }
}

impl SensorProvider for RaplProvider {
    fn name(&self) -> &str {
        "rapl"
    }
    fn set_needed_keys(&mut self, keys: Option<&HashSet<String>>) {
        // None means full discovery (poll everything). Some(set) means pruned.
        // cpu_power is "needed" when None (discovery) or when the set contains it.
        let was_needed = self.needed_keys.is_none()
            || self
                .needed_keys
                .as_ref()
                .is_some_and(|k| k.contains("cpu_power"));
        let now_needed = keys.is_none() || keys.is_some_and(|k| k.contains("cpu_power"));
        // Reset baseline only on the false→true transition (unavailable→available),
        // so the first poll after a pruning gap primes the counter instead of
        // averaging over the entire gap (and avoids multi-wrap issues on long gaps).
        if !was_needed && now_needed {
            self.last_energy_uj = None;
            self.last_poll = None;
        }
        self.needed_keys = keys.cloned();
    }
    fn wants_any(&self, needed: &HashSet<String>) -> bool {
        needed.contains("cpu_power")
    }

    fn poll(&mut self) -> Result<Vec<SensorReading>> {
        // If needed_keys is set and cpu_power isn't needed, skip entirely.
        if let Some(ref needed) = self.needed_keys {
            if !needed.contains("cpu_power") {
                return Ok(Vec::new());
            }
        }
        let mut readings = Vec::new();

        let Some(energy_uj) = self.read_energy_uj() else {
            // Distinguish "no RAPL hardware" (silent) from "exists but unreadable" (actionable warn).
            if !self.access_warned {
                let path = self.base_path.join("intel-rapl:0/energy_uj");
                if path.exists() {
                    log::warn!(
                        "Cannot read {} — CPU power will display as \"--\". \
                         Run `thermalwriter setup-udev` to install the udev rule that grants non-root access.",
                        path.display()
                    );
                    self.access_warned = true;
                }
            }
            return Ok(readings);
        };

        let now = Instant::now();

        if let (Some(prev_energy), Some(prev_time)) = (self.last_energy_uj, self.last_poll) {
            let dt = now.duration_since(prev_time);
            let dt_secs = dt.as_secs_f64();

            if dt_secs > 0.05 {
                // Handle counter rollover
                let delta_uj = if energy_uj >= prev_energy {
                    energy_uj - prev_energy
                } else {
                    // Counter wrapped — add max range. If max is unknown, skip this
                    // tick rather than substituting u64::MAX (~1.8e13 µJ → ~18 TW).
                    let Some(max) = self.max_energy_uj else {
                        self.last_energy_uj = Some(energy_uj);
                        self.last_poll = Some(now);
                        return Ok(readings);
                    };
                    (max - prev_energy) + energy_uj
                };

                let watts = (delta_uj as f64 / 1_000_000.0) / dt_secs;
                let watts_str = format!("{:.0}", watts);

                readings.push(SensorReading {
                    key: "cpu_power".to_string(),
                    value: watts_str,
                    unit: "W".to_string(),
                });
            }
        }

        self.last_energy_uj = Some(energy_uj);
        self.last_poll = Some(now);

        Ok(readings)
    }

    fn available_sensors(&self) -> Vec<SensorDescriptor> {
        if self.read_energy_uj().is_some() {
            vec![SensorDescriptor {
                key: "cpu_power".to_string(),
                name: "CPU Package Power".to_string(),
                unit: "W".to_string(),
                cost_us: 0,
            }]
        } else {
            Vec::new()
        }
    }
    fn declared_keys(&self) -> Vec<&str> {
        vec!["cpu_power"]
    }
}
