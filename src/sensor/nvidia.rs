// Nvidia GPU sensor provider: prefers NVML, falls back to nvidia-smi.
//
// Forking nvidia-smi every poll costs ~15 ms of process overhead on this
// hardware and dominates steady-state daemon CPU. NVML is the same data over
// a long-lived dlopen'd library handle. When neither path is available we
// back off instead of retrying a failed exec every second (#80 / #91).

use anyhow::Result;
use nvml_wrapper::Nvml;
use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

use super::{SensorDescriptor, SensorProvider, SensorReading};

// 500ms is generous for a healthy GPU; a hung driver blocks indefinitely.
const NVIDIA_SMI_TIMEOUT: Duration = Duration::from_millis(500);
/// How long to wait before re-probing after NVML and nvidia-smi both fail.
const UNAVAILABLE_BACKOFF: Duration = Duration::from_secs(60);

enum Backend {
    /// Live NVML handle (libnvidia-ml loaded via libloading).
    Nvml(Nvml),
    /// Fork nvidia-smi each poll.
    Smi,
    /// Neither path worked; retry after `retry_at`.
    Unavailable { retry_at: Instant },
}

pub struct NvidiaProvider {
    backend: Backend,
    /// Executable used for the smi fallback. Overridable for tests.
    smi_path: PathBuf,
}

impl Default for NvidiaProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl NvidiaProvider {
    pub fn new() -> Self {
        Self::with_smi_path(PathBuf::from("nvidia-smi"))
    }

    /// Construct a provider that uses `smi_path` when NVML is unavailable.
    /// Tests that need to shim the binary should prefer [`Self::smi_only`].
    pub fn with_smi_path(smi_path: PathBuf) -> Self {
        let backend = probe_backend(&smi_path);
        Self { backend, smi_path }
    }

    /// Force the nvidia-smi backend (skip NVML). Used by tests that inject a
    /// shim binary via PATH or an absolute path.
    pub fn smi_only(smi_path: PathBuf) -> Self {
        Self {
            backend: Backend::Smi,
            smi_path,
        }
    }

    fn reprobe_if_due(&mut self) {
        let Backend::Unavailable { retry_at } = &self.backend else {
            return;
        };
        if Instant::now() < *retry_at {
            return;
        }
        self.backend = probe_backend(&self.smi_path);
    }
}

fn probe_backend(smi_path: &Path) -> Backend {
    match Nvml::init() {
        Ok(nvml) => {
            // Confirm at least one device is addressable; otherwise fall through.
            match nvml.device_count() {
                Ok(n) if n > 0 => Backend::Nvml(nvml),
                _ => probe_smi(smi_path),
            }
        }
        Err(_) => probe_smi(smi_path),
    }
}

fn probe_smi(smi_path: &Path) -> Backend {
    // Cheap existence check — avoid forking every poll when the binary is gone.
    let available = if smi_path.is_absolute() {
        smi_path.is_file()
    } else {
        which_in_path(smi_path)
    };
    if available {
        Backend::Smi
    } else {
        Backend::Unavailable {
            retry_at: Instant::now() + UNAVAILABLE_BACKOFF,
        }
    }
}

fn which_in_path(command: &Path) -> bool {
    let Some(name) = command.to_str() else {
        return false;
    };
    let Ok(path_var) = std::env::var("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return true;
        }
    }
    false
}

fn poll_nvml(nvml: &Nvml) -> Result<Vec<SensorReading>> {
    let device = match nvml.device_by_index(0) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("NVML device_by_index(0) failed: {e}");
            return Ok(Vec::new());
        }
    };

    let mut readings = Vec::with_capacity(5);

    match device.temperature(TemperatureSensor::Gpu) {
        Ok(temp) => readings.push(SensorReading {
            key: "gpu_temp".into(),
            value: temp.to_string(),
            unit: "°C".into(),
        }),
        Err(e) => log::debug!("NVML temperature unavailable: {e}"),
    }

    match device.utilization_rates() {
        Ok(util) => readings.push(SensorReading {
            key: "gpu_util".into(),
            value: util.gpu.to_string(),
            unit: "%".into(),
        }),
        Err(e) => log::debug!("NVML utilization unavailable: {e}"),
    }

    match device.power_usage() {
        Ok(mw) => {
            let watts = f64::from(mw) / 1000.0;
            readings.push(SensorReading {
                key: "gpu_power".into(),
                value: format!("{:.0}", watts),
                unit: "W".into(),
            });
        }
        Err(e) => log::debug!("NVML power_usage unavailable: {e}"),
    }

    match device.memory_info() {
        Ok(mem) => {
            // NVML reports bytes; match nvidia-smi MiB→GB formatting (1 decimal).
            let used_mib = mem.used as f64 / (1024.0 * 1024.0);
            let total_mib = mem.total as f64 / (1024.0 * 1024.0);
            readings.push(SensorReading {
                key: "vram_used".into(),
                value: format!("{:.1}", used_mib / 1024.0),
                unit: "GB".into(),
            });
            readings.push(SensorReading {
                key: "vram_total".into(),
                value: format!("{:.1}", total_mib / 1024.0),
                unit: "GB".into(),
            });
        }
        Err(e) => log::debug!("NVML memory_info unavailable: {e}"),
    }

    Ok(readings)
}

fn poll_smi(smi_path: &Path) -> Result<Vec<SensorReading>> {
    let mut child = match Command::new(smi_path)
        .args([
            "--query-gpu=temperature.gpu,utilization.gpu,power.draw,memory.used,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return Ok(Vec::new()), // binary vanished between probe and poll
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
    if fields[0] != "N/A" && fields[0].parse::<f64>().is_ok() {
        readings.push(SensorReading {
            key: "gpu_temp".to_string(),
            value: fields[0].to_string(),
            unit: "°C".to_string(),
        });
    }

    // utilization.gpu — skip if N/A
    if fields[1] != "N/A" && fields[1].parse::<f64>().is_ok() {
        readings.push(SensorReading {
            key: "gpu_util".to_string(),
            value: fields[1].to_string(),
            unit: "%".to_string(),
        });
    }

    // power.draw (watts with decimals) — skip if N/A
    if fields[2] != "N/A"
        && let Ok(w) = fields[2].parse::<f64>()
    {
        readings.push(SensorReading {
            key: "gpu_power".to_string(),
            value: format!("{:.0}", w),
            unit: "W".to_string(),
        });
    }

    // memory.used (MiB → GB) — skip if N/A
    if fields[3] != "N/A"
        && let Ok(mib) = fields[3].parse::<f64>()
    {
        readings.push(SensorReading {
            key: "vram_used".to_string(),
            value: format!("{:.1}", mib / 1024.0),
            unit: "GB".to_string(),
        });
    }

    // memory.total (MiB → GB) — skip if N/A
    if fields[4] != "N/A"
        && let Ok(mib) = fields[4].parse::<f64>()
    {
        readings.push(SensorReading {
            key: "vram_total".to_string(),
            value: format!("{:.1}", mib / 1024.0),
            unit: "GB".to_string(),
        });
    }

    readings
}

impl SensorProvider for NvidiaProvider {
    fn name(&self) -> &str {
        "nvidia"
    }

    fn poll(&mut self) -> Result<Vec<SensorReading>> {
        self.reprobe_if_due();

        match &self.backend {
            Backend::Nvml(nvml) => match poll_nvml(nvml) {
                Ok(readings) => Ok(readings),
                Err(e) => {
                    log::warn!("NVML poll failed ({e:#}); falling back to nvidia-smi probe");
                    self.backend = probe_smi(&self.smi_path);
                    match &self.backend {
                        Backend::Smi => poll_smi(&self.smi_path),
                        Backend::Unavailable { .. } => Ok(Vec::new()),
                        Backend::Nvml(_) => unreachable!("probe_smi never returns Nvml"),
                    }
                }
            },
            Backend::Smi => {
                let readings = poll_smi(&self.smi_path)?;
                // If the binary disappeared, demote to unavailable with backoff.
                if readings.is_empty() {
                    let still_there = if self.smi_path.is_absolute() {
                        self.smi_path.is_file()
                    } else {
                        which_in_path(&self.smi_path)
                    };
                    if !still_there {
                        self.backend = Backend::Unavailable {
                            retry_at: Instant::now() + UNAVAILABLE_BACKOFF,
                        };
                    }
                }
                Ok(readings)
            }
            Backend::Unavailable { .. } => Ok(Vec::new()),
        }
    }

    fn available_sensors(&self) -> Vec<SensorDescriptor> {
        vec![
            SensorDescriptor {
                key: "gpu_temp".into(),
                name: "GPU Temperature".into(),
                unit: "°C".into(),
            },
            SensorDescriptor {
                key: "gpu_util".into(),
                name: "GPU Utilization".into(),
                unit: "%".into(),
            },
            SensorDescriptor {
                key: "gpu_power".into(),
                name: "GPU Power".into(),
                unit: "W".into(),
            },
            SensorDescriptor {
                key: "vram_used".into(),
                name: "VRAM Used".into(),
                unit: "GB".into(),
            },
            SensorDescriptor {
                key: "vram_total".into(),
                name: "VRAM Total".into(),
                unit: "GB".into(),
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_backend_skips_spawn_until_backoff() {
        // Point at a path that cannot exist; provider must not panic and must
        // stay quiet across repeated polls inside the backoff window.
        let missing = PathBuf::from("/tmp/thermalwriter-definitely-missing-nvidia-smi");
        let mut provider = NvidiaProvider {
            backend: Backend::Unavailable {
                retry_at: Instant::now() + Duration::from_secs(3600),
            },
            smi_path: missing,
        };
        for _ in 0..5 {
            let readings = provider.poll().unwrap();
            assert!(readings.is_empty());
            assert!(matches!(provider.backend, Backend::Unavailable { .. }));
        }
    }

    #[test]
    fn smi_only_uses_injected_path() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("nvidia-smi");
        // The shim exits 0 with valid CSV so poll returns readings.
        std::fs::write(&shim, "#!/bin/sh\necho '55, 10, 100.0, 1024, 8192'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&shim).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&shim, perms).unwrap();
        }

        let mut provider = NvidiaProvider::smi_only(shim);
        let readings = provider.poll().unwrap();
        assert!(
            readings
                .iter()
                .any(|r| r.key == "gpu_temp" && r.value == "55"),
            "smi_only must parse shim output: {:?}",
            readings
        );
    }
}
