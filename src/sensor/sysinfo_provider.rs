// SysinfoProvider: reads RAM and CPU metrics via the sysinfo crate.

use anyhow::Result;
use sysinfo::System;

use super::{SensorDescriptor, SensorProvider, SensorReading};

const BYTES_PER_GIB: f64 = 1_073_741_824.0;

pub struct SysinfoProvider {
    sys: System,
}

impl SysinfoProvider {
    pub fn new() -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();
        Self { sys }
    }
}

impl SensorProvider for SysinfoProvider {
    fn name(&self) -> &str {
        "sysinfo"
    }

    fn poll(&mut self) -> Result<Vec<SensorReading>> {
        self.sys.refresh_memory();
        self.sys.refresh_cpu_usage();

        let mut readings = Vec::new();

        // RAM used (bytes → GiB, 1 decimal)
        let used = self.sys.used_memory() as f64;
        readings.push(SensorReading {
            key: "ram_used".to_string(),
            value: format!("{:.1}", used / BYTES_PER_GIB),
            unit: "GB".to_string(),
        });

        // RAM total (bytes → GiB, 1 decimal)
        let total = self.sys.total_memory() as f64;
        readings.push(SensorReading {
            key: "ram_total".to_string(),
            value: format!("{:.1}", total / BYTES_PER_GIB),
            unit: "GB".to_string(),
        });

        // CPU utilization — average across all cores
        let cpus = self.sys.cpus();
        if !cpus.is_empty() {
            let avg = cpus.iter().map(|c| c.cpu_usage() as f64).sum::<f64>() / cpus.len() as f64;
            readings.push(SensorReading {
                key: "cpu_util".to_string(),
                value: format!("{:.1}", avg),
                unit: "%".to_string(),
            });
        }

        Ok(readings)
    }

    fn available_sensors(&self) -> Vec<SensorDescriptor> {
        vec![
            SensorDescriptor { key: "ram_used".to_string(), name: "RAM Used".to_string(), unit: "GB".to_string() },
            SensorDescriptor { key: "ram_total".to_string(), name: "RAM Total".to_string(), unit: "GB".to_string() },
            SensorDescriptor { key: "cpu_util".to_string(), name: "CPU Utilization".to_string(), unit: "%".to_string() },
        ]
    }
}
