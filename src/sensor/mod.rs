// Sensor system: SensorProvider trait and concrete sensor readers.
// Providers read system metrics (CPU/GPU temps, power, RAM, FPS).

pub mod amdgpu;
pub mod history;
pub mod hwmon;
pub mod mangohud;
#[doc(hidden)]
pub mod mock;
pub mod nvidia;
pub mod rapl;
pub mod sysinfo_provider;

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct SensorReading {
    pub key: String,
    pub value: String,
    pub unit: String,
}

#[derive(Debug, Clone)]
pub struct SensorDescriptor {
    pub key: String,
    pub name: String,
    pub unit: String,
    /// Last measured poll cost attributed to this key, in microseconds.
    /// 0 means unknown / not yet measured / effectively free.
    pub cost_us: u64,
}

impl SensorDescriptor {
    pub fn new(key: impl Into<String>, name: impl Into<String>, unit: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            name: name.into(),
            unit: unit.into(),
            cost_us: 0,
        }
    }
}

pub trait SensorProvider: Send {
    fn name(&self) -> &str;
    fn poll(&mut self) -> Result<Vec<SensorReading>>;
    fn available_sensors(&self) -> Vec<SensorDescriptor>;

    /// Optional: provider-specific timing breakdown (e.g. per hwmon chip).
    /// Default empty — hub attributes the whole provider duration to every key.
    fn last_source_costs_us(&self) -> Vec<(String, u64)> {
        Vec::new()
    }

    /// Optional: restrict expensive optional sources to keys the layout needs.
    /// Default no-op.
    fn set_needed_keys(&mut self, _keys: Option<&HashSet<String>>) {}
}

/// Per-provider wall time from the last hub poll.
#[derive(Debug, Clone, Default)]
pub struct ProviderPollStat {
    pub name: String,
    pub duration: Duration,
    pub keys_emitted: usize,
}

/// Aggregate timing from the last [`SensorHub::poll`].
#[derive(Debug, Clone, Default)]
pub struct SensorPollStats {
    pub total: Duration,
    pub providers: Vec<ProviderPollStat>,
    /// Per-key attributed cost in microseconds (last poll).
    pub key_cost_us: HashMap<String, u64>,
}

/// Aggregates all sensor providers and exposes a flat key→value map.
pub struct SensorHub {
    providers: Vec<Box<dyn SensorProvider>>,
    /// Keys that have already produced a collision warning. Cleared never —
    /// one warn per key for the life of the hub keeps hybrid-GPU / multi-chip
    /// machines from flooding the journal every poll.
    collision_warned: HashSet<String>,
    /// Keys the active layout actually uses (history + sensor vars). `None`
    /// means discovery mode — poll everything that isn't hard-denylisted.
    needed_keys: Option<HashSet<String>>,
    last_stats: SensorPollStats,
}

impl Default for SensorHub {
    fn default() -> Self {
        Self::new()
    }
}

impl SensorHub {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
            collision_warned: HashSet::new(),
            needed_keys: None,
            last_stats: SensorPollStats::default(),
        }
    }

    pub fn add_provider(&mut self, provider: Box<dyn SensorProvider>) {
        self.providers.push(provider);
    }

    /// Restrict expensive optional sources to keys required by the active layout.
    /// Pass `None` to return to full discovery polling.
    pub fn set_needed_keys(&mut self, keys: Option<HashSet<String>>) {
        self.needed_keys = keys;
        let view = self.needed_keys.as_ref();
        for provider in &mut self.providers {
            provider.set_needed_keys(view);
        }
    }

    pub fn needed_keys(&self) -> Option<&HashSet<String>> {
        self.needed_keys.as_ref()
    }

    pub fn last_poll_stats(&self) -> &SensorPollStats {
        &self.last_stats
    }

    /// Poll all providers and return aggregated sensor data.
    ///
    /// Provider registration order is precedence: earlier providers win.
    /// Later providers that return a colliding key are ignored, and each
    /// colliding key is logged at `warn` at most once for the life of the hub.
    pub fn poll(&mut self) -> HashMap<String, String> {
        let total_start = Instant::now();
        let mut data = HashMap::new();
        let mut key_owner: HashMap<String, String> = HashMap::new();
        let mut provider_stats = Vec::with_capacity(self.providers.len());
        let mut key_cost_us: HashMap<String, u64> = HashMap::new();

        // Re-push needed keys each poll so late-added providers see them.
        let needed_view = self.needed_keys.clone();
        for provider in &mut self.providers {
            provider.set_needed_keys(needed_view.as_ref());
        }

        for provider in &mut self.providers {
            let name = provider.name().to_string();
            let started = Instant::now();
            let result = provider.poll();
            let elapsed = started.elapsed();
            let source_costs = provider.last_source_costs_us();

            let mut keys_emitted = 0usize;
            match result {
                Ok(readings) => {
                    keys_emitted = readings.len();
                    let provider_us = elapsed.as_micros() as u64;
                    // Prefer fine-grained source costs (hwmon chips); else split
                    // provider cost across keys it actually contributed.
                    let contributed: Vec<String> = readings
                        .iter()
                        .filter(|r| !data.contains_key(&r.key))
                        .map(|r| r.key.clone())
                        .collect();

                    for reading in readings {
                        if data.contains_key(&reading.key) {
                            if self.collision_warned.insert(reading.key.clone()) {
                                log::warn!(
                                    "Ignoring sensor key '{}' from provider '{}' (earlier provider already owns it)",
                                    reading.key,
                                    name
                                );
                            }
                            continue;
                        }
                        key_owner.insert(reading.key.clone(), name.clone());
                        data.insert(reading.key, reading.value);
                    }

                    if !source_costs.is_empty() {
                        // source_costs are (source_id, us). Attribute each
                        // source's cost to keys whose owner source matches via
                        // prefix convention "source_id:" is not used — hwmon
                        // returns (chip_name, us) and keys contain chip_name.
                        for (source, cost) in &source_costs {
                            let matching: Vec<&String> = contributed
                                .iter()
                                .filter(|k| key_matches_source(k, source))
                                .collect();
                            if matching.is_empty() {
                                continue;
                            }
                            let share = cost / matching.len() as u64;
                            for k in matching {
                                *key_cost_us.entry(k.clone()).or_insert(0) += share.max(1);
                            }
                        }
                    } else if !contributed.is_empty() && provider_us > 0 {
                        let share = provider_us / contributed.len() as u64;
                        for k in &contributed {
                            *key_cost_us.entry(k.clone()).or_insert(0) += share.max(1);
                        }
                    }
                }
                Err(e) => {
                    log::warn!("Sensor provider '{}' failed: {}", name, e);
                }
            }

            provider_stats.push(ProviderPollStat {
                name,
                duration: elapsed,
                keys_emitted,
            });
        }

        self.last_stats = SensorPollStats {
            total: total_start.elapsed(),
            providers: provider_stats,
            key_cost_us,
        };

        data
    }

    pub fn available_sensors(&self) -> Vec<SensorDescriptor> {
        let costs = &self.last_stats.key_cost_us;
        self.providers
            .iter()
            .flat_map(|p| p.available_sensors())
            .map(|mut d| {
                if d.cost_us == 0 {
                    d.cost_us = costs.get(&d.key).copied().unwrap_or(0);
                }
                d
            })
            .collect()
    }

    /// Build the default desktop sensor stack (same order as the daemon).
    pub fn with_default_providers(mangohud_log_dir: &str) -> Self {
        let mut hub = Self::new();
        hub.add_provider(Box::new(hwmon::HwmonProvider::new()));
        hub.add_provider(Box::new(sysinfo_provider::SysinfoProvider::new()));
        // Nvidia before AmdGpu — hybrid machines report the discrete GPU.
        hub.add_provider(Box::new(nvidia::NvidiaProvider::new()));
        hub.add_provider(Box::new(amdgpu::AmdGpuProvider::new()));
        hub.add_provider(Box::new(mangohud::MangoHudProvider::from_configured_dir(
            mangohud_log_dir,
        )));
        hub.add_provider(Box::new(rapl::RaplProvider::new()));
        hub
    }
}

fn key_matches_source(key: &str, source: &str) -> bool {
    // hwmon keys are "{chip}_{label}_tempN" / "{chip}_{label}_fanN"
    // canonical aliases (cpu_temp) are attributed to CPU chips via source name.
    if key.starts_with(source) {
        return true;
    }
    if source == "k10temp"
        || source == "coretemp"
        || source == "zenpower"
        || source == "k8temp"
        || source == "fam15h_power"
    {
        return key == "cpu_temp"
            || key == "cpu_fan"
            || key.starts_with("cpu_c")
            || key.starts_with("cpu_ccd");
    }
    false
}

/// Canonical keys always considered "needed" so CPU/GPU/RAM keep polling even
/// when a layout's frontmatter is empty during discovery transitions.
pub fn default_needed_keys() -> HashSet<String> {
    [
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
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}
