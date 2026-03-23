// Sensor system: SensorProvider trait and concrete sensor readers.
// Providers read system metrics (CPU/GPU temps, power, RAM, FPS).

pub mod hwmon;
pub mod amdgpu;
pub mod nvidia;
pub mod sysinfo_provider;
pub mod mangohud;

use std::collections::HashMap;
use anyhow::Result;

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
}

impl SensorHub {
    pub fn new() -> Self {
        Self { providers: Vec::new() }
    }

    pub fn add_provider(&mut self, provider: Box<dyn SensorProvider>) {
        self.providers.push(provider);
    }

    /// Poll all providers and return aggregated sensor data.
    pub fn poll(&mut self) -> HashMap<String, String> {
        let mut data = HashMap::new();
        for provider in &mut self.providers {
            match provider.poll() {
                Ok(readings) => {
                    for reading in readings {
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
        self.providers.iter().flat_map(|p| p.available_sensors()).collect()
    }
}
