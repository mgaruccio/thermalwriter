use serial_test::serial;
use std::fs;
use tempfile::TempDir;
use thermalwriter::sensor::SensorProvider;
use thermalwriter::sensor::amdgpu::AmdGpuProvider;
use thermalwriter::sensor::hwmon::HwmonProvider;
use thermalwriter::sensor::mangohud::MangoHudProvider;
use thermalwriter::sensor::nvidia::NvidiaProvider;
use thermalwriter::sensor::rapl::RaplProvider;
use thermalwriter::sensor::sysinfo_provider::SysinfoProvider;

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
fn hwmon_emits_cpu_temp_alias_for_k10temp() {
    let tmp = TempDir::new().unwrap();
    let hwmon_dir = tmp.path().join("hwmon0");
    fs::create_dir_all(&hwmon_dir).unwrap();
    fs::write(hwmon_dir.join("name"), "k10temp\n").unwrap();
    fs::write(hwmon_dir.join("temp1_input"), "72000\n").unwrap();
    fs::write(hwmon_dir.join("temp1_label"), "Tctl\n").unwrap();

    let mut provider = HwmonProvider::with_base_path(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    let alias = readings.iter().find(|r| r.key == "cpu_temp").unwrap();
    assert_eq!(alias.value, "72");
    assert_eq!(alias.unit, "°C");
}

#[test]
fn hwmon_emits_cpu_temp_alias_for_coretemp() {
    let tmp = TempDir::new().unwrap();
    let hwmon_dir = tmp.path().join("hwmon0");
    fs::create_dir_all(&hwmon_dir).unwrap();
    fs::write(hwmon_dir.join("name"), "coretemp\n").unwrap();
    fs::write(hwmon_dir.join("temp1_input"), "58000\n").unwrap();
    fs::write(hwmon_dir.join("temp1_label"), "Package id 0\n").unwrap();

    let mut provider = HwmonProvider::with_base_path(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    let alias = readings.iter().find(|r| r.key == "cpu_temp").unwrap();
    assert_eq!(alias.value, "58");
}

#[test]
fn hwmon_no_cpu_temp_alias_for_non_cpu_chip() {
    let tmp = TempDir::new().unwrap();
    let hwmon_dir = tmp.path().join("hwmon0");
    fs::create_dir_all(&hwmon_dir).unwrap();
    fs::write(hwmon_dir.join("name"), "nct6798\n").unwrap();
    fs::write(hwmon_dir.join("temp1_input"), "35000\n").unwrap();

    let mut provider = HwmonProvider::with_base_path(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    assert!(
        readings.iter().find(|r| r.key == "cpu_temp").is_none(),
        "Non-CPU chip should not emit cpu_temp alias"
    );
}

#[test]
fn hwmon_cpu_temp_alias_only_emitted_once_across_chips() {
    // Two CPU chips in same hwmon dir — cpu_temp should only appear once
    let tmp = TempDir::new().unwrap();
    for (i, chip) in ["k10temp", "coretemp"].iter().enumerate() {
        let hwmon_dir = tmp.path().join(format!("hwmon{}", i));
        fs::create_dir_all(&hwmon_dir).unwrap();
        fs::write(hwmon_dir.join("name"), format!("{}\n", chip)).unwrap();
        fs::write(hwmon_dir.join("temp1_input"), "50000\n").unwrap();
    }

    let mut provider = HwmonProvider::with_base_path(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    let cpu_temp_count = readings.iter().filter(|r| r.key == "cpu_temp").count();
    assert_eq!(
        cpu_temp_count, 1,
        "cpu_temp alias should appear exactly once"
    );
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
fn hwmon_missing_base_path_returns_empty() {
    let mut provider = HwmonProvider::with_base_path("/nonexistent/path/hwmon".into());
    let readings = provider.poll().unwrap();
    assert!(readings.is_empty());
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
    use thermalwriter::sensor::SensorHub;

    let tmp = TempDir::new().unwrap();
    let hwmon_dir = tmp.path().join("hwmon0");
    fs::create_dir_all(&hwmon_dir).unwrap();
    fs::write(hwmon_dir.join("name"), "coretemp\n").unwrap();
    fs::write(hwmon_dir.join("temp1_input"), "72000\n").unwrap();
    fs::write(hwmon_dir.join("temp1_label"), "Core 0\n").unwrap();

    let mut hub = SensorHub::new();
    hub.add_provider(Box::new(HwmonProvider::with_base_path(
        tmp.path().to_path_buf(),
    )));

    let data = hub.poll();
    assert!(!data.is_empty());
    let temp_val = data.values().next().unwrap();
    assert_eq!(temp_val, "72");
}

#[test]
fn sensor_hub_continues_on_provider_failure() {
    use anyhow::anyhow;
    use thermalwriter::sensor::{SensorDescriptor, SensorHub, SensorReading};

    struct FailingProvider;
    impl SensorProvider for FailingProvider {
        fn name(&self) -> &str {
            "failing"
        }
        fn poll(&mut self) -> anyhow::Result<Vec<SensorReading>> {
            Err(anyhow!("simulated failure"))
        }
        fn available_sensors(&self) -> Vec<SensorDescriptor> {
            vec![]
        }
    }

    let mut hub = SensorHub::new();
    hub.add_provider(Box::new(FailingProvider));

    // Should not panic, returns empty map
    let data = hub.poll();
    assert!(data.is_empty());
}

#[test]
fn sensor_hub_earlier_provider_wins_on_key_collision() {
    use thermalwriter::sensor::{SensorDescriptor, SensorHub, SensorProvider, SensorReading};

    struct StaticProvider {
        name: &'static str,
        readings: Vec<SensorReading>,
    }

    impl SensorProvider for StaticProvider {
        fn name(&self) -> &str {
            self.name
        }
        fn poll(&mut self) -> anyhow::Result<Vec<SensorReading>> {
            Ok(self.readings.clone())
        }
        fn available_sensors(&self) -> Vec<SensorDescriptor> {
            self.readings
                .iter()
                .map(|r| SensorDescriptor {
                    key: r.key.clone(),
                    name: r.key.clone(),
                    unit: r.unit.clone(),
                })
                .collect()
        }
    }

    let mut hub = SensorHub::new();
    hub.add_provider(Box::new(StaticProvider {
        name: "first",
        readings: vec![SensorReading {
            key: "cpu_temp".into(),
            value: "from_first".into(),
            unit: "C".into(),
        }],
    }));
    hub.add_provider(Box::new(StaticProvider {
        name: "second",
        readings: vec![
            SensorReading {
                key: "cpu_temp".into(),
                value: "from_second".into(),
                unit: "C".into(),
            },
            SensorReading {
                key: "gpu_temp".into(),
                value: "81".into(),
                unit: "C".into(),
            },
        ],
    }));

    let data = hub.poll();
    assert_eq!(data.get("cpu_temp").map(String::as_str), Some("from_first"));
    assert_eq!(data.get("gpu_temp").map(String::as_str), Some("81"));
    assert_eq!(data.len(), 2);
}

// ─── AmdGpuProvider tests ────────────────────────────────────────────────────

/// Build a fake DRM sysfs tree for testing AmdGpuProvider.
/// Returns: (TempDir, card_device_path)
fn build_fake_drm_tree(tmp: &TempDir) -> std::path::PathBuf {
    let card_dir = tmp.path().join("card0").join("device");
    fs::create_dir_all(&card_dir).unwrap();

    // AMD PCI vendor so the provider accepts this card.
    fs::write(card_dir.join("vendor"), "0x1002\n").unwrap();

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

#[test]
fn amdgpu_skips_non_amd_card_and_reads_later_amd() {
    // Hybrid systems often enumerate Intel first. card0 is Intel (no AMD nodes);
    // card1 is AMD with util 77 — poll must return 77, not empty.
    let tmp = TempDir::new().unwrap();

    let intel = tmp.path().join("card0").join("device");
    fs::create_dir_all(&intel).unwrap();
    fs::write(intel.join("vendor"), "0x8086\n").unwrap();

    let amd = tmp.path().join("card1").join("device");
    fs::create_dir_all(&amd).unwrap();
    fs::write(amd.join("vendor"), "0x1002\n").unwrap();
    fs::write(amd.join("gpu_busy_percent"), "77\n").unwrap();

    let mut provider = AmdGpuProvider::with_base_path(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    let util = readings.iter().find(|r| r.key == "gpu_util").unwrap();
    assert_eq!(util.value, "77");
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
    assert_eq!(
        parts.len(),
        2,
        "Expected 1 decimal place in '{}'",
        ram_used.value
    );
    assert_eq!(
        parts[1].len(),
        1,
        "Expected exactly 1 decimal digit in '{}'",
        ram_used.value
    );
}

// ─── SysinfoProvider per-core + network tests ────────────────────────────────

#[test]
fn sysinfo_returns_per_core_cpu_util() {
    let mut provider = SysinfoProvider::new();
    // Poll twice so sysinfo can compute meaningful cpu_usage
    let _ = provider.poll().unwrap();
    let readings = provider.poll().unwrap();

    // Should have at least cpu_c0_util
    let core0 = readings.iter().find(|r| r.key == "cpu_c0_util").unwrap();
    let util: f64 = core0.value.parse().unwrap();
    assert!(
        (0.0..=100.0).contains(&util),
        "cpu_c0_util should be 0-100, got {}",
        util
    );
    assert_eq!(core0.unit, "%");
}

#[test]
fn sysinfo_returns_per_core_cpu_freq() {
    let mut provider = SysinfoProvider::new();
    let readings = provider.poll().unwrap();

    // Should have at least cpu_c0_freq
    let core0_freq = readings.iter().find(|r| r.key == "cpu_c0_freq").unwrap();
    let freq: f64 = core0_freq.value.parse().unwrap();
    assert!(freq > 0.0, "cpu_c0_freq should be > 0 MHz, got {}", freq);
    assert_eq!(core0_freq.unit, "MHz");
}

#[test]
fn sysinfo_per_core_keys_use_correct_format() {
    let mut provider = SysinfoProvider::new();
    let readings = provider.poll().unwrap();

    // All per-core util keys must match cpu_c{N}_util pattern
    for r in &readings {
        if r.key.starts_with("cpu_c") && r.key.ends_with("_util") {
            let middle = r.key.trim_start_matches("cpu_c").trim_end_matches("_util");
            middle
                .parse::<usize>()
                .unwrap_or_else(|_| panic!("core index should be numeric: {}", r.key));
        }
    }
}

#[test]
fn sysinfo_returns_net_rx_and_tx_after_two_polls() {
    let mut provider = SysinfoProvider::new();
    // First poll sets baseline — no net_rx/net_tx expected
    let first = provider.poll().unwrap();
    // net_rx/net_tx should not appear on first poll (no delta yet)
    // (they may appear on first poll with value 0 — that's also acceptable)

    std::thread::sleep(std::time::Duration::from_millis(50));

    // Second poll should have net_rx and net_tx
    let second = provider.poll().unwrap();
    let net_rx = second.iter().find(|r| r.key == "net_rx");
    let net_tx = second.iter().find(|r| r.key == "net_tx");
    assert!(
        net_rx.is_some(),
        "net_rx should be present after second poll"
    );
    assert!(
        net_tx.is_some(),
        "net_tx should be present after second poll"
    );

    let rx_val: f64 = net_rx.unwrap().value.parse().unwrap();
    let tx_val: f64 = net_tx.unwrap().value.parse().unwrap();
    assert!(rx_val >= 0.0, "net_rx should be >= 0, got {}", rx_val);
    assert!(tx_val >= 0.0, "net_tx should be >= 0, got {}", tx_val);
    assert_eq!(net_rx.unwrap().unit, "B/s");
    assert_eq!(net_tx.unwrap().unit, "B/s");
    drop(first); // suppress unused warning
}

// ─── MangoHudProvider tests ──────────────────────────────────────────────────

fn write_mangohud_csv(dir: &std::path::Path, filename: &str, content: &str) {
    fs::write(dir.join(filename), content).unwrap();
}

#[test]
fn mangohud_reads_fps_and_frametime() {
    let tmp = TempDir::new().unwrap();
    write_mangohud_csv(
        tmp.path(),
        "game.csv",
        "fps,frametime,cpu_load,gpu_load\n120,8.333,45,72\n",
    );

    let mut provider = MangoHudProvider::with_log_dir(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    let fps = readings.iter().find(|r| r.key == "fps").unwrap();
    assert_eq!(fps.value, "120");

    let frametime = readings.iter().find(|r| r.key == "frametime").unwrap();
    assert_eq!(frametime.value, "8.3");
}

#[test]
fn mangohud_reads_cpu_and_gpu_load() {
    let tmp = TempDir::new().unwrap();
    write_mangohud_csv(
        tmp.path(),
        "game.csv",
        "fps,frametime,cpu_load,gpu_load\n60,16.667,30,95\n",
    );

    let mut provider = MangoHudProvider::with_log_dir(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    let cpu = readings.iter().find(|r| r.key == "cpu_load").unwrap();
    assert_eq!(cpu.value, "30");

    let gpu = readings.iter().find(|r| r.key == "gpu_load").unwrap();
    assert_eq!(gpu.value, "95");
}

#[test]
fn mangohud_no_csv_files_returns_empty() {
    let tmp = TempDir::new().unwrap();
    // No files in directory

    let mut provider = MangoHudProvider::with_log_dir(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();
    assert!(readings.is_empty());
}

#[test]
fn mangohud_missing_log_dir_returns_empty() {
    let mut provider = MangoHudProvider::with_log_dir("/nonexistent/mangohud/path".into());
    let readings = provider.poll().unwrap();
    assert!(readings.is_empty());
}

#[test]
fn mangohud_headers_but_no_data_rows_returns_empty() {
    let tmp = TempDir::new().unwrap();
    write_mangohud_csv(tmp.path(), "game.csv", "fps,frametime,cpu_load,gpu_load\n");

    let mut provider = MangoHudProvider::with_log_dir(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();
    assert!(readings.is_empty());
}

#[test]
fn mangohud_fps_rounded_to_integer() {
    let tmp = TempDir::new().unwrap();
    write_mangohud_csv(
        tmp.path(),
        "game.csv",
        "fps,frametime,cpu_load,gpu_load\n119.7,8.351,50,80\n",
    );

    let mut provider = MangoHudProvider::with_log_dir(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    let fps = readings.iter().find(|r| r.key == "fps").unwrap();
    // 119.7 rounds to 120
    assert_eq!(fps.value, "120");
}

#[test]
fn mangohud_reads_most_recent_csv_when_multiple_files() {
    let tmp = TempDir::new().unwrap();

    // Write older file first
    write_mangohud_csv(
        tmp.path(),
        "old_game.csv",
        "fps,frametime,cpu_load,gpu_load\n30,33.3,10,20\n",
    );

    // Small delay to ensure different mtime, then write newer file
    std::thread::sleep(std::time::Duration::from_millis(10));
    write_mangohud_csv(
        tmp.path(),
        "new_game.csv",
        "fps,frametime,cpu_load,gpu_load\n144,6.944,70,90\n",
    );

    let mut provider = MangoHudProvider::with_log_dir(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    // Should use the most recently modified file (new_game.csv)
    let fps = readings.iter().find(|r| r.key == "fps").unwrap();
    assert_eq!(fps.value, "144");
}

// ─── HwmonProvider per-core temp + CCD alias tests ───────────────────────────

#[test]
fn hwmon_emits_per_core_temp_alias_from_core_label() {
    let tmp = TempDir::new().unwrap();
    let hwmon_dir = tmp.path().join("hwmon0");
    fs::create_dir_all(&hwmon_dir).unwrap();
    fs::write(hwmon_dir.join("name"), "coretemp\n").unwrap();
    fs::write(hwmon_dir.join("temp1_input"), "70000\n").unwrap();
    fs::write(hwmon_dir.join("temp1_label"), "Core 0\n").unwrap();
    fs::write(hwmon_dir.join("temp2_input"), "72000\n").unwrap();
    fs::write(hwmon_dir.join("temp2_label"), "Core 1\n").unwrap();

    let mut provider = HwmonProvider::with_base_path(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    let c0 = readings.iter().find(|r| r.key == "cpu_c0_temp").unwrap();
    assert_eq!(c0.value, "70");
    assert_eq!(c0.unit, "°C");

    let c1 = readings.iter().find(|r| r.key == "cpu_c1_temp").unwrap();
    assert_eq!(c1.value, "72");
}

#[test]
fn hwmon_emits_ccd_temp_alias_from_tccd_label() {
    let tmp = TempDir::new().unwrap();
    let hwmon_dir = tmp.path().join("hwmon0");
    fs::create_dir_all(&hwmon_dir).unwrap();
    fs::write(hwmon_dir.join("name"), "k10temp\n").unwrap();
    fs::write(hwmon_dir.join("temp1_input"), "60000\n").unwrap();
    fs::write(hwmon_dir.join("temp1_label"), "Tctl\n").unwrap();
    fs::write(hwmon_dir.join("temp3_input"), "62000\n").unwrap();
    fs::write(hwmon_dir.join("temp3_label"), "Tccd1\n").unwrap();
    fs::write(hwmon_dir.join("temp4_input"), "65000\n").unwrap();
    fs::write(hwmon_dir.join("temp4_label"), "Tccd2\n").unwrap();

    let mut provider = HwmonProvider::with_base_path(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    // Tccd1 → cpu_ccd0_temp (0-indexed)
    let ccd0 = readings.iter().find(|r| r.key == "cpu_ccd0_temp").unwrap();
    assert_eq!(ccd0.value, "62");
    assert_eq!(ccd0.unit, "°C");

    // Tccd2 → cpu_ccd1_temp (0-indexed)
    let ccd1 = readings.iter().find(|r| r.key == "cpu_ccd1_temp").unwrap();
    assert_eq!(ccd1.value, "65");
}

#[test]
fn hwmon_no_per_core_or_ccd_alias_for_non_cpu_chip() {
    let tmp = TempDir::new().unwrap();
    let hwmon_dir = tmp.path().join("hwmon0");
    fs::create_dir_all(&hwmon_dir).unwrap();
    fs::write(hwmon_dir.join("name"), "nct6798\n").unwrap();
    fs::write(hwmon_dir.join("temp1_input"), "35000\n").unwrap();
    fs::write(hwmon_dir.join("temp1_label"), "Core 0\n").unwrap();
    fs::write(hwmon_dir.join("temp2_input"), "40000\n").unwrap();
    fs::write(hwmon_dir.join("temp2_label"), "Tccd1\n").unwrap();

    let mut provider = HwmonProvider::with_base_path(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    assert!(
        readings
            .iter()
            .all(|r| !r.key.starts_with("cpu_c") || r.key == "cpu_temp"),
        "Non-CPU chip should not emit per-core or CCD aliases: {:?}",
        readings.iter().map(|r| &r.key).collect::<Vec<_>>()
    );
}

// ─── RaplProvider rollover tests ─────────────────────────────────────────────

#[test]
fn rapl_rollover_with_unreadable_max_does_not_explode() {
    // Synthesize a RAPL provider whose base_path points to a tempdir where
    // energy_uj rolls over but max_energy_range_uj is missing. Assert the
    // computed wattage is either absent (no reading) or within sane bounds
    // (< 10kW), NOT ~1.8e13 watts (which is what u64::MAX / 1e6 / dt gives).
    let tmp = tempfile::tempdir().unwrap();
    let rapl_dir = tmp.path().join("intel-rapl:0");
    fs::create_dir_all(&rapl_dir).unwrap();

    let energy_path = rapl_dir.join("energy_uj");
    // Tick 1: large prev value near counter end
    fs::write(&energy_path, "1000000000000").unwrap();

    let mut provider = RaplProvider::with_base_path(tmp.path().to_path_buf());
    let _ = provider.poll().unwrap(); // primes prev_energy

    // Tick 2: smaller value (rollover) — max_energy_range_uj does NOT exist
    fs::write(&energy_path, "100").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(60));

    let readings = provider.poll().unwrap();
    if let Some(power) = readings.iter().find(|r| r.key == "cpu_power") {
        let watts: f64 = power
            .value
            .parse()
            .expect("cpu_power value must be a number");
        assert!(
            watts.is_finite() && (0.0..10_000.0).contains(&watts),
            "rollover with missing max produced insane wattage: {} W (expected absent or < 10kW)",
            watts
        );
    }
    // Absent reading is also acceptable — both outcomes are sane.
}

// ─── nvidia-smi N/A parser tests ─────────────────────────────────────────────

#[test]
fn nvidia_parser_skips_na_fields_without_emitting_nan() {
    // Simulate nvidia-smi output where power.draw is "N/A" (driver hung or Optimus).
    // The parser must NOT emit a gpu_power reading with value "NaN", "0", or garbage.
    // Valid fields (gpu_temp=65) must still be emitted.
    let line = "65, 30, N/A, 4096, 16384";
    let readings = thermalwriter::sensor::nvidia::parse_csv_line(line);

    let power = readings.iter().find(|r| r.key == "gpu_power");
    assert!(
        power.is_none(),
        "N/A power.draw must not produce a gpu_power reading; got {:?}",
        power
    );

    let temp = readings.iter().find(|r| r.key == "gpu_temp");
    assert_eq!(
        temp.map(|r| r.value.as_str()),
        Some("65"),
        "valid gpu_temp field must still be emitted when power is N/A"
    );

    let util = readings.iter().find(|r| r.key == "gpu_util");
    assert_eq!(
        util.map(|r| r.value.as_str()),
        Some("30"),
        "valid gpu_util field must still be emitted when power is N/A"
    );
}

#[test]
fn nvidia_parser_emits_all_fields_when_all_valid() {
    let line = "72, 85, 180.7, 8192, 16384";
    let readings = thermalwriter::sensor::nvidia::parse_csv_line(line);

    let temp = readings
        .iter()
        .find(|r| r.key == "gpu_temp")
        .expect("gpu_temp missing");
    assert_eq!(temp.value, "72");

    let util = readings
        .iter()
        .find(|r| r.key == "gpu_util")
        .expect("gpu_util missing");
    assert_eq!(util.value, "85");

    let power = readings
        .iter()
        .find(|r| r.key == "gpu_power")
        .expect("gpu_power missing");
    assert_eq!(power.value, "181"); // 180.7 rounds to 181 via format!("{:.0}")

    let vram_used = readings
        .iter()
        .find(|r| r.key == "vram_used")
        .expect("vram_used missing");
    assert_eq!(vram_used.value, "8.0"); // 8192 MiB / 1024 = 8.0 GB

    let vram_total = readings
        .iter()
        .find(|r| r.key == "vram_total")
        .expect("vram_total missing");
    assert_eq!(vram_total.value, "16.0");
}

// ─── MangoHud partial-line scan tests ────────────────────────────────────────

#[test]
fn mangohud_partial_leading_line_is_dropped() {
    // Simulate the case where seek lands in the middle of a line.
    // The 4KB tail will start with a partial line fragment, then a newline, then
    // a complete line. The parser must discard the partial fragment and use only
    // the complete line that follows the first newline.
    //
    // We construct a CSV large enough that the seek lands mid-line in a data row,
    // so the tail_bytes start with a partial fragment like "0,72\n".
    // The correct last line "144,6.5,60,80" must be returned; the partial fragment
    // must not be parsed as a data row.
    let tmp = tempfile::tempdir().unwrap();

    // Header + enough rows to push the last row's start near but before 4KB boundary,
    // ensuring the tail seek lands mid-row on our controlled content.
    // Each data row is ~40 bytes. 4096 / 40 ≈ 102 rows to cross the 4KB boundary.
    let mut content = String::from("fps,frametime,cpu_load,gpu_load\n");
    for i in 0..120 {
        // Rows vary slightly to ensure we get a clean cut
        content.push_str(&format!("{},16.6,50,70\n", 60 + (i % 5)));
    }
    // Final authoritative row with distinct values
    content.push_str("144,6.5,60,80\n");

    write_mangohud_csv(tmp.path(), "game.csv", &content);

    let mut provider = MangoHudProvider::with_log_dir(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    // The last complete row must be parsed — fps=144
    let fps = readings
        .iter()
        .find(|r| r.key == "fps")
        .expect("fps must be present");
    assert_eq!(
        fps.value, "144",
        "must read the last complete row, not a partial fragment"
    );

    // cpu_load=60 from the last row (not 50 from the earlier rows)
    let cpu = readings
        .iter()
        .find(|r| r.key == "cpu_load")
        .expect("cpu_load must be present");
    assert_eq!(
        cpu.value, "60",
        "cpu_load must come from the last complete row"
    );
}

#[test]
fn mangohud_partial_trailing_row_without_newline_is_dropped() {
    // Simulate MangoHud mid-write: the file ends with a partial row that has no
    // trailing '\n' yet. The tail read picks this up as the "last line" via
    // lines().rev() — but it's garbage (incomplete field count or wrong values).
    // The parser must discard it and return the last COMPLETE row ("144,...").
    //
    // This is the production failure mode: MangoHud writes continuously and a
    // read landing mid-write sees a truncated last row without a terminating newline.
    let tmp = tempfile::tempdir().unwrap();

    // Complete rows with trailing newline, then a partial row WITHOUT trailing newline.
    // The partial fragment "999,1.0,99,99" (no \n) simulates a mid-flight write.
    let content = "fps,frametime,cpu_load,gpu_load\n\
                   144,6.5,60,80\n\
                   999,1.0,99,99"; // no trailing newline — partial write in progress

    write_mangohud_csv(tmp.path(), "game.csv", content);

    let mut provider = MangoHudProvider::with_log_dir(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    // The partial trailing row (999,...) must be dropped.
    // The last complete row (144,...) must be returned.
    let fps = readings
        .iter()
        .find(|r| r.key == "fps")
        .expect("fps must be present");
    assert_eq!(
        fps.value, "144",
        "partial trailing row without newline must be discarded; got fps={}",
        fps.value
    );

    let cpu = readings
        .iter()
        .find(|r| r.key == "cpu_load")
        .expect("cpu_load must be present");
    assert_eq!(
        cpu.value, "60",
        "cpu_load must come from last complete row, not partial fragment"
    );
}

// ─── NvidiaProvider timeout tests ────────────────────────────────────────────

#[test]
#[ignore = "spawns and kills a hung child process; run manually outside restricted sandboxes"]
#[serial]
fn nvidia_poll_times_out_on_hung_subprocess() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    let dir = TempDir::new().unwrap();
    let shim = dir.path().join("nvidia-smi");
    {
        let mut f = std::fs::File::create(&shim).unwrap();
        writeln!(f, "#!/bin/sh\nsleep 5").unwrap();
    } // drop f so the file is closed before exec (avoids ETXTBSY)
    let mut perms = std::fs::metadata(&shim).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&shim, perms).unwrap();

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", dir.path().display(), original_path);

    // PATH mutation is process-wide; #[serial] ensures no other test runs
    // concurrently while we have it pointed at the shim directory.
    unsafe {
        std::env::set_var("PATH", &new_path);
    }

    let mut provider = NvidiaProvider::smi_only(shim.clone());
    let start = Instant::now();
    let result = provider.poll().unwrap();
    let elapsed = start.elapsed();

    unsafe {
        std::env::set_var("PATH", original_path);
    }

    assert!(
        elapsed < std::time::Duration::from_millis(1500),
        "poll took {:?}, expected < 1.5s — timeout is not firing",
        elapsed
    );
    assert!(
        result.is_empty(),
        "poll should return empty on timeout, got {:?}",
        result
    );
}

#[test]
fn nvidia_unavailable_backend_does_not_spawn_repeatedly() {
    // Construct a provider already in the Unavailable state with a long backoff
    // and a missing binary path. Repeated polls must stay empty without panicking.
    let missing = std::path::PathBuf::from("/tmp/thermalwriter-no-such-nvidia-smi");
    let mut provider = NvidiaProvider::smi_only(missing);
    // First poll with a missing absolute path demotes to Unavailable.
    let _ = provider.poll().unwrap();
    for _ in 0..10 {
        assert!(provider.poll().unwrap().is_empty());
    }
}

#[test]
fn nvidia_nvml_or_smi_returns_gpu_temp_on_this_machine() {
    // Best-effort smoke: on the developer workstation with an NVIDIA GPU this
    // must produce at least gpu_temp. On machines without NVIDIA hardware the
    // provider returns empty — that is also success (no panic / no hang).
    let mut provider = NvidiaProvider::new();
    let start = std::time::Instant::now();
    let readings = provider.poll().unwrap();
    assert!(
        start.elapsed() < std::time::Duration::from_millis(1500),
        "nvidia poll took {:?}, expected < 1.5s",
        start.elapsed()
    );
    if !readings.is_empty() {
        assert!(
            readings.iter().any(|r| r.key == "gpu_temp"),
            "non-empty nvidia poll must include gpu_temp: {:?}",
            readings
        );
    }
}

// --- Slow/wireless hwmon chip protection ---
// A wedged WiFi NIC (e.g. ath12k firmware stall) turns a temp*_input read into
// a multi-second uninterruptible block, freezing the tick loop. The provider
// must never read wireless-NIC chips, and must quarantine any chip whose read
// stalls so it is skipped on subsequent polls.

fn make_fifo(path: &std::path::Path) {
    let cpath = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    let rc = unsafe { libc::mkfifo(cpath.as_ptr(), 0o644) };
    assert_eq!(rc, 0, "mkfifo failed");
}

#[test]
fn hwmon_skips_wireless_nic_chips() {
    let tmp = TempDir::new().unwrap();
    let wifi_dir = tmp.path().join("hwmon0");
    fs::create_dir_all(&wifi_dir).unwrap();
    fs::write(wifi_dir.join("name"), "ath12k_hwmon\n").unwrap();
    fs::write(wifi_dir.join("temp1_input"), "45000\n").unwrap();

    let cpu_dir = tmp.path().join("hwmon1");
    fs::create_dir_all(&cpu_dir).unwrap();
    fs::write(cpu_dir.join("name"), "k10temp\n").unwrap();
    fs::write(cpu_dir.join("temp1_input"), "72000\n").unwrap();
    fs::write(cpu_dir.join("temp1_label"), "Tctl\n").unwrap();

    let mut provider = HwmonProvider::with_base_path(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    assert!(
        !readings.iter().any(|r| r.key.contains("ath12k")),
        "wireless NIC chip must not be polled, got {:?}",
        readings
    );
    assert!(
        readings.iter().any(|r| r.key == "cpu_temp"),
        "CPU chip should still be read"
    );
}

#[test]
fn hwmon_quarantines_slow_chip_after_first_poll() {
    use std::time::{Duration, Instant};

    let tmp = TempDir::new().unwrap();
    let slow_dir = tmp.path().join("hwmon0");
    fs::create_dir_all(&slow_dir).unwrap();
    fs::write(slow_dir.join("name"), "slowchip\n").unwrap();
    let fifo = slow_dir.join("temp1_input");
    make_fifo(&fifo);

    let cpu_dir = tmp.path().join("hwmon1");
    fs::create_dir_all(&cpu_dir).unwrap();
    fs::write(cpu_dir.join("name"), "k10temp\n").unwrap();
    fs::write(cpu_dir.join("temp1_input"), "72000\n").unwrap();

    // First poll: reading temp1_input blocks until this writer opens the FIFO
    // 400ms later — well past the quarantine threshold.
    let fifo_clone = fifo.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        fs::write(&fifo_clone, "45000\n").unwrap();
    });

    let mut provider = HwmonProvider::with_base_path(tmp.path().to_path_buf());
    let first = provider.poll().unwrap();
    writer.join().unwrap();

    assert!(
        !first.iter().any(|r| r.key.contains("slowchip")),
        "slow chip readings should be dropped, got {:?}",
        first
    );

    // Second poll: a writer is standing by, so if the provider (incorrectly)
    // reads the FIFO again it gets a value quickly instead of hanging the test.
    let fifo_clone = fifo.clone();
    std::thread::spawn(move || {
        let _ = fs::write(&fifo_clone, "45000\n");
    });

    let start = Instant::now();
    let second = provider.poll().unwrap();
    let elapsed = start.elapsed();

    assert!(
        !second.iter().any(|r| r.key.contains("slowchip")),
        "quarantined chip must be skipped on later polls, got {:?}",
        second
    );
    assert!(
        second.iter().any(|r| r.key == "cpu_temp"),
        "healthy chips should still be read after quarantine"
    );
    assert!(
        elapsed < Duration::from_millis(250),
        "post-quarantine poll should be fast, took {:?}",
        elapsed
    );
}
