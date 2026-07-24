//! Synthetic sensor data for previews, hardware-free rendering, and benches.
//!
//! Hidden from public docs — this is a shared fixture for `examples/` and
//! `benches/`, not a supported library API.

use crate::render::SensorData;
use crate::sensor::history::SensorHistory;
use std::collections::HashMap;

/// Mock sensor data simulating a gaming session under load.
pub fn mock_sensors() -> SensorData {
    HashMap::from([
        ("cpu_temp".into(), "67".into()),
        ("cpu_util".into(), "42".into()),
        ("gpu_temp".into(), "71".into()),
        ("gpu_util".into(), "87".into()),
        ("gpu_power".into(), "285".into()),
        ("ram_used".into(), "24.2".into()),
        ("ram_total".into(), "60.4".into()),
        ("vram_used".into(), "9.8".into()),
        ("vram_total".into(), "15.9".into()),
        ("fps".into(), "144".into()),
        ("frametime".into(), "6.9".into()),
        ("track_title".into(), "Hard Times".into()),
        ("track_artist".into(), "Paramore".into()),
        ("track_album".into(), "After Laughter".into()),
        ("track_status".into(), "Playing".into()),
        ("track_player".into(), "mock".into()),
        ("track_position".into(), "1:23".into()),
        ("track_duration".into(), "3:12".into()),
        ("track_position_s".into(), "83".into()),
        ("track_duration_s".into(), "192".into()),
        ("track_progress".into(), "43".into()),
        ("track_has_art".into(), "0".into()),
    ])
}

/// Mock sensor data with slight variation per iteration, to make history graphs visible.
pub fn mock_sensors_varying(iteration: u64) -> SensorData {
    let mut m = mock_sensors();
    let phase = (iteration as f64 * 0.3).sin();
    let cpu_util: f64 = 42.0 + phase * 15.0;
    let cpu_temp: f64 = 67.0 + phase * 5.0;
    m.insert("cpu_util".into(), format!("{:.1}", cpu_util));
    m.insert("cpu_temp".into(), format!("{:.0}", cpu_temp));
    m
}

/// Generate synthetic history data for preview (60 points, sinusoidal wave around a base value).
/// Uses a deterministic pattern so previews/benches are reproducible.
pub fn fill_synthetic_history(
    history: &mut SensorHistory,
    metrics: &[String],
    sensor_data: &SensorData,
) {
    let sample_count = 60usize;
    for metric in metrics {
        // Use current sensor value as base if available, otherwise pick a reasonable default
        let base: f64 = sensor_data
            .get(metric)
            .and_then(|v| v.parse().ok())
            .unwrap_or(50.0);

        for i in 0..sample_count {
            // Sinusoidal variation ±20% of base
            let phase = (i as f64 / sample_count as f64) * std::f64::consts::TAU;
            let amplitude = base * 0.2;
            let value = (base + amplitude * phase.sin()).max(0.0);

            let mut data = HashMap::new();
            data.insert(metric.clone(), format!("{:.1}", value));
            history.record(&data);
        }
    }
}
