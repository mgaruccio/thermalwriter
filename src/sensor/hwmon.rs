// hwmon sensor provider: reads /sys/class/hwmon for CPU temperatures and fans.
//
// Adaptive pruning:
// - Hard denylist: wireless NICs (can block for seconds) + known high-latency
//   low-value chips (spd5118 DIMM thermals).
// - Slow quarantine: any chip > SLOW_CHIP_THRESHOLD once is dropped forever.
// - Needed-key filter: when the active layout declares which metrics it uses,
//   skip chips that cannot contribute those keys (after a short discovery
//   window that measures per-chip cost for the UI).
// - Cost-EMA skip: optional chips whose rolling average exceeds
//   ADAPTIVE_COST_SKIP are skipped unless a needed key maps to them.

use anyhow::Result;
use log::warn;
use std::collections::{HashMap, HashSet};
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

/// High-latency, low-value chips for a cooler LCD. JEDEC SPD5118 DIMM thermal
/// sensors take ~2 ms each via I2C and a typical board has four of them —
/// ~8 ms/poll for keys no stock layout displays. Skip by name.
const SKIP_CHIP_NAMES: &[&str] = &["spd5118"];

/// Any chip whose full read takes longer than this is quarantined for the
/// rest of the provider's lifetime. Normal sysfs sensor reads are microseconds;
/// crossing this means the read is blocking in a driver.
const SLOW_CHIP_THRESHOLD: Duration = Duration::from_millis(50);

/// Optional chips slower than this (EMA) are skipped when they don't serve a
/// needed key. 500µs catches nvme-style multi-ms family members without
/// touching k10temp/it8696 (~20µs).
const ADAPTIVE_COST_SKIP: Duration = Duration::from_micros(500);

/// Polls spent discovering chip costs before needed-key filtering kicks in.
const DISCOVERY_POLLS: u32 = 2;

pub struct HwmonProvider {
    base_path: PathBuf,
    /// Chips quarantined after a slow read; skipped on all subsequent polls.
    slow_chips: HashSet<String>,
    /// Exponential moving average of per-chip poll cost.
    chip_cost_ema: HashMap<String, Duration>,
    /// Last poll's per-chip cost (for hub attribution).
    last_chip_costs_us: Vec<(String, u64)>,
    /// Keys the active layout needs; None = full discovery.
    needed_keys: Option<HashSet<String>>,
    /// Number of polls completed (drives discovery window).
    polls_done: u32,
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
            chip_cost_ema: HashMap::new(),
            last_chip_costs_us: Vec::new(),
            needed_keys: None,
            polls_done: 0,
        }
    }

    /// For testing with a fake sysfs tree.
    pub fn with_base_path(base: PathBuf) -> Self {
        Self {
            base_path: base,
            slow_chips: HashSet::new(),
            chip_cost_ema: HashMap::new(),
            last_chip_costs_us: Vec::new(),
            needed_keys: None,
            polls_done: 0,
        }
    }

    fn read_file_trimmed(path: &std::path::Path) -> Option<String> {
        fs::read_to_string(path).ok().map(|s| s.trim().to_string())
    }

    fn is_hard_skipped(chip_name: &str) -> bool {
        WIRELESS_CHIP_PREFIXES
            .iter()
            .any(|p| chip_name.starts_with(p))
            || SKIP_CHIP_NAMES.contains(&chip_name)
    }

    fn is_cpu_chip(chip_name: &str) -> bool {
        CPU_CHIP_NAMES.contains(&chip_name)
    }

    /// Whether this chip could produce a key the layout needs.
    fn chip_serves_needed(&self, chip_name: &str) -> bool {
        let Some(needed) = &self.needed_keys else {
            return true;
        };
        if needed.is_empty() {
            return true;
        }
        // CPU chips produce cpu_temp / cpu_cN_temp / cpu_fan aliases.
        if Self::is_cpu_chip(chip_name) {
            return needed.iter().any(|k| {
                k == "cpu_temp"
                    || k == "cpu_fan"
                    || k.starts_with("cpu_c")
                    || k.starts_with("cpu_ccd")
                    || k.starts_with(chip_name)
            });
        }
        needed.iter().any(|k| k.starts_with(chip_name))
    }

    fn should_poll_chip(&self, chip_name: &str) -> bool {
        if Self::is_hard_skipped(chip_name) || self.slow_chips.contains(chip_name) {
            return false;
        }
        // Discovery window: measure everything once so the UI can show costs.
        if self.polls_done < DISCOVERY_POLLS || self.needed_keys.is_none() {
            return true;
        }
        if self.chip_serves_needed(chip_name) {
            return true;
        }
        // Optional + expensive → skip.
        match self.chip_cost_ema.get(chip_name) {
            Some(ema) if *ema >= ADAPTIVE_COST_SKIP => false,
            // Unknown cost: one more sample, then EMA decides.
            None => true,
            Some(_) => {
                // Cheap optional chip: still skip once we know the layout doesn't
                // need it — reading free chips still burns directory walks.
                false
            }
        }
    }

    fn update_ema(&mut self, chip: &str, sample: Duration) {
        let entry = self.chip_cost_ema.entry(chip.to_string()).or_insert(sample);
        // EMA α=0.4 — reacts quickly to the first few samples.
        let alpha = 0.4f64;
        let prev = entry.as_secs_f64();
        let next = alpha * sample.as_secs_f64() + (1.0 - alpha) * prev;
        *entry = Duration::from_secs_f64(next);
    }
}

impl SensorProvider for HwmonProvider {
    fn name(&self) -> &str {
        "hwmon"
    }

    fn set_needed_keys(&mut self, keys: Option<&HashSet<String>>) {
        self.needed_keys = keys.cloned();
    }

    fn last_source_costs_us(&self) -> Vec<(String, u64)> {
        self.last_chip_costs_us.clone()
    }

    fn poll(&mut self) -> Result<Vec<SensorReading>> {
        let mut readings = Vec::new();
        self.last_chip_costs_us.clear();
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

            if !self.should_poll_chip(&chip_name) {
                continue;
            }

            let is_cpu_chip = Self::is_cpu_chip(&chip_name);
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
            self.update_ema(&chip_name, chip_elapsed);
            self.last_chip_costs_us
                .push((chip_name.clone(), chip_elapsed.as_micros() as u64));

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

        self.polls_done = self.polls_done.saturating_add(1);
        Ok(readings)
    }

    fn available_sensors(&self) -> Vec<SensorDescriptor> {
        // Discover by polling once — use a mutable clone to avoid borrow issues
        let mut probe = HwmonProvider {
            base_path: self.base_path.clone(),
            slow_chips: self.slow_chips.clone(),
            chip_cost_ema: self.chip_cost_ema.clone(),
            last_chip_costs_us: Vec::new(),
            needed_keys: None, // full discovery for the catalog
            polls_done: 0,
        };
        match probe.poll() {
            Ok(readings) => {
                let costs: HashMap<&str, u64> = probe
                    .last_chip_costs_us
                    .iter()
                    .map(|(k, v)| (k.as_str(), *v))
                    .collect();
                readings
                    .iter()
                    .map(|r| {
                        let chip = r.key.split('_').next().unwrap_or("");
                        let cost = costs.get(chip).copied().unwrap_or(0);
                        SensorDescriptor {
                            key: r.key.clone(),
                            name: r.key.clone(),
                            unit: r.unit.clone(),
                            cost_us: cost,
                        }
                    })
                    .collect()
            }
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
    rest.parse().ok()
}

/// Parse "TccdN" label → zero-based CCD index (Tccd1 → 0).
fn parse_ccd_label(label: &str) -> Option<u32> {
    let label = label.trim();
    let lower = label.to_lowercase();
    let rest = lower.strip_prefix("tccd")?;
    let n: u32 = rest.parse().ok()?;
    n.checked_sub(1)
}
