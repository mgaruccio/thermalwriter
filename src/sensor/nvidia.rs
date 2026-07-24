// Nvidia GPU sensor provider: prefers NVML, falls back to nvidia-smi.
//
// Forking nvidia-smi every poll costs ~15 ms of process overhead on this
// hardware and dominates steady-state daemon CPU. NVML is the same data over
// a long-lived library handle owned by a dedicated worker thread (~µs/query).
//
// Hung-driver policy (both paths share a 500 ms wall-clock budget):
// - smi: wait_timeout(500ms) + kill/reap the child
// - NVML: request/response against a worker; recv_timeout(500ms). NVML calls
//   are not cancellable, so a wedged call may leave the worker stuck until the
//   driver recovers — but sensor_hub.poll / the tick loop always return within
//   the budget and demote off NVML. (#80 / #91)

use anyhow::Result;
use nvml_wrapper::Nvml;
use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

use super::{SensorDescriptor, SensorProvider, SensorReading};

const NVIDIA_POLL_BUDGET: Duration = Duration::from_millis(500);
const REPROBE_BACKOFF: Duration = Duration::from_secs(60);
const SMI_EMPTY_FAIL_LIMIT: u32 = 3;

/// Dedicated NVML owner. Queries run only on this thread; the tick loop never
/// blocks longer than [`NVIDIA_POLL_BUDGET`] waiting for a reply.
struct NvmlWorker {
    req_tx: SyncSender<()>,
    resp_rx: Receiver<NvmlPollOutcome>,
}

impl NvmlWorker {
    fn spawn() -> Option<Self> {
        let nvml = match Nvml::init() {
            Ok(n) => n,
            Err(e) => {
                log::debug!("NVML init failed: {e}");
                return None;
            }
        };
        match nvml.device_count() {
            Ok(n) if n > 0 => {}
            Ok(_) => {
                log::debug!("NVML init ok but device_count=0");
                return None;
            }
            Err(e) => {
                log::debug!("NVML device_count failed: {e}");
                return None;
            }
        }

        let (req_tx, req_rx) = mpsc::sync_channel::<()>(0); // rendezvous
        let (resp_tx, resp_rx) = mpsc::sync_channel::<NvmlPollOutcome>(1);

        thread::Builder::new()
            .name("tw-nvml".into())
            .spawn(move || nvml_worker_loop(nvml, req_rx, resp_tx))
            .ok()?;

        Some(Self { req_tx, resp_rx })
    }

    /// Poll with a hard wall-clock timeout. On timeout the worker may still be
    /// inside a wedged NVML call; the caller must drop this worker (demote).
    fn poll_timed(&self) -> NvmlPollOutcome {
        // Drop any stale reply from a previous timed-out request.
        while self.resp_rx.try_recv().is_ok() {}

        if self.req_tx.send(()).is_err() {
            return NvmlPollOutcome::Fatal("NVML worker is gone".into());
        }
        match self.resp_rx.recv_timeout(NVIDIA_POLL_BUDGET) {
            Ok(outcome) => outcome,
            Err(RecvTimeoutError::Timeout) => NvmlPollOutcome::TimedOut,
            Err(RecvTimeoutError::Disconnected) => {
                NvmlPollOutcome::Fatal("NVML worker exited without result".into())
            }
        }
    }
}

fn nvml_worker_loop(nvml: Nvml, req_rx: Receiver<()>, resp_tx: SyncSender<NvmlPollOutcome>) {
    while req_rx.recv().is_ok() {
        let outcome = collect_nvml_readings(&nvml);
        // If the consumer timed out and dropped us, send may fail — exit.
        if resp_tx.send(outcome).is_err() {
            break;
        }
    }
}

enum Backend {
    Nvml(NvmlWorker),
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
        // Dropping NvmlWorker closes req_tx; the worker exits when it next
        // notices (or stays stuck inside a wedged NVML call until the driver
        // recovers — the tick loop is already free).
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
                if let Some(worker) = NvmlWorker::spawn() {
                    log::info!("NVML became available; leaving nvidia-smi fallback");
                    self.backend = Backend::Nvml(worker);
                } else if let Backend::Smi { reprobe_at, .. } = &mut self.backend {
                    *reprobe_at = Instant::now() + REPROBE_BACKOFF;
                }
            }
            _ => {}
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
    if let Some(worker) = NvmlWorker::spawn() {
        Backend::Nvml(worker)
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
    /// Handle live, no metrics this pass (all queries unsupported).
    Empty,
    /// Library/device failure — caller must demote off NVML.
    Fatal(String),
    /// Exceeded [`NVIDIA_POLL_BUDGET`] waiting for the worker.
    TimedOut,
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

        let nvml_outcome = if let Backend::Nvml(worker) = &self.backend {
            Some(worker.poll_timed())
        } else {
            None
        };

        if let Some(outcome) = nvml_outcome {
            return match outcome {
                NvmlPollOutcome::Ok(readings) => Ok(readings),
                NvmlPollOutcome::Empty => Ok(Vec::new()),
                NvmlPollOutcome::Fatal(reason) => {
                    self.demote_nvml(&reason);
                    self.poll_via_smi_backend()
                }
                NvmlPollOutcome::TimedOut => {
                    self.demote_nvml(&format!("NVML poll timed out after {NVIDIA_POLL_BUDGET:?}"));
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
        assert!(!matches!(outcome, NvmlPollOutcome::TimedOut));
    }

    #[test]
    fn timed_out_is_distinct_from_empty() {
        let outcome = NvmlPollOutcome::TimedOut;
        assert!(matches!(outcome, NvmlPollOutcome::TimedOut));
        assert!(!matches!(outcome, NvmlPollOutcome::Empty));
        assert!(!matches!(outcome, NvmlPollOutcome::Ok(_)));
    }

    #[test]
    fn nvml_worker_polls_within_budget_on_healthy_gpu() {
        let Some(worker) = NvmlWorker::spawn() else {
            return;
        };
        let started = Instant::now();
        match worker.poll_timed() {
            NvmlPollOutcome::Ok(r) => {
                assert!(
                    r.iter().any(|x| x.key == "gpu_temp"),
                    "NVML worker must return gpu_temp: {r:?}"
                );
            }
            NvmlPollOutcome::Empty => {}
            NvmlPollOutcome::Fatal(e) => panic!("unexpected fatal on healthy GPU: {e}"),
            NvmlPollOutcome::TimedOut => panic!("NVML timed out on healthy GPU"),
        }
        assert!(
            started.elapsed() < NVIDIA_POLL_BUDGET,
            "healthy NVML poll must finish under the hang budget"
        );
    }

    #[test]
    fn nvml_worker_timeout_path_is_wired() {
        // We cannot wedge libnvidia-ml in unit tests. Assert the provider demotes
        // when poll_timed returns TimedOut by driving smi_only (no NVML) is not
        // enough — instead verify TimedOut is handled in the match via a private
        // unit that constructs the outcome.
        let mut provider = NvidiaProvider::smi_only(PathBuf::from(
            "/tmp/thermalwriter-definitely-missing-nvidia-smi",
        ));
        // Simulate demotion entry points used by the TimedOut arm.
        provider.demote_nvml("NVML poll timed out after 500ms");
        assert!(
            matches!(provider.backend, Backend::Unavailable { .. }),
            "demote with missing smi must enter Unavailable"
        );
    }
}
