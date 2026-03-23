// AmdGpu sensor provider: reads /sys/class/drm/card*/device for GPU metrics.

use std::fs;
use std::path::PathBuf;
use anyhow::Result;

use super::{SensorDescriptor, SensorProvider, SensorReading};

const DEFAULT_DRM_PATH: &str = "/sys/class/drm";
const BYTES_PER_GIB: f64 = 1_073_741_824.0;

pub struct AmdGpuProvider {
    base_path: PathBuf,
}

impl AmdGpuProvider {
    pub fn new() -> Self {
        Self { base_path: PathBuf::from(DEFAULT_DRM_PATH) }
    }

    /// For testing with a fake sysfs tree.
    pub fn with_base_path(base: PathBuf) -> Self {
        Self { base_path: base }
    }

    fn read_trimmed(path: &std::path::Path) -> Option<String> {
        fs::read_to_string(path).ok().map(|s| s.trim().to_string())
    }

    fn read_u64(path: &std::path::Path) -> Option<u64> {
        Self::read_trimmed(path)?.parse().ok()
    }
}

impl SensorProvider for AmdGpuProvider {
    fn name(&self) -> &str {
        "amdgpu"
    }

    fn poll(&mut self) -> Result<Vec<SensorReading>> {
        let mut readings = Vec::new();

        // Scan all card* directories
        let entries = match fs::read_dir(&self.base_path) {
            Ok(e) => e,
            Err(_) => return Ok(readings), // Missing sysfs — return empty, not error
        };

        for entry in entries.flatten() {
            let card_name = entry.file_name();
            let card_str = card_name.to_string_lossy();

            // Only process card directories (card0, card1, etc.), not renderD* etc.
            if !card_str.starts_with("card") || card_str.contains('-') {
                continue;
            }

            let device_dir = entry.path().join("device");
            if !device_dir.exists() {
                continue;
            }

            // GPU utilization
            if let Some(val) = Self::read_trimmed(&device_dir.join("gpu_busy_percent")) {
                readings.push(SensorReading {
                    key: "gpu_util".to_string(),
                    value: val,
                    unit: "%".to_string(),
                });
            }

            // VRAM used (bytes → GiB, 1 decimal)
            if let Some(bytes) = Self::read_u64(&device_dir.join("mem_info_vram_used")) {
                readings.push(SensorReading {
                    key: "vram_used".to_string(),
                    value: format!("{:.1}", bytes as f64 / BYTES_PER_GIB),
                    unit: "GB".to_string(),
                });
            }

            // VRAM total (bytes → GiB, 1 decimal)
            if let Some(bytes) = Self::read_u64(&device_dir.join("mem_info_vram_total")) {
                readings.push(SensorReading {
                    key: "vram_total".to_string(),
                    value: format!("{:.1}", bytes as f64 / BYTES_PER_GIB),
                    unit: "GB".to_string(),
                });
            }

            // hwmon subdir for power and temperature
            let hwmon_base = device_dir.join("hwmon");
            if let Ok(hwmon_entries) = fs::read_dir(&hwmon_base) {
                for hwmon_entry in hwmon_entries.flatten() {
                    let hwmon_dir = hwmon_entry.path();

                    // Power (microwatts → watts, integer)
                    if let Some(uw) = Self::read_u64(&hwmon_dir.join("power1_average")) {
                        readings.push(SensorReading {
                            key: "gpu_power".to_string(),
                            value: (uw / 1_000_000).to_string(),
                            unit: "W".to_string(),
                        });
                    }

                    // Temperature (millidegrees → degrees, integer)
                    if let Some(millideg) = Self::read_u64(&hwmon_dir.join("temp1_input")) {
                        readings.push(SensorReading {
                            key: "gpu_temp".to_string(),
                            value: (millideg / 1000).to_string(),
                            unit: "°C".to_string(),
                        });
                    }
                }
            }

            // Only read the first valid card (one GPU)
            break;
        }

        Ok(readings)
    }

    fn available_sensors(&self) -> Vec<SensorDescriptor> {
        let mut probe = AmdGpuProvider::with_base_path(self.base_path.clone());
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
