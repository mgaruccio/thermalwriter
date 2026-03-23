// hwmon sensor provider: reads /sys/class/hwmon for CPU temperatures and power.

use std::fs;
use std::path::PathBuf;
use anyhow::Result;

use super::{SensorDescriptor, SensorProvider, SensorReading};

const DEFAULT_HWMON_PATH: &str = "/sys/class/hwmon";

pub struct HwmonProvider {
    base_path: PathBuf,
}

impl HwmonProvider {
    pub fn new() -> Self {
        Self { base_path: PathBuf::from(DEFAULT_HWMON_PATH) }
    }

    /// For testing with a fake sysfs tree.
    pub fn with_base_path(base: PathBuf) -> Self {
        Self { base_path: base }
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

        for entry in entries.flatten() {
            let hwmon_dir = entry.path();
            let chip_name = Self::read_file_trimmed(&hwmon_dir.join("name"))
                .unwrap_or_else(|| "unknown".to_string());

            // Read temperatures (temp*_input files, millidegrees C)
            for i in 1..=16 {
                let input = hwmon_dir.join(format!("temp{}_input", i));
                if let Some(val_str) = Self::read_file_trimmed(&input) {
                    if let Ok(millideg) = val_str.parse::<i64>() {
                        let label = Self::read_file_trimmed(&hwmon_dir.join(format!("temp{}_label", i)))
                            .unwrap_or_else(|| format!("temp{}", i));
                        let key = format!("{}_{}_temp{}", chip_name, label.to_lowercase().replace(' ', "_"), i);
                        readings.push(SensorReading {
                            key,
                            value: (millideg / 1000).to_string(),
                            unit: "°C".to_string(),
                        });
                    }
                }
            }

            // Read fan speeds (fan*_input files, RPM)
            for i in 1..=8 {
                let input = hwmon_dir.join(format!("fan{}_input", i));
                if let Some(val_str) = Self::read_file_trimmed(&input) {
                    if let Ok(rpm) = val_str.parse::<u64>() {
                        let label = Self::read_file_trimmed(&hwmon_dir.join(format!("fan{}_label", i)))
                            .unwrap_or_else(|| format!("fan{}", i));
                        let key = format!("{}_{}_fan{}", chip_name, label.to_lowercase().replace(' ', "_"), i);
                        readings.push(SensorReading {
                            key,
                            value: rpm.to_string(),
                            unit: "RPM".to_string(),
                        });
                    }
                }
            }
        }

        Ok(readings)
    }

    fn available_sensors(&self) -> Vec<SensorDescriptor> {
        // Discover by polling once — use a mutable clone to avoid borrow issues
        let mut probe = HwmonProvider::with_base_path(self.base_path.clone());
        match probe.poll() {
            Ok(readings) => readings.iter().map(|r| SensorDescriptor {
                key: r.key.clone(),
                name: r.key.clone(),
                unit: r.unit.clone(),
            }).collect(),
            Err(_) => Vec::new(),
        }
    }
}
