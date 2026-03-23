use thermalrighter::sensor::hwmon::HwmonProvider;
use thermalrighter::sensor::amdgpu::AmdGpuProvider;
use thermalrighter::sensor::sysinfo_provider::SysinfoProvider;
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

// ─── AmdGpuProvider tests ────────────────────────────────────────────────────

/// Build a fake DRM sysfs tree for testing AmdGpuProvider.
/// Returns: (TempDir, card_device_path)
fn build_fake_drm_tree(tmp: &TempDir) -> std::path::PathBuf {
    let card_dir = tmp.path().join("card0").join("device");
    fs::create_dir_all(&card_dir).unwrap();

    // GPU utilization
    fs::write(card_dir.join("gpu_busy_percent"), "42\n").unwrap();

    // VRAM: 4 GiB used, 8 GiB total
    fs::write(card_dir.join("mem_info_vram_used"), "4294967296\n").unwrap();
    fs::write(card_dir.join("mem_info_vram_total"), "8589934592\n").unwrap();

    // hwmon subdir for power and temperature
    let hwmon_dir = card_dir.join("hwmon").join("hwmon0");
    fs::create_dir_all(&hwmon_dir).unwrap();
    fs::write(hwmon_dir.join("power1_average"), "120000000\n").unwrap(); // 120 W in microwatts
    fs::write(hwmon_dir.join("temp1_input"), "65000\n").unwrap(); // 65°C in millidegrees

    tmp.path().to_path_buf()
}

#[test]
fn amdgpu_reads_gpu_utilization() {
    let tmp = TempDir::new().unwrap();
    build_fake_drm_tree(&tmp);

    let mut provider = AmdGpuProvider::with_base_path(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    let util = readings.iter().find(|r| r.key == "gpu_util").unwrap();
    assert_eq!(util.value, "42");
}

#[test]
fn amdgpu_converts_vram_bytes_to_gb() {
    let tmp = TempDir::new().unwrap();
    build_fake_drm_tree(&tmp);

    let mut provider = AmdGpuProvider::with_base_path(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    let used = readings.iter().find(|r| r.key == "vram_used").unwrap();
    assert_eq!(used.value, "4.0");

    let total = readings.iter().find(|r| r.key == "vram_total").unwrap();
    assert_eq!(total.value, "8.0");
}

#[test]
fn amdgpu_converts_microwatts_to_watts() {
    let tmp = TempDir::new().unwrap();
    build_fake_drm_tree(&tmp);

    let mut provider = AmdGpuProvider::with_base_path(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    let power = readings.iter().find(|r| r.key == "gpu_power").unwrap();
    assert_eq!(power.value, "120");
}

#[test]
fn amdgpu_converts_millidegrees_to_degrees() {
    let tmp = TempDir::new().unwrap();
    build_fake_drm_tree(&tmp);

    let mut provider = AmdGpuProvider::with_base_path(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    let temp = readings.iter().find(|r| r.key == "gpu_temp").unwrap();
    assert_eq!(temp.value, "65");
}

#[test]
fn amdgpu_missing_sysfs_returns_empty_not_error() {
    let mut provider = AmdGpuProvider::with_base_path("/nonexistent/drm/path".into());
    let result = provider.poll().unwrap();
    assert!(result.is_empty());
}

#[test]
fn amdgpu_partial_sysfs_no_panic() {
    // Missing hwmon subdir — should still return partial readings
    let tmp = TempDir::new().unwrap();
    let card_dir = tmp.path().join("card0").join("device");
    fs::create_dir_all(&card_dir).unwrap();
    fs::write(card_dir.join("gpu_busy_percent"), "55\n").unwrap();
    // No hwmon, no VRAM files

    let mut provider = AmdGpuProvider::with_base_path(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    // gpu_util should be present, no panic on missing files
    let util = readings.iter().find(|r| r.key == "gpu_util").unwrap();
    assert_eq!(util.value, "55");
}

// ─── SysinfoProvider tests ───────────────────────────────────────────────────

#[test]
fn sysinfo_returns_ram_readings() {
    let mut provider = SysinfoProvider::new();
    let readings = provider.poll().unwrap();

    let ram_used = readings.iter().find(|r| r.key == "ram_used").unwrap();
    let ram_total = readings.iter().find(|r| r.key == "ram_total").unwrap();

    // Values should be non-zero on any real machine
    let used: f64 = ram_used.value.parse().unwrap();
    let total: f64 = ram_total.value.parse().unwrap();
    assert!(used > 0.0);
    assert!(total > 0.0);
    assert!(used <= total);
    assert_eq!(ram_used.unit, "GB");
    assert_eq!(ram_total.unit, "GB");
}

#[test]
fn sysinfo_returns_cpu_util() {
    let mut provider = SysinfoProvider::new();
    let readings = provider.poll().unwrap();

    let cpu = readings.iter().find(|r| r.key == "cpu_util").unwrap();
    let util: f64 = cpu.value.parse().unwrap();
    // CPU util should be 0-100
    assert!((0.0..=100.0).contains(&util));
    assert_eq!(cpu.unit, "%");
}

#[test]
fn sysinfo_ram_format_one_decimal() {
    let mut provider = SysinfoProvider::new();
    let readings = provider.poll().unwrap();

    let ram_used = readings.iter().find(|r| r.key == "ram_used").unwrap();
    // Should have exactly 1 decimal place e.g. "7.8"
    let parts: Vec<&str> = ram_used.value.split('.').collect();
    assert_eq!(parts.len(), 2, "Expected 1 decimal place in '{}'", ram_used.value);
    assert_eq!(parts[1].len(), 1, "Expected exactly 1 decimal digit in '{}'", ram_used.value);
}
