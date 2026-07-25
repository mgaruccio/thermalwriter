// SysinfoProvider: reads RAM, CPU, and network metrics via the sysinfo crate.

use std::collections::HashSet;
use std::time::Instant;

use anyhow::Result;
use sysinfo::{Networks, System};

use super::{SensorDescriptor, SensorProvider, SensorReading};

const BYTES_PER_GIB: f64 = 1_073_741_824.0;

pub struct SysinfoProvider {
    sys: System,
    networks: Networks,
    last_net_rx: Option<u64>,
    last_net_tx: Option<u64>,
    last_poll: Option<Instant>,
    last_net_poll: Option<Instant>,
    needed_keys: Option<HashSet<String>>,
}

impl Default for SysinfoProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SysinfoProvider {
    pub fn new() -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();
        let networks = Networks::new_with_refreshed_list();
        Self {
            sys,
            networks,
            last_net_rx: None,
            last_net_tx: None,
            last_poll: None,
            last_net_poll: None,
            needed_keys: None,
        }
    }
}

impl SensorProvider for SysinfoProvider {
    fn name(&self) -> &str {
        "sysinfo"
    }

    /// Check if any needed key matches our static keys or the per-core pattern
    /// `cpu_c{N}_util` / `cpu_c{N}_freq`.
    fn wants_any(&self, needed: &HashSet<String>) -> bool {
        const STATIC_KEYS: &[&str] = &["ram_used", "ram_total", "cpu_util", "net_rx", "net_tx"];
        STATIC_KEYS.iter().any(|k| needed.contains(*k))
            || needed
                .iter()
                .any(|k| k.starts_with("cpu_c") && (k.ends_with("_util") || k.ends_with("_freq")))
    }
    fn set_needed_keys(&mut self, keys: Option<&HashSet<String>>) {
        let wants_net = |k: Option<&HashSet<String>>| {
            k.is_none() || k.is_some_and(|s| s.contains("net_rx") || s.contains("net_tx"))
        };
        let was_net_needed = wants_net(self.needed_keys.as_ref());
        let now_net_needed = wants_net(keys);
        // Reset network counters on false→true transition so the first throughput
        // sample after a pruning gap is averaged over the active poll interval,
        // not the entire gap (and a counter reset during the gap is hidden by
        // saturating_sub).
        if !was_net_needed && now_net_needed {
            self.last_net_rx = None;
            self.last_net_tx = None;
            self.last_net_poll = None;
        }
        self.needed_keys = keys.cloned();
    }

    fn poll(&mut self) -> Result<Vec<SensorReading>> {
        let needed = self.needed_keys.as_ref();
        let wants =
            |key: &str| needed.is_none() || needed.map(|s| s.contains(key)).unwrap_or(false);

        let now = Instant::now();
        let mut readings = Vec::new();

        // RAM — only refresh if needed.
        if wants("ram_used") || wants("ram_total") {
            self.sys.refresh_memory();
        }

        // CPU — only refresh what's needed. sysinfo separates usage and
        // frequency refreshes (refresh_cpu_usage does NOT update frequency).
        let usage_needed = wants("cpu_util")
            || (needed.is_some() && needed.unwrap().iter().any(|k| k.ends_with("_util")));
        let frequency_needed =
            needed.is_none() || needed.unwrap().iter().any(|k| k.ends_with("_freq"));
        if usage_needed {
            self.sys.refresh_cpu_usage();
        }
        if frequency_needed {
            self.sys.refresh_cpu_frequency();
        }

        // Network — only refresh if net_rx/net_tx needed.
        if wants("net_rx") || wants("net_tx") {
            self.networks.refresh(true);
        }

        // RAM used (bytes → GiB, 1 decimal)
        if wants("ram_used") {
            let used = self.sys.used_memory() as f64;
            readings.push(SensorReading {
                key: "ram_used".to_string(),
                value: format!("{:.1}", used / BYTES_PER_GIB),
                unit: "GB".to_string(),
            });
        }

        // RAM total (bytes → GiB, 1 decimal)
        if wants("ram_total") {
            let total = self.sys.total_memory() as f64;
            readings.push(SensorReading {
                key: "ram_total".to_string(),
                value: format!("{:.1}", total / BYTES_PER_GIB),
                unit: "GB".to_string(),
            });
        }

        // CPU utilization — average across all cores
        if usage_needed || frequency_needed {
            let cpus = self.sys.cpus();
            if !cpus.is_empty() {
                let avg =
                    cpus.iter().map(|c| c.cpu_usage() as f64).sum::<f64>() / cpus.len() as f64;
                if wants("cpu_util") {
                    readings.push(SensorReading {
                        key: "cpu_util".to_string(),
                        value: format!("{:.1}", avg),
                        unit: "%".to_string(),
                    });
                }

                // Per-core CPU utilization and frequency
                for (i, cpu) in cpus.iter().enumerate() {
                    let key_util = format!("cpu_c{}_util", i);
                    let key_freq = format!("cpu_c{}_freq", i);
                    if wants(&key_util) {
                        readings.push(SensorReading {
                            key: key_util,
                            value: format!("{:.1}", cpu.cpu_usage() as f64),
                            unit: "%".to_string(),
                        });
                    }
                    if wants(&key_freq) {
                        readings.push(SensorReading {
                            key: key_freq,
                            value: format!("{}", cpu.frequency()),
                            unit: "MHz".to_string(),
                        });
                    }
                }
            }
        }

        // Network throughput — delta bytes/sec across non-loopback interfaces
        if wants("net_rx") || wants("net_tx") {
            let mut total_rx: u64 = 0;
            let mut total_tx: u64 = 0;
            for (name, data) in self.networks.iter() {
                if name == "lo" {
                    continue;
                }
                total_rx = total_rx.saturating_add(data.total_received());
                total_tx = total_tx.saturating_add(data.total_transmitted());
            }
            if let (Some(prev_rx), Some(prev_tx), Some(prev_time)) =
                (self.last_net_rx, self.last_net_tx, self.last_net_poll)
            {
                let dt_secs = now.duration_since(prev_time).as_secs_f64();
                if dt_secs > 0.01 {
                    let delta_rx = total_rx.saturating_sub(prev_rx);
                    let delta_tx = total_tx.saturating_sub(prev_tx);
                    let rx_bps = (delta_rx as f64 / dt_secs).round() as u64;
                    let tx_bps = (delta_tx as f64 / dt_secs).round() as u64;

                    if wants("net_rx") {
                        readings.push(SensorReading {
                            key: "net_rx".to_string(),
                            value: rx_bps.to_string(),
                            unit: "B/s".to_string(),
                        });
                    }
                    if wants("net_tx") {
                        readings.push(SensorReading {
                            key: "net_tx".to_string(),
                            value: tx_bps.to_string(),
                            unit: "B/s".to_string(),
                        });
                    }
                }
            }

            self.last_net_rx = Some(total_rx);
            self.last_net_tx = Some(total_tx);
            self.last_net_poll = Some(now);
        }

        self.last_poll = Some(now);

        Ok(readings)
    }

    fn available_sensors(&self) -> Vec<SensorDescriptor> {
        let mut sensors = vec![
            SensorDescriptor {
                key: "ram_used".to_string(),
                name: "RAM Used".to_string(),
                unit: "GB".to_string(),
                cost_us: 0,
            },
            SensorDescriptor {
                key: "ram_total".to_string(),
                name: "RAM Total".to_string(),
                unit: "GB".to_string(),
                cost_us: 0,
            },
            SensorDescriptor {
                key: "cpu_util".to_string(),
                name: "CPU Utilization".to_string(),
                unit: "%".to_string(),
                cost_us: 0,
            },
            SensorDescriptor {
                key: "net_rx".to_string(),
                name: "Network RX".to_string(),
                unit: "B/s".to_string(),
                cost_us: 0,
            },
            SensorDescriptor {
                key: "net_tx".to_string(),
                name: "Network TX".to_string(),
                unit: "B/s".to_string(),
                cost_us: 0,
            },
        ];

        let cpus = self.sys.cpus();
        for i in 0..cpus.len() {
            sensors.push(SensorDescriptor {
                key: format!("cpu_c{}_util", i),
                name: format!("CPU Core {} Utilization", i),
                unit: "%".to_string(),
                cost_us: 0,
            });
            sensors.push(SensorDescriptor {
                key: format!("cpu_c{}_freq", i),
                name: format!("CPU Core {} Frequency", i),
                unit: "MHz".to_string(),
                cost_us: 0,
            });
        }

        sensors
    }
    fn declared_keys(&self) -> Vec<&str> {
        vec!["ram_used", "ram_total", "cpu_util", "net_rx", "net_tx"]
    }
}
