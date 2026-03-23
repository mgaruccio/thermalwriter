use std::collections::HashMap;
use std::time::Duration;
use thermalwriter::sensor::history::SensorHistory;

#[test]
fn history_records_and_queries_numeric_values() {
    let mut history = SensorHistory::new();
    history.configure_metric("cpu_temp", Duration::from_secs(60));

    let mut data = HashMap::new();
    data.insert("cpu_temp".to_string(), "65".to_string());
    history.record(&data);

    data.insert("cpu_temp".to_string(), "67".to_string());
    history.record(&data);

    let values = history.query("cpu_temp", 10);
    assert_eq!(values.len(), 2);
    assert!((values[0] - 65.0).abs() < 0.01);
    assert!((values[1] - 67.0).abs() < 0.01);
}

#[test]
fn history_skips_non_numeric_values() {
    let mut history = SensorHistory::new();
    history.configure_metric("cpu_temp", Duration::from_secs(60));

    let mut data = HashMap::new();
    data.insert("cpu_temp".to_string(), "--".to_string());
    history.record(&data);

    let values = history.query("cpu_temp", 10);
    assert!(values.is_empty());
}

#[test]
fn history_ignores_unconfigured_metrics() {
    let mut history = SensorHistory::new();
    // Don't configure any metrics

    let mut data = HashMap::new();
    data.insert("cpu_temp".to_string(), "65".to_string());
    history.record(&data);

    let values = history.query("cpu_temp", 10);
    assert!(values.is_empty());
}

#[test]
fn history_query_downsamples_when_buffer_exceeds_count() {
    let mut history = SensorHistory::new();
    history.configure_metric("val", Duration::from_secs(3600));

    let mut data = HashMap::new();
    for i in 0..100 {
        data.insert("val".to_string(), i.to_string());
        history.record(&data);
    }

    let values = history.query("val", 10);
    assert_eq!(values.len(), 10);
    // First value should be near 0, last near 99
    assert!(values[0] < 10.0);
    assert!(values[9] > 89.0);
}

#[test]
fn history_prunes_old_entries() {
    let mut history = SensorHistory::new();
    // Very short duration for testing
    history.configure_metric("val", Duration::from_millis(50));

    let mut data = HashMap::new();
    data.insert("val".to_string(), "1".to_string());
    history.record(&data);

    std::thread::sleep(std::time::Duration::from_millis(100));

    data.insert("val".to_string(), "2".to_string());
    history.record(&data);

    let values = history.query("val", 100);
    // Old entry should be pruned, only "2" remains
    assert_eq!(values.len(), 1);
    assert!((values[0] - 2.0).abs() < 0.01);
}

#[test]
fn history_inject_into_context_adds_arrays() {
    let mut history = SensorHistory::new();
    history.configure_metric("cpu_temp", Duration::from_secs(60));

    let mut data = HashMap::new();
    data.insert("cpu_temp".to_string(), "65".to_string());
    history.record(&data);

    let mut context = tera::Context::new();
    history.inject_into_context(&mut context, 10);

    // Context should now contain cpu_temp_history
    let json = context.into_json();
    let arr = json.get("cpu_temp_history").unwrap().as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert!((arr[0].as_f64().unwrap() - 65.0).abs() < 0.01);
}
