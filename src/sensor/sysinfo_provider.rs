// SysinfoProvider: reads RAM, CPU, and network metrics via the sysinfo crate.

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
}

impl SysinfoProvider {
    pub fn new() -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();
        let mut networks = Networks::new_with_refreshed_list();
        networks.refresh(true);
        Self {
            sys,
            networks,
            last_net_rx: None,
            last_net_tx: None,
            last_poll: None,
        }
    }
}

impl SensorProvider for SysinfoProvider {
    fn name(&self) -> &str {
        "sysinfo"
    }

    fn poll(&mut self) -> Result<Vec<SensorReading>> {
        self.sys.refresh_memory();
        self.sys.refresh_cpu_usage();
        self.networks.refresh(true);

        let now = Instant::now();
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

            // Per-core CPU utilization and frequency
            for (i, cpu) in cpus.iter().enumerate() {
                readings.push(SensorReading {
                    key: format!("cpu_c{}_util", i),
                    value: format!("{:.1}", cpu.cpu_usage() as f64),
                    unit: "%".to_string(),
                });
                readings.push(SensorReading {
                    key: format!("cpu_c{}_freq", i),
                    value: format!("{}", cpu.frequency()),
                    unit: "MHz".to_string(),
                });
            }
        }

        // Network throughput — delta bytes/sec across non-loopback interfaces
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
            (self.last_net_rx, self.last_net_tx, self.last_poll)
        {
            let dt_secs = now.duration_since(prev_time).as_secs_f64();
            if dt_secs > 0.01 {
                let delta_rx = total_rx.saturating_sub(prev_rx);
                let delta_tx = total_tx.saturating_sub(prev_tx);
                let rx_bps = (delta_rx as f64 / dt_secs).round() as u64;
                let tx_bps = (delta_tx as f64 / dt_secs).round() as u64;

                readings.push(SensorReading {
                    key: "net_rx".to_string(),
                    value: rx_bps.to_string(),
                    unit: "B/s".to_string(),
                });
                readings.push(SensorReading {
                    key: "net_tx".to_string(),
                    value: tx_bps.to_string(),
                    unit: "B/s".to_string(),
                });
            }
        }

        self.last_net_rx = Some(total_rx);
        self.last_net_tx = Some(total_tx);
        self.last_poll = Some(now);

        Ok(readings)
    }

    fn available_sensors(&self) -> Vec<SensorDescriptor> {
        let mut sensors = vec![
            SensorDescriptor { key: "ram_used".to_string(), name: "RAM Used".to_string(), unit: "GB".to_string() },
            SensorDescriptor { key: "ram_total".to_string(), name: "RAM Total".to_string(), unit: "GB".to_string() },
            SensorDescriptor { key: "cpu_util".to_string(), name: "CPU Utilization".to_string(), unit: "%".to_string() },
            SensorDescriptor { key: "net_rx".to_string(), name: "Network RX".to_string(), unit: "B/s".to_string() },
            SensorDescriptor { key: "net_tx".to_string(), name: "Network TX".to_string(), unit: "B/s".to_string() },
        ];

        let cpus = self.sys.cpus();
        for i in 0..cpus.len() {
            sensors.push(SensorDescriptor {
                key: format!("cpu_c{}_util", i),
                name: format!("CPU Core {} Utilization", i),
                unit: "%".to_string(),
            });
            sensors.push(SensorDescriptor {
                key: format!("cpu_c{}_freq", i),
                name: format!("CPU Core {} Frequency", i),
                unit: "MHz".to_string(),
            });
        }

        sensors
    }
}
