// hwmon sensor provider: reads /sys/class/hwmon for CPU temperatures and power.

use anyhow::Result;
use log::warn;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::{SensorDescriptor, SensorProvider, SensorReading};

const DEFAULT_HWMON_PATH: &str = "/sys/class/hwmon";

/// Chip names that correspond to CPU temperature sensors.
/// These receive a canonical `cpu_temp` alias on their first temp reading.
const CPU_CHIP_NAMES: &[&str] = &["k10temp", "coretemp", "zenpower", "k8temp", "fam15h_power"];

/// Wireless NIC hwmon chips are never read: their temp*_input is a firmware
/// RPC, not a register read, and blocks uninterruptibly for seconds when the
/// firmware is wedged (observed: ath12k "fw stats" stalls freezing the tick
/// loop 6s per poll).
const WIRELESS_CHIP_PREFIXES: &[&str] = &[
    "ath10k", "ath11k", "ath12k", "iwlwifi", "mt76", "mt79", "rtw88", "rtw89", "brcmfmac",
];

/// Any chip whose full read takes longer than this is quarantined for the
/// rest of the provider's lifetime. Normal sysfs sensor reads are microseconds;
/// crossing this means the read is blocking in a driver.
const SLOW_CHIP_THRESHOLD: Duration = Duration::from_millis(250);

pub struct HwmonProvider {
    base_path: PathBuf,
    /// Chips quarantined after a slow read; skipped on all subsequent polls.
    slow_chips: HashSet<String>,
}

impl Default for HwmonProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl HwmonProvider {
    pub fn new() -> Self {
        Self {
            base_path: PathBuf::from(DEFAULT_HWMON_PATH),
            slow_chips: HashSet::new(),
        }
    }

    /// For testing with a fake sysfs tree.
    pub fn with_base_path(base: PathBuf) -> Self {
        Self {
            base_path: base,
            slow_chips: HashSet::new(),
        }
    }

    fn read_file_trimmed(path: &std::path::Path) -> Option<String> {
        fs::read_to_string(path).ok().map(|s| s.trim().to_string())
    }
}

impl SensorProvider for HwmonProvider {
    fn name(&self) -> &str {
        "hwmon"
    }

    fn poll(&mut self) -> Result<Vec<SensorReading>> {
        let mut readings = Vec::new();
        let entries = match fs::read_dir(&self.base_path) {
            Ok(e) => e,
            Err(_) => return Ok(readings), // Missing sysfs — return empty, not error
        };

        let mut cpu_temp_aliased = false;
        let mut cpu_fan_aliased = false;

        for entry in entries.flatten() {
            let hwmon_dir = entry.path();
            let chip_name = Self::read_file_trimmed(&hwmon_dir.join("name"))
                .unwrap_or_else(|| "unknown".to_string());

            if WIRELESS_CHIP_PREFIXES
                .iter()
                .any(|p| chip_name.starts_with(p))
                || self.slow_chips.contains(&chip_name)
            {
                continue;
            }

            let is_cpu_chip = CPU_CHIP_NAMES.contains(&chip_name.as_str());
            let chip_start = Instant::now();
            let readings_before_chip = readings.len();
            let aliased_before_chip = (cpu_temp_aliased, cpu_fan_aliased);

            // Read temperatures (temp*_input files, millidegrees C)
            for i in 1..=16 {
                let input = hwmon_dir.join(format!("temp{}_input", i));
                if let Some(val_str) = Self::read_file_trimmed(&input)
                    && let Ok(millideg) = val_str.parse::<i64>()
                {
                    let label =
                        Self::read_file_trimmed(&hwmon_dir.join(format!("temp{}_label", i)))
                            .unwrap_or_else(|| format!("temp{}", i));
                    let key = format!(
                        "{}_{}_temp{}",
                        chip_name,
                        label.to_lowercase().replace(' ', "_"),
                        i
                    );
                    let deg = (millideg / 1000).to_string();
                    readings.push(SensorReading {
                        key,
                        value: deg.clone(),
                        unit: "°C".to_string(),
                    });
                    // Emit canonical alias for templates
                    if is_cpu_chip && !cpu_temp_aliased {
                        readings.push(SensorReading {
                            key: "cpu_temp".to_string(),
                            value: deg.clone(),
                            unit: "°C".to_string(),
                        });
                        cpu_temp_aliased = true;
                    }
                    // Per-core temp alias: "Core N" label → cpu_cN_temp
                    if is_cpu_chip {
                        if let Some(core_num) = parse_core_label(&label) {
                            readings.push(SensorReading {
                                key: format!("cpu_c{}_temp", core_num),
                                value: deg.clone(),
                                unit: "°C".to_string(),
                            });
                        }
                        // CCD temp alias: "TccdN" label → cpu_ccd{N-1}_temp
                        if let Some(ccd_idx) = parse_ccd_label(&label) {
                            readings.push(SensorReading {
                                key: format!("cpu_ccd{}_temp", ccd_idx),
                                value: deg,
                                unit: "°C".to_string(),
                            });
                        }
                    }
                }
            }

            // Read fan speeds (fan*_input files, RPM)
            for i in 1..=8 {
                let input = hwmon_dir.join(format!("fan{}_input", i));
                if let Some(val_str) = Self::read_file_trimmed(&input)
                    && let Ok(rpm) = val_str.parse::<u64>()
                {
                    let label = Self::read_file_trimmed(&hwmon_dir.join(format!("fan{}_label", i)))
                        .unwrap_or_else(|| format!("fan{}", i));
                    let key = format!(
                        "{}_{}_fan{}",
                        chip_name,
                        label.to_lowercase().replace(' ', "_"),
                        i
                    );
                    let rpm_str = rpm.to_string();
                    readings.push(SensorReading {
                        key,
                        value: rpm_str.clone(),
                        unit: "RPM".to_string(),
                    });
                    // Emit canonical alias for templates
                    if is_cpu_chip && !cpu_fan_aliased {
                        readings.push(SensorReading {
                            key: "cpu_fan".to_string(),
                            value: rpm_str,
                            unit: "RPM".to_string(),
                        });
                        cpu_fan_aliased = true;
                    }
                }
            }

            let chip_elapsed = chip_start.elapsed();
            if chip_elapsed > SLOW_CHIP_THRESHOLD {
                warn!(
                    "hwmon chip '{}' took {:?} to read (blocking driver call?); \
                     quarantining it for the rest of this run",
                    chip_name, chip_elapsed
                );
                // Drop this chip's readings so it never appears as a sensor,
                // and roll back alias flags it may have claimed.
                readings.truncate(readings_before_chip);
                (cpu_temp_aliased, cpu_fan_aliased) = aliased_before_chip;
                self.slow_chips.insert(chip_name);
            }
        }

        Ok(readings)
    }

    fn available_sensors(&self) -> Vec<SensorDescriptor> {
        // Discover by polling once — use a mutable clone to avoid borrow issues
        let mut probe = HwmonProvider {
            base_path: self.base_path.clone(),
            slow_chips: self.slow_chips.clone(),
        };
        match probe.poll() {
            Ok(readings) => readings
                .iter()
                .map(|r| SensorDescriptor {
                    key: r.key.clone(),
                    name: r.key.clone(),
                    unit: r.unit.clone(),
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// Parse "Core N" or "Core N" (case-insensitive) label → core index N.
/// Returns None if label doesn't match the pattern.
fn parse_core_label(label: &str) -> Option<u32> {
    let label = label.trim();
    let lower = label.to_lowercase();
    let rest = lower.strip_prefix("core ")?;
    rest.trim().parse::<u32>().ok()
}

/// Parse "TccdN" label → 0-indexed CCD index (Tccd1 → 0, Tccd2 → 1).
/// Returns None if label doesn't match the pattern.
fn parse_ccd_label(label: &str) -> Option<u32> {
    let label = label.trim();
    // Match case-insensitive "Tccd" prefix
    let lower = label.to_lowercase();
    let rest = lower.strip_prefix("tccd")?;
    let n: u32 = rest.trim().parse().ok()?;
    if n == 0 {
        return None;
    } // Tccd0 doesn't exist in practice; guard against underflow
    Some(n - 1)
}
