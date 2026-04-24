// RAPL power sensor: reads CPU package power from /sys/class/powercap.
// Computes instantaneous watts from energy counter deltas between polls.

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;

use super::{SensorDescriptor, SensorProvider, SensorReading};

const DEFAULT_POWERCAP_PATH: &str = "/sys/class/powercap";

pub struct RaplProvider {
    base_path: PathBuf,
    last_energy_uj: Option<u64>,
    last_poll: Option<Instant>,
    access_warned: bool,
}

impl RaplProvider {
    pub fn new() -> Self {
        Self {
            base_path: PathBuf::from(DEFAULT_POWERCAP_PATH),
            last_energy_uj: None,
            last_poll: None,
            access_warned: false,
        }
    }

    pub fn with_base_path(base: PathBuf) -> Self {
        Self {
            base_path: base,
            last_energy_uj: None,
            last_poll: None,
            access_warned: false,
        }
    }

    fn read_energy_uj(&self) -> Option<u64> {
        // intel-rapl:0 is the CPU package (works on both Intel and AMD)
        let path = self.base_path.join("intel-rapl:0/energy_uj");
        fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    fn read_max_energy_uj(&self) -> Option<u64> {
        let path = self.base_path.join("intel-rapl:0/max_energy_range_uj");
        fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }
}

impl SensorProvider for RaplProvider {
    fn name(&self) -> &str {
        "rapl"
    }

    fn poll(&mut self) -> Result<Vec<SensorReading>> {
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
                    // Counter wrapped — add max range
                    let max = self.read_max_energy_uj().unwrap_or(u64::MAX);
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
            }]
        } else {
            Vec::new()
        }
    }
}
