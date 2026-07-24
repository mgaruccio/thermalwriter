// Nvidia GPU sensor provider: prefers NVML, falls back to nvidia-smi.
//
// Forking nvidia-smi every poll costs ~15 ms of process overhead on this
// hardware and dominates steady-state daemon CPU. NVML is the same data over
// a long-lived dlopen'd library handle (~µs per query). When neither path is
// available we back off instead of retrying a failed exec every second
// (#80 / #91).
//
// Hung-driver policy: nvidia-smi still uses wait_timeout(500ms)+kill. NVML
// calls are not cancellable; we measure wall time after each poll and demote
// to smi/backoff if a call ever exceeds the same 500 ms budget (so a once-
// wedged driver does not keep us on the slow path after it recovers). A call
// that never returns still blocks sensor_hub.poll — same class of failure as
// a wedged hwmon read — but no longer forks a process every second on the
// healthy path.

use anyhow::Result;
use nvml_wrapper::Nvml;
use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

use super::{SensorDescriptor, SensorProvider, SensorReading};

const NVIDIA_POLL_BUDGET: Duration = Duration::from_millis(500);
const REPROBE_BACKOFF: Duration = Duration::from_secs(60);
const SMI_EMPTY_FAIL_LIMIT: u32 = 3;

enum Backend {
    Nvml(Nvml),
    Smi {
        reprobe_at: Instant,
        empty_streak: u32,
    },
    Unavailable {
        retry_at: Instant,
    },
}

pub struct NvidiaProvider {
    backend: Backend,
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

    pub fn with_smi_path(smi_path: PathBuf) -> Self {
        Self {
            backend: probe_backend(&smi_path),
            smi_path,
        }
    }

    /// Force the nvidia-smi backend (skip NVML). Used by tests that inject a
    /// shim binary via PATH or an absolute path.
    pub fn smi_only(smi_path: PathBuf) -> Self {
        Self {
            backend: Backend::Smi {
                reprobe_at: Instant::now() + REPROBE_BACKOFF,
                empty_streak: 0,
            },
            smi_path,
        }
    }

    fn enter_smi(&mut self) {
        self.backend = Backend::Smi {
            reprobe_at: Instant::now() + REPROBE_BACKOFF,
            empty_streak: 0,
        };
    }

    fn enter_unavailable(&mut self) {
        self.backend = Backend::Unavailable {
            retry_at: Instant::now() + REPROBE_BACKOFF,
        };
    }

    fn demote_nvml(&mut self, reason: &str) {
        log::warn!("{reason}; falling back from NVML");
        if smi_present(&self.smi_path) {
            self.enter_smi();
        } else {
            self.enter_unavailable();
        }
    }

    fn maybe_reprobe(&mut self) {
        match &self.backend {
            Backend::Unavailable { retry_at } if Instant::now() >= *retry_at => {
                self.backend = probe_backend(&self.smi_path);
            }
            Backend::Smi { reprobe_at, .. } if Instant::now() >= *reprobe_at => {
                match try_init_nvml() {
                    Some(nvml) => {
                        log::info!("NVML became available; leaving nvidia-smi fallback");
                        self.backend = Backend::Nvml(nvml);
                    }
                    None => {
                        if let Backend::Smi { reprobe_at, .. } = &mut self.backend {
                            *reprobe_at = Instant::now() + REPROBE_BACKOFF;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn try_init_nvml() -> Option<Nvml> {
    match Nvml::init() {
        Ok(nvml) => match nvml.device_count() {
            Ok(n) if n > 0 => Some(nvml),
            Ok(_) => {
                log::debug!("NVML init ok but device_count=0");
                None
            }
            Err(e) => {
                log::debug!("NVML device_count failed: {e}");
                None
            }
        },
        Err(e) => {
            log::debug!("NVML init failed: {e}");
            None
        }
    }
}

fn smi_present(smi_path: &Path) -> bool {
    if smi_path.is_absolute() {
        smi_path.is_file()
    } else {
        which_in_path(smi_path)
    }
}

fn probe_backend(smi_path: &Path) -> Backend {
    if let Some(nvml) = try_init_nvml() {
        Backend::Nvml(nvml)
    } else if smi_present(smi_path) {
        Backend::Smi {
            reprobe_at: Instant::now() + REPROBE_BACKOFF,
            empty_streak: 0,
        }
    } else {
        Backend::Unavailable {
            retry_at: Instant::now() + REPROBE_BACKOFF,
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
        if dir.join(name).is_file() {
            return true;
        }
    }
    false
}

enum NvmlPollOutcome {
    Ok(Vec<SensorReading>),
    Empty,
    /// Device/library failure — caller must demote off NVML.
    Fatal(String),
}

fn collect_nvml_readings(nvml: &Nvml) -> NvmlPollOutcome {
    let device = match nvml.device_by_index(0) {
        Ok(d) => d,
        Err(e) => return NvmlPollOutcome::Fatal(format!("NVML device_by_index(0) failed: {e}")),
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

    if readings.is_empty() {
        NvmlPollOutcome::Empty
    } else {
        NvmlPollOutcome::Ok(readings)
    }
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
        Err(_) => return Ok(Vec::new()),
    };

    match child.wait_timeout(NVIDIA_POLL_BUDGET) {
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
        Ok(Some(_)) => Ok(Vec::new()),
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            log::warn!(
                "nvidia-smi timed out after {:?} — GPU may be in deep sleep or driver hung",
                NVIDIA_POLL_BUDGET
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

    if fields[0] != "N/A" && fields[0].parse::<f64>().is_ok() {
        readings.push(SensorReading {
            key: "gpu_temp".to_string(),
            value: fields[0].to_string(),
            unit: "°C".to_string(),
        });
    }

    if fields[1] != "N/A" && fields[1].parse::<f64>().is_ok() {
        readings.push(SensorReading {
            key: "gpu_util".to_string(),
            value: fields[1].to_string(),
            unit: "%".to_string(),
        });
    }

    if fields[2] != "N/A"
        && let Ok(w) = fields[2].parse::<f64>()
    {
        readings.push(SensorReading {
            key: "gpu_power".to_string(),
            value: format!("{:.0}", w),
            unit: "W".to_string(),
        });
    }

    if fields[3] != "N/A"
        && let Ok(mib) = fields[3].parse::<f64>()
    {
        readings.push(SensorReading {
            key: "vram_used".to_string(),
            value: format!("{:.1}", mib / 1024.0),
            unit: "GB".to_string(),
        });
    }

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
        self.maybe_reprobe();

        // NVML path: borrow the handle, collect, then decide demotion without
        // holding the borrow.
        if matches!(self.backend, Backend::Nvml(_)) {
            let started = Instant::now();
            let outcome = {
                let Backend::Nvml(nvml) = &self.backend else {
                    unreachable!()
                };
                collect_nvml_readings(nvml)
            };
            let elapsed = started.elapsed();

            // Soft budget: if NVML ever runs longer than the old smi timeout,
            // demote so a recovering-but-slow driver does not keep us hot.
            if elapsed > NVIDIA_POLL_BUDGET {
                self.demote_nvml(&format!(
                    "NVML poll took {elapsed:?} (budget {NVIDIA_POLL_BUDGET:?})"
                ));
                return self.poll_via_smi_backend();
            }

            return match outcome {
                NvmlPollOutcome::Ok(readings) => Ok(readings),
                NvmlPollOutcome::Empty => Ok(Vec::new()),
                NvmlPollOutcome::Fatal(reason) => {
                    self.demote_nvml(&reason);
                    self.poll_via_smi_backend()
                }
            };
        }

        match self.backend {
            Backend::Smi { .. } => self.poll_via_smi_backend(),
            Backend::Unavailable { .. } => Ok(Vec::new()),
            Backend::Nvml(_) => unreachable!("handled above"),
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

impl NvidiaProvider {
    fn poll_via_smi_backend(&mut self) -> Result<Vec<SensorReading>> {
        if !matches!(self.backend, Backend::Smi { .. }) {
            if smi_present(&self.smi_path) {
                self.enter_smi();
            } else {
                self.enter_unavailable();
                return Ok(Vec::new());
            }
        }

        let readings = poll_smi(&self.smi_path)?;

        if let Backend::Smi { empty_streak, .. } = &mut self.backend {
            if readings.is_empty() {
                *empty_streak = empty_streak.saturating_add(1);
                let streak = *empty_streak;
                let gone = !smi_present(&self.smi_path);
                if gone || streak >= SMI_EMPTY_FAIL_LIMIT {
                    log::warn!(
                        "nvidia-smi unusable (empty_streak={streak}, present={}); backing off",
                        !gone
                    );
                    self.enter_unavailable();
                }
            } else {
                *empty_streak = 0;
            }
        }

        Ok(readings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_backend_skips_spawn_until_backoff() {
        let missing = PathBuf::from("/tmp/thermalwriter-definitely-missing-nvidia-smi");
        let mut provider = NvidiaProvider {
            backend: Backend::Unavailable {
                retry_at: Instant::now() + Duration::from_secs(3600),
            },
            smi_path: missing,
        };
        for _ in 0..5 {
            assert!(provider.poll().unwrap().is_empty());
            assert!(matches!(provider.backend, Backend::Unavailable { .. }));
        }
    }

    #[test]
    fn smi_only_uses_injected_path() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("nvidia-smi");
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
        assert!(matches!(provider.backend, Backend::Smi { .. }));
    }

    #[test]
    fn smi_empty_streak_enters_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("nvidia-smi");
        std::fs::write(&shim, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&shim).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&shim, perms).unwrap();
        }

        let mut provider = NvidiaProvider::smi_only(shim);
        for _ in 0..SMI_EMPTY_FAIL_LIMIT {
            assert!(provider.poll().unwrap().is_empty());
        }
        assert!(matches!(provider.backend, Backend::Unavailable { .. }));
    }

    #[test]
    fn device_by_index_failure_is_fatal_not_empty() {
        let outcome = NvmlPollOutcome::Fatal("NVML device_by_index(0) failed: test".into());
        assert!(matches!(outcome, NvmlPollOutcome::Fatal(_)));
        assert!(!matches!(outcome, NvmlPollOutcome::Empty));
    }

    #[test]
    fn nvml_collects_gpu_temp_on_this_machine_or_skips() {
        let Some(nvml) = try_init_nvml() else {
            return;
        };
        match collect_nvml_readings(&nvml) {
            NvmlPollOutcome::Ok(r) => {
                assert!(r.iter().any(|x| x.key == "gpu_temp"), "{r:?}");
            }
            NvmlPollOutcome::Empty => {}
            NvmlPollOutcome::Fatal(e) => panic!("unexpected fatal: {e}"),
        }
    }
}
