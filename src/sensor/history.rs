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
        self.configs.insert(key.to_string(), MetricConfig { max_duration });
        self.buffers.entry(key.to_string()).or_insert_with(VecDeque::new);
    }

    /// Record current sensor readings. Only configured metrics are stored.
    /// Non-numeric values are silently skipped.
    pub fn record(&mut self, data: &HashMap<String, String>) {
        let now = Instant::now();
        for (key, config) in &self.configs {
            if let Some(val_str) = data.get(key) {
                if let Ok(val) = val_str.parse::<f64>() {
                    let buf = self.buffers.entry(key.clone()).or_insert_with(VecDeque::new);
                    buf.push_back(Sample { time: now, value: val });
                    // Prune old entries
                    let cutoff = now - config.max_duration;
                    while buf.front().is_some_and(|s| s.time < cutoff) {
                        buf.pop_front();
                    }
                }
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
