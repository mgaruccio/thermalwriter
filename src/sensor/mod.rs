// Sensor system: SensorProvider trait and concrete sensor readers.
// Providers read system metrics (CPU/GPU temps, power, RAM, FPS).

pub mod amdgpu;
pub mod history;
pub mod hwmon;
pub mod llm;
pub mod mangohud;
#[doc(hidden)]
pub mod mock;
pub mod needed_keys;
pub mod nvidia;
pub mod rapl;
pub use needed_keys::{LayoutSensorRecipe, layout_needed_keys};
pub mod sysinfo_provider;

use anyhow::Result;
use log::debug;
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
    /// Canonical keys this provider *can* emit, regardless of whether they are
    /// currently readable. Used for layout-needed-key derivation so that a
    /// temporarily-unreadable sensor (e.g. RAPL during early boot) stays
    /// eligible for polling when a layout references it.
    /// Default: empty (provider only declares keys via `available_sensors`).
    fn declared_keys(&self) -> Vec<&str> {
        Vec::new()
    }

    /// Returns true if this provider could emit any of the needed keys.
    /// Used by the hub to skip providers that can't contribute.
    /// Default: `true` (conservative — never skip unless overridden).
    /// Override with a static key check when keys are known at compile time.
    fn wants_any(&self, _needed: &HashSet<String>) -> bool {
        true
    }

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
    /// Catalog of known sensors with last attributed costs. Built once from
    /// provider discovery, then only cost fields are refreshed after each poll
    /// (never re-polls providers just to list sensors).
    last_catalog: Vec<SensorDescriptor>,
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
            last_catalog: Vec::new(),
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

            // Short-circuit: skip providers that can't contribute any *remaining* needed key.
            if let Some(ref n) = self.needed_keys {
                // Compute keys still missing from collected data.
                let remaining: HashSet<String> = n
                    .iter()
                    .filter(|k| !data.contains_key(*k))
                    .cloned()
                    .collect();
                if remaining.is_empty() {
                    debug!("All needed keys collected; stopping provider poll");
                    break;
                }
                // Skip providers whose static keys don't intersect with remaining needed.
                // hwmon returns empty provided_keys() (dynamic) → wants_any returns
                // true → never skipped here (it chip-filters internally).
                if !provider.wants_any(&remaining) {
                    debug!(
                        "Skipping provider '{}' (no remaining needed keys intersect)",
                        name
                    );
                    provider_stats.push(ProviderPollStat {
                        name,
                        duration: Duration::ZERO,
                        keys_emitted: 0,
                    });
                    continue;
                }
            }

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
            key_cost_us: key_cost_us.clone(),
        };

        // One-time discovery of names/units; subsequent polls only refresh costs.
        if self.last_catalog.is_empty() {
            self.last_catalog = self.discover_catalog();
        }
        // Merge any newly seen keys from this poll into the catalog.
        for key in data.keys() {
            if !self.last_catalog.iter().any(|d| d.key == *key) {
                self.last_catalog.push(SensorDescriptor {
                    key: key.clone(),
                    name: key.clone(),
                    unit: String::new(),
                    cost_us: 0,
                });
            }
        }
        for d in &mut self.last_catalog {
            d.cost_us = key_cost_us.get(&d.key).copied().unwrap_or(0);
        }

        data
    }

    /// Expensive: asks each provider for its descriptor list (some re-poll).
    /// Only used to seed [`last_catalog`] once.
    fn discover_catalog(&self) -> Vec<SensorDescriptor> {
        self.providers
            .iter()
            .flat_map(|p| p.available_sensors())
            .collect()
    }

    pub fn available_sensors(&self) -> Vec<SensorDescriptor> {
        if !self.last_catalog.is_empty() {
            return self.last_catalog.clone();
        }
        // Pre-first-poll fallback (e.g. unit tests that never called poll).
        self.discover_catalog()
    }
    /// Canonical keys declared by providers (regardless of current readability).
    /// Used for layout-needed-key token scanning so a transiently-unreadable
    /// sensor (e.g. RAPL's `cpu_power`) stays eligible when a layout references it.
    pub fn declared_keys(&self) -> HashSet<String> {
        self.providers
            .iter()
            .flat_map(|p| p.declared_keys())
            .map(String::from)
            .collect()
    }

    /// Build the default desktop sensor stack (same order as the daemon).
    pub fn with_default_providers(mangohud_log_dir: &str) -> Self {
        Self::with_default_providers_config(
            mangohud_log_dir,
            &crate::config::LlmSensorConfig::default(),
        )
    }

    /// Build the default provider stack with daemon sensor configuration.
    pub fn with_default_providers_config(
        mangohud_log_dir: &str,
        llm_config: &crate::config::LlmSensorConfig,
    ) -> Self {
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
        hub.add_provider(Box::new(llm::LlmProvider::from_config(llm_config)));
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

// Needed-key computation now lives in `needed_keys::layout_needed_keys`, which
// derives the needed set from the active layout's frontmatter + template tokens
// against the known sensor catalog. `default_needed_keys` (the old fixed desktop
// set) is removed — see `layout_needed_keys` for the replacement.
