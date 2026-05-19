// Nvidia GPU sensor provider: reads metrics via nvidia-smi.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;
use anyhow::Result;
use wait_timeout::ChildExt;

use super::{SensorDescriptor, SensorProvider, SensorReading};

// 500ms is generous for a healthy GPU; a hung driver blocks indefinitely.
const NVIDIA_SMI_TIMEOUT: Duration = Duration::from_millis(500);

pub struct NvidiaProvider;

impl NvidiaProvider {
    pub fn new() -> Self {
        Self
    }
}

/// Parse one CSV line from nvidia-smi --format=csv,noheader,nounits output.
/// Fields: temperature.gpu, utilization.gpu, power.draw, memory.used, memory.total
/// Skips any field where the value is "N/A" (Optimus, driver hung, or unsupported query).
pub fn parse_csv_line(line: &str) -> Vec<SensorReading> {
    let mut readings = Vec::new();
    let fields: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    if fields.len() < 5 {
        return readings;
    }

    // temperature.gpu — skip if N/A
    if fields[0] != "N/A" {
        if let Ok(_) = fields[0].parse::<f64>() {
            readings.push(SensorReading {
                key: "gpu_temp".to_string(),
                value: fields[0].to_string(),
                unit: "°C".to_string(),
            });
        }
    }

    // utilization.gpu — skip if N/A
    if fields[1] != "N/A" {
        if let Ok(_) = fields[1].parse::<f64>() {
            readings.push(SensorReading {
                key: "gpu_util".to_string(),
                value: fields[1].to_string(),
                unit: "%".to_string(),
            });
        }
    }

    // power.draw (watts with decimals) — skip if N/A
    if fields[2] != "N/A" {
        if let Ok(w) = fields[2].parse::<f64>() {
            readings.push(SensorReading {
                key: "gpu_power".to_string(),
                value: format!("{:.0}", w),
                unit: "W".to_string(),
            });
        }
    }

    // memory.used (MiB → GB) — skip if N/A
    if fields[3] != "N/A" {
        if let Ok(mib) = fields[3].parse::<f64>() {
            readings.push(SensorReading {
                key: "vram_used".to_string(),
                value: format!("{:.1}", mib / 1024.0),
                unit: "GB".to_string(),
            });
        }
    }

    // memory.total (MiB → GB) — skip if N/A
    if fields[4] != "N/A" {
        if let Ok(mib) = fields[4].parse::<f64>() {
            readings.push(SensorReading {
                key: "vram_total".to_string(),
                value: format!("{:.1}", mib / 1024.0),
                unit: "GB".to_string(),
            });
        }
    }

    readings
}

impl SensorProvider for NvidiaProvider {
    fn name(&self) -> &str {
        "nvidia"
    }

    fn poll(&mut self) -> Result<Vec<SensorReading>> {
        let mut child = match Command::new("nvidia-smi")
            .args([
                "--query-gpu=temperature.gpu,utilization.gpu,power.draw,memory.used,memory.total",
                "--format=csv,noheader,nounits",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return Ok(Vec::new()), // nvidia-smi not installed
        };

        match child.wait_timeout(NVIDIA_SMI_TIMEOUT) {
            Ok(Some(status)) if status.success() => {
                let mut buf = String::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_string(&mut buf);
                }
                let line = buf.trim();
                if line.is_empty() {
                    Ok(Vec::new())
                } else {
                    Ok(parse_csv_line(line))
                }
            }
            Ok(Some(_)) => Ok(Vec::new()), // non-zero exit
            Ok(None) => {
                // Timed out — kill and reap so the process doesn't become a zombie.
                let _ = child.kill();
                let _ = child.wait();
                log::warn!(
                    "nvidia-smi timed out after {:?} — GPU may be in deep sleep or driver hung",
                    NVIDIA_SMI_TIMEOUT
                );
                Ok(Vec::new())
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                log::warn!("nvidia-smi wait failed: {}", e);
                Ok(Vec::new())
            }
        }
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
