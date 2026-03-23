// Nvidia GPU sensor provider: reads metrics via nvidia-smi.

use std::process::Command;
use anyhow::Result;

use super::{SensorDescriptor, SensorProvider, SensorReading};

pub struct NvidiaProvider;

impl NvidiaProvider {
    pub fn new() -> Self {
        Self
    }
}

impl SensorProvider for NvidiaProvider {
    fn name(&self) -> &str {
        "nvidia"
    }

    fn poll(&mut self) -> Result<Vec<SensorReading>> {
        let mut readings = Vec::new();

        let output = match Command::new("nvidia-smi")
            .args([
                "--query-gpu=temperature.gpu,utilization.gpu,power.draw,memory.used,memory.total",
                "--format=csv,noheader,nounits",
            ])
            .output()
        {
            Ok(o) if o.status.success() => o,
            _ => return Ok(readings), // nvidia-smi not available or failed
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout.trim();
        if line.is_empty() {
            return Ok(readings);
        }

        let fields: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if fields.len() < 5 {
            return Ok(readings);
        }

        // temperature.gpu
        if let Ok(_) = fields[0].parse::<f64>() {
            readings.push(SensorReading {
                key: "gpu_temp".to_string(),
                value: fields[0].to_string(),
                unit: "°C".to_string(),
            });
        }

        // utilization.gpu
        if let Ok(_) = fields[1].parse::<f64>() {
            readings.push(SensorReading {
                key: "gpu_util".to_string(),
                value: fields[1].to_string(),
                unit: "%".to_string(),
            });
        }

        // power.draw (already in watts with decimals)
        if let Ok(w) = fields[2].parse::<f64>() {
            readings.push(SensorReading {
                key: "gpu_power".to_string(),
                value: format!("{:.0}", w),
                unit: "W".to_string(),
            });
        }

        // memory.used (MiB → GB with 1 decimal)
        if let Ok(mib) = fields[3].parse::<f64>() {
            readings.push(SensorReading {
                key: "vram_used".to_string(),
                value: format!("{:.1}", mib / 1024.0),
                unit: "GB".to_string(),
            });
        }

        // memory.total (MiB → GB with 1 decimal)
        if let Ok(mib) = fields[4].parse::<f64>() {
            readings.push(SensorReading {
                key: "vram_total".to_string(),
                value: format!("{:.1}", mib / 1024.0),
                unit: "GB".to_string(),
            });
        }

        Ok(readings)
    }

    fn available_sensors(&self) -> Vec<SensorDescriptor> {
        vec![
            SensorDescriptor { key: "gpu_temp".into(), name: "GPU Temperature".into(), unit: "°C".into() },
            SensorDescriptor { key: "gpu_util".into(), name: "GPU Utilization".into(), unit: "%".into() },
            SensorDescriptor { key: "gpu_power".into(), name: "GPU Power".into(), unit: "W".into() },
            SensorDescriptor { key: "vram_used".into(), name: "VRAM Used".into(), unit: "GB".into() },
            SensorDescriptor { key: "vram_total".into(), name: "VRAM Total".into(), unit: "GB".into() },
        ]
    }
}
