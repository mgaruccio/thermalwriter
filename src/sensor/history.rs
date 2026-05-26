use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Per-metric configuration for history retention.
struct MetricConfig {
    max_duration: Duration,
}

/// Timestamped sensor reading.
struct Sample {
    time: Instant,
    value: f64,
}

/// Ring buffer of sensor readings, keyed by metric name.
/// Records numeric values from SensorHub polls and serves
/// downsampled history arrays for Tera template injection.
pub struct SensorHistory {
    buffers: HashMap<String, VecDeque<Sample>>,
    configs: HashMap<String, MetricConfig>,
}

impl Default for SensorHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl SensorHistory {
    pub fn new() -> Self {
        Self {
            buffers: HashMap::new(),
            configs: HashMap::new(),
        }
    }

    /// Configure a metric for history retention.
    /// Must be called before `record()` will store values for this metric.
    pub fn configure_metric(&mut self, key: &str, max_duration: Duration) {
        self.configs
            .insert(key.to_string(), MetricConfig { max_duration });
        self.buffers.entry(key.to_string()).or_default();
    }

    /// Record current sensor readings. Only configured metrics are stored.
    /// Non-numeric values are silently skipped.
    /// Pruning runs unconditionally for every configured metric — including when the
    /// key is absent (sensor dropout) or non-numeric (e.g. nvidia-smi "N/A") — so
    /// stale samples don't accumulate indefinitely as ghost values on history graphs.
    pub fn record(&mut self, data: &HashMap<String, String>) {
        let now = Instant::now();
        // Collect cutoffs first to avoid a simultaneous borrow of configs + buffers.
        let cutoffs: Vec<(String, Instant)> = self
            .configs
            .iter()
            .map(|(k, c)| (k.clone(), now - c.max_duration))
            .collect();
        for (key, cutoff) in cutoffs {
            let buf = self.buffers.entry(key.clone()).or_default();
            if let Some(val_str) = data.get(&key)
                && let Ok(val) = val_str.parse::<f64>()
            {
                buf.push_back(Sample {
                    time: now,
                    value: val,
                });
            }
            // Prune unconditionally — covers dropout and non-numeric values.
            while buf.front().is_some_and(|s| s.time < cutoff) {
                buf.pop_front();
            }
        }
    }

    /// Query the most recent `count` samples for a metric.
    /// Returns evenly-spaced values by picking from the buffer.
    /// Returns empty Vec if metric is not configured or has no data.
    pub fn query(&self, key: &str, count: usize) -> Vec<f64> {
        let Some(buf) = self.buffers.get(key) else {
            return Vec::new();
        };
        if buf.is_empty() || count == 0 {
            return Vec::new();
        }
        if buf.len() <= count {
            return buf.iter().map(|s| s.value).collect();
        }
        // Downsample: pick evenly-spaced indices
        let step = buf.len() as f64 / count as f64;
        (0..count)
            .map(|i| {
                let idx = (i as f64 * step).round() as usize;
                buf[idx.min(buf.len() - 1)].value
            })
            .collect()
    }

    /// Returns all configured metric keys.
    pub fn configured_metrics(&self) -> Vec<String> {
        self.configs.keys().cloned().collect()
    }

    /// Inject history arrays into a Tera context.
    /// For each configured metric "foo", adds "foo_history" as a JSON array of floats.
    pub fn inject_into_context(&self, context: &mut tera::Context, sample_count: usize) {
        for key in self.configs.keys() {
            let values = self.query(key, sample_count);
            context.insert(format!("{}_history", key), &values);
        }
    }
}
