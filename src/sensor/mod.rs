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
use std::collections::HashMap;

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
}

pub trait SensorProvider: Send {
    fn name(&self) -> &str;
    fn poll(&mut self) -> Result<Vec<SensorReading>>;
    fn available_sensors(&self) -> Vec<SensorDescriptor>;
}

/// Aggregates all sensor providers and exposes a flat key→value map.
pub struct SensorHub {
    providers: Vec<Box<dyn SensorProvider>>,
    /// Keys that have already produced a collision warning. Cleared never —
    /// one warn per key for the life of the hub keeps hybrid-GPU / multi-chip
    /// machines from flooding the journal every poll.
    collision_warned: std::collections::HashSet<String>,
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
            collision_warned: std::collections::HashSet::new(),
        }
    }

    pub fn add_provider(&mut self, provider: Box<dyn SensorProvider>) {
        self.providers.push(provider);
    }

    /// Poll all providers and return aggregated sensor data.
    ///
    /// Provider registration order is precedence: earlier providers win.
    /// Later providers that return a colliding key are ignored, and each
    /// colliding key is logged at `warn` at most once for the life of the hub.
    pub fn poll(&mut self) -> HashMap<String, String> {
        let mut data = HashMap::new();
        for provider in &mut self.providers {
            match provider.poll() {
                Ok(readings) => {
                    for reading in readings {
                        if data.contains_key(&reading.key) {
                            if self.collision_warned.insert(reading.key.clone()) {
                                log::warn!(
                                    "Ignoring sensor key '{}' from provider '{}' (earlier provider already owns it)",
                                    reading.key,
                                    provider.name()
                                );
                            }
                            continue;
                        }
                        data.insert(reading.key, reading.value);
                    }
                }
                Err(e) => {
                    log::warn!("Sensor provider '{}' failed: {}", provider.name(), e);
                }
            }
        }
        data
    }

    pub fn available_sensors(&self) -> Vec<SensorDescriptor> {
        self.providers
            .iter()
            .flat_map(|p| p.available_sensors())
            .collect()
    }
}
