use thermalrighter::sensor::hwmon::HwmonProvider;
use thermalrighter::sensor::SensorProvider;
use std::fs;
use tempfile::TempDir;

#[test]
fn hwmon_reads_temperature_from_sysfs() {
    let tmp = TempDir::new().unwrap();
    let hwmon_dir = tmp.path().join("hwmon0");
    fs::create_dir_all(&hwmon_dir).unwrap();
    fs::write(hwmon_dir.join("name"), "coretemp\n").unwrap();
    fs::write(hwmon_dir.join("temp1_input"), "65000\n").unwrap(); // 65°C in millidegrees
    fs::write(hwmon_dir.join("temp1_label"), "Package id 0\n").unwrap();

    let mut provider = HwmonProvider::with_base_path(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    let cpu_temp = readings.iter().find(|r| r.key.contains("temp")).unwrap();
    assert_eq!(cpu_temp.value, "65");
    assert_eq!(cpu_temp.unit, "°C");
}

#[test]
fn hwmon_reads_fan_speed_from_sysfs() {
    let tmp = TempDir::new().unwrap();
    let hwmon_dir = tmp.path().join("hwmon1");
    fs::create_dir_all(&hwmon_dir).unwrap();
    fs::write(hwmon_dir.join("name"), "nct6798\n").unwrap();
    fs::write(hwmon_dir.join("fan1_input"), "1200\n").unwrap(); // RPM
    fs::write(hwmon_dir.join("fan1_label"), "CPU Fan\n").unwrap();

    let mut provider = HwmonProvider::with_base_path(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    let fan = readings.iter().find(|r| r.key.contains("fan")).unwrap();
    assert_eq!(fan.value, "1200");
    assert_eq!(fan.unit, "RPM");
}

#[test]
fn hwmon_millidegree_integer_division() {
    // Verify 65500 millidegrees → "65" (integer division, truncates not rounds)
    let tmp = TempDir::new().unwrap();
    let hwmon_dir = tmp.path().join("hwmon0");
    fs::create_dir_all(&hwmon_dir).unwrap();
    fs::write(hwmon_dir.join("name"), "k10temp\n").unwrap();
    fs::write(hwmon_dir.join("temp1_input"), "65500\n").unwrap();
    fs::write(hwmon_dir.join("temp1_label"), "Tctl\n").unwrap();

    let mut provider = HwmonProvider::with_base_path(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    let temp = readings.iter().find(|r| r.key.contains("temp")).unwrap();
    assert_eq!(temp.value, "65");
}

#[test]
fn hwmon_missing_base_path_returns_error() {
    let mut provider = HwmonProvider::with_base_path("/nonexistent/path/hwmon".into());
    let result = provider.poll();
    assert!(result.is_err());
}

#[test]
fn hwmon_empty_dir_returns_empty_readings() {
    let tmp = TempDir::new().unwrap();
    // No hwmon subdirs
    let mut provider = HwmonProvider::with_base_path(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();
    assert!(readings.is_empty());
}

#[test]
fn sensory_hub_aggregates_providers() {
    use thermalrighter::sensor::SensorHub;

    let tmp = TempDir::new().unwrap();
    let hwmon_dir = tmp.path().join("hwmon0");
    fs::create_dir_all(&hwmon_dir).unwrap();
    fs::write(hwmon_dir.join("name"), "coretemp\n").unwrap();
    fs::write(hwmon_dir.join("temp1_input"), "72000\n").unwrap();
    fs::write(hwmon_dir.join("temp1_label"), "Core 0\n").unwrap();

    let mut hub = SensorHub::new();
    hub.add_provider(Box::new(HwmonProvider::with_base_path(tmp.path().to_path_buf())));

    let data = hub.poll();
    assert!(!data.is_empty());
    let temp_val = data.values().next().unwrap();
    assert_eq!(temp_val, "72");
}

#[test]
fn sensor_hub_continues_on_provider_failure() {
    use thermalrighter::sensor::{SensorHub, SensorReading, SensorDescriptor};
    use anyhow::anyhow;

    struct FailingProvider;
    impl SensorProvider for FailingProvider {
        fn name(&self) -> &str { "failing" }
        fn poll(&mut self) -> anyhow::Result<Vec<SensorReading>> {
            Err(anyhow!("simulated failure"))
        }
        fn available_sensors(&self) -> Vec<SensorDescriptor> { vec![] }
    }

    let mut hub = SensorHub::new();
    hub.add_provider(Box::new(FailingProvider));

    // Should not panic, returns empty map
    let data = hub.poll();
    assert!(data.is_empty());
}
