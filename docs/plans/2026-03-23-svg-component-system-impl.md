# SVG Component System — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use forge:executing-plans to implement this plan task-by-task.

**Goal:** Build a composable SVG component library with sensor history, backgrounds, btop-style visualizations, and a theme palette system.

**Architecture:** Extend the existing SVG rendering pipeline with: (1) a sensor history ring buffer fed by SensorHub, (2) Tera custom functions backed by Rust that emit SVG fragments, (3) a theme palette injected into Tera context, (4) GIF/video animation frame management. The SVG template remains the single rendering format — components are Tera function calls within templates.

**Tech Stack:** Rust, Tera 1.x (custom functions), resvg/usvg (SVG→Pixmap), tiny-skia, sysinfo (per-core CPU, network), image crate (GIF decoding), serde/toml (config), base64 (image embedding)

**Required Skills:**
- `designing-layouts`: Invoke after Task 22 — update skill with component catalog and composability rules
- `forge:writing-tests`: Follow TDD discipline for all implementation tasks

## Context for Executor

### Key Files
- `src/render/svg.rs:1-78` — SvgRenderer: Tera::one_off() → usvg → resvg. Must switch to persistent Tera instance.
- `src/render/mod.rs:1-71` — FrameSource trait, SensorData type alias (HashMap<String, String>), TemplateRenderer
- `src/sensor/mod.rs:1-69` — SensorHub, SensorProvider trait, SensorReading struct
- `src/sensor/sysinfo_provider.rs:1-70` — SysinfoProvider: currently only aggregate cpu_util, ram_used, ram_total. Needs per-core CPU + network.
- `src/sensor/hwmon.rs:1-126` — HwmonProvider: reads /sys/class/hwmon. Needs per-core temp + CCD temp aliases.
- `src/service/tick.rs:95-164` — run_tick_loop: polls sensors → renders → encodes → sends. Must add history recording + decoupled poll/render rates.
- `src/config.rs:1-121` — Config struct with DisplayConfig + SensorsConfig. Needs ThemeConfig section.
- `src/main.rs:1-137` — Daemon startup: wires SensorHub, renderer, tick loop. Must wire SensorHistory + theme.
- `tests/sensor_tests.rs` — 27 sensor tests using fake sysfs (TempDir pattern). Follow this pattern for new sensor tests.
- `tests/render_tests.rs` — 7 render tests. Follow this pattern for component tests.
- `examples/preview_layout.rs` — Preview tool. Must support history-dependent layouts (pre-fill with synthetic data).
- `examples/render_layout.rs` — Hardware push tool. Must support history accumulation.
- `layouts/svg/neon-dash.svg` — Reference SVG layout showing Tera variables, gradients, text positioning.

### Research Findings

**Tera custom functions (Tera 1.x):**
- Implement `tera::Function` trait: `fn call(&self, args: &HashMap<String, Value>) -> tera::Result<Value>` and `fn is_safe(&self) -> bool`
- `is_safe() -> true` means the returned string won't be HTML-escaped — required since components return raw SVG markup
- Register with `tera.register_function("graph", GraphFunction { ... })`
- Functions receive only their explicit arguments, NOT the full Tera context. To pass history data, the renderer injects `cpu_util_history` as a context variable, and the template passes it: `{{ graph(data=cpu_util_history, ...) }}`
- Currently using `Tera::one_off()` which creates a throwaway Tera instance per render. Must switch to a persistent `Tera` instance created once with all functions registered.
- Autoescaping should be disabled for `.svg` templates since SVG is not HTML.

**resvg `<image>` element support:**
- resvg/usvg supports `<image>` with `href="data:image/png;base64,..."` (data URIs) — this is how we embed raster backgrounds
- Also supports file path references — requires `options.resources_dir` to be set
- Supports PNG, JPEG formats. GIF gives first frame only.
- `preserveAspectRatio` attribute is fully supported

**sysinfo per-core CPU (sysinfo 0.33):**
- `System::cpus()` returns `Vec<Cpu>` with per-core `cpu_usage()` (f32, 0-100) and `frequency()` (u64, MHz)
- Cores are indexed by position in the Vec — `cpus[0]` is core 0
- `refresh_cpu_usage()` must be called at least twice (with delay) for meaningful values — already handled by existing poll pattern

**k10temp CCD temperatures:**
- On AMD Zen3+, `/sys/class/hwmon/hwmonN/` with `name=k10temp` exposes `temp3_label=Tccd1`, `temp4_label=Tccd2` (etc.)
- Label text is the identifier — parse labels to detect CCD temps
- 9950X3D has 2 CCDs: CCD0 (X3D/V-Cache), CCD1 (standard)

**Network throughput via sysinfo:**
- `sysinfo::Networks` provides per-interface `received()` and `transmitted()` (cumulative bytes)
- Must compute delta between polls for bytes/sec — same pattern as RAPL energy counter
- `System::networks()` → iterate interfaces, sum all non-loopback

**Core-to-CCD mapping:**
- `/sys/devices/system/cpu/cpuN/topology/die_id` gives the CCD (die) for each logical CPU
- On 9950X3D: cores 0-15 → die_id 0 (CCD0), cores 16-31 → die_id 1 (CCD1)

### Relevant Patterns
- `src/sensor/rapl.rs:53-96` — Delta-based sensor (energy counter → watts). Follow this pattern for network throughput (bytes counter → bytes/sec).
- `src/sensor/hwmon.rs:46-78` — Fake sysfs testing with TempDir. Follow for CCD temp tests.
- `tests/sensor_tests.rs:9-24` — Integration test pattern: create fake sysfs tree, instantiate provider with custom base path, assert readings.

## Execution Architecture

**Team:** 3 devs, 1 spec reviewer, 1 quality reviewer
**Task dependencies:**
- Tasks 1-3 (sensor history, extended sensors, theme) are independent — can run in parallel
- Task 4 (component library foundation) depends on Task 1 (history) and Task 3 (theme)
- Tasks 5-7 (graph, btop, background components) depend on Task 4
- Tasks 5-7 are independent of each other — can run in parallel
- Task 8 (animation) depends on Task 7 (background component)
- Task 9 (tick loop integration) depends on Tasks 1, 3, 4, 8
- Task 10 (example/preview updates) depends on Task 9
- Task 11 (skill update) depends on all prior tasks

**Phases:**
- Phase 1: Foundation (Tasks 1-3) — sensor history, extended sensors, theme palette
- Phase 2: Component Library (Tasks 4-8) — Tera functions, graphs, btop, backgrounds, animation
- Phase 3: Integration (Tasks 9-11) — tick loop, examples, skill update
- Phase 4: Showcase (Task 12) — demo layout using all components

**Milestones:**
- After Phase 1 (before Task 4): foundation verified, sensors emit per-core data, history buffers work
- After Phase 2 (before Task 9): all components render correct SVG, tested in isolation
- After Phase 3 (before Task 12): full pipeline works end-to-end, preview_layout supports components
- After Phase 4 (final): showcase layout rendered on hardware

---

## Phase 1: Foundation

### Task 1: Implement SensorHistory buffer [READ-DO]

**Files:**
- Create: `src/sensor/history.rs`
- Modify: `src/sensor/mod.rs:1-10` — add `pub mod history;`
- Test: `tests/sensor_history_tests.rs`

**Step 1: Write the failing test for basic recording and querying**

```rust
// tests/sensor_history_tests.rs
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
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test sensor_history_tests history_records_and_queries -- --nocapture`
Expected: compilation error — `sensor::history` module doesn't exist

**Step 3: Implement SensorHistory**

```rust
// src/sensor/history.rs
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
```

**Step 4: Add module declaration**

In `src/sensor/mod.rs`, add after line 6 (`pub mod rapl;`):
```rust
pub mod history;
```

**Step 5: Run test to verify it passes**

Run: `cargo test --test sensor_history_tests history_records_and_queries -- --nocapture`
Expected: PASS

**Step 6: Write additional tests**

```rust
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
```

**Step 7: Run all history tests**

Run: `cargo test --test sensor_history_tests -- --nocapture`
Expected: all PASS

**Step 8: Commit**

```bash
git add src/sensor/history.rs src/sensor/mod.rs tests/sensor_history_tests.rs
git commit -m "feat: add SensorHistory ring buffer for time-series sensor data"
```

---

### Task 2: Review Task 1

**Trigger:** Both reviewers start simultaneously when Task 1 completes.

**Killer items (blocking):**
- [ ] `SensorHistory::record()` in `src/sensor/history.rs` only stores values for metrics previously registered via `configure_metric()` — verify by checking `history_ignores_unconfigured_metrics` test exists and passes
- [ ] `SensorHistory::query()` returns empty Vec (not panic) when metric has no data — verify `history_skips_non_numeric_values` test
- [ ] Old entries are actually pruned in `record()` — verify `history_prunes_old_entries` test and check the `while` loop in `record()` compares against `now - config.max_duration`
- [ ] `inject_into_context()` uses `format!("{}_history", key)` naming convention — verify in test and code
- [ ] `SensorHistory` is `Send` (required for tick loop) — check there are no `Rc`, `RefCell`, or other non-Send types
- [ ] Test assertions check actual values (`assert!((values[0] - 65.0).abs() < 0.01)`), not just `!is_empty()`

**Quality items (non-blocking):**
- [ ] No unnecessary `clone()` calls on hot paths
- [ ] `query()` downsample logic handles edge cases (count=0, count=1, count > buf.len())

---

### Task 3: Milestone — Sensor history buffer

**Present to user:**
- SensorHistory module implemented with configure/record/query/inject API
- Test results and coverage
- Any surprises or deviations from design

**Wait for user response before proceeding to Task 4.**

---

### Task 4: Extend SysinfoProvider with per-core CPU and network sensors [DO-CONFIRM]

**Files:**
- Modify: `src/sensor/sysinfo_provider.rs:1-70`
- Test: `tests/sensor_tests.rs` (add new tests at end)

**Implement:** Extend `SysinfoProvider::poll()` to emit:
- `cpu_c0_util` through `cpu_cN_util` — per-core utilization % (from `self.sys.cpus()[i].cpu_usage()`)
- `cpu_c0_freq` through `cpu_cN_freq` — per-core frequency MHz (from `self.sys.cpus()[i].frequency()`)
- `net_rx` and `net_tx` — network throughput in bytes/sec (delta-based, like RAPL)

For network: add `networks: sysinfo::Networks` field to `SysinfoProvider`. On each poll, compute delta bytes since last poll, divide by elapsed time. Sum all non-loopback interfaces. Follow the RAPL delta pattern in `src/sensor/rapl.rs:53-96`.

For per-core: iterate `self.sys.cpus()` by index, emit `cpu_c{i}_util` and `cpu_c{i}_freq`.

**Confirm checklist:**
- [ ] Failing tests written FIRST for per-core util, per-core freq, and network throughput
- [ ] Per-core keys use format `cpu_c{i}_util`, `cpu_c{i}_freq` where i is 0-indexed core number
- [ ] Network throughput uses delta between polls (not cumulative bytes) — first poll returns no net_rx/net_tx (like RAPL)
- [ ] Network sums all non-loopback interfaces — check interface name != "lo"
- [ ] `available_sensors()` updated to include new keys
- [ ] Existing tests still pass (`cargo test --test sensor_tests`)
- [ ] Committed with clear message

---

### Task 5: Extend HwmonProvider with per-core temps and CCD aliases [DO-CONFIRM]

**Files:**
- Modify: `src/sensor/hwmon.rs:1-126`
- Test: `tests/sensor_tests.rs` (add new tests at end)

**Implement:** Extend HwmonProvider to emit:
- `cpu_c{i}_temp` — per-core temperature aliases. When `chip_name` is in `CPU_CHIP_NAMES` and `temp{n}_label` contains "Core" (e.g., "Core 0", "Core 1"), emit `cpu_c{core_num}_temp` alias.
- `cpu_ccd{n}_temp` — CCD temperature aliases. When `temp{n}_label` matches `Tccd1`, `Tccd2`, etc., emit `cpu_ccd0_temp`, `cpu_ccd1_temp` (0-indexed from the label number).

Follow existing alias pattern at `src/sensor/hwmon.rs:70-78` (the `cpu_temp_aliased` pattern).

**Confirm checklist:**
- [ ] Failing tests written FIRST using fake sysfs (TempDir pattern from existing hwmon tests)
- [ ] Per-core temp alias format: `cpu_c{N}_temp` where N comes from "Core N" label text
- [ ] CCD temp alias format: `cpu_ccd{N}_temp` where N = Tccd label number - 1 (Tccd1 → ccd0)
- [ ] Non-CPU chips don't emit per-core or CCD aliases
- [ ] Existing hwmon tests still pass
- [ ] Committed with clear message

---

### Task 6: Implement ThemePalette and config [DO-CONFIRM]

**Files:**
- Create: `src/theme.rs`
- Modify: `src/config.rs:53-58` — add `pub theme: ThemeConfig` to Config struct
- Modify: `src/lib.rs:1-6` — add `pub mod theme;`
- Test: `tests/theme_tests.rs`

**Implement:**

```rust
// src/theme.rs
use serde::{Deserialize, Serialize};
use anyhow::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemePalette {
    pub primary: String,
    pub secondary: String,
    pub accent: String,
    pub background: String,
    pub surface: String,
    pub text: String,
    pub text_dim: String,
    pub success: String,
    pub warning: String,
    pub critical: String,
}

impl Default for ThemePalette {
    fn default() -> Self {
        Self {
            primary: "#e94560".to_string(),
            secondary: "#53d8fb".to_string(),
            accent: "#20f5d8".to_string(),
            background: "#08080f".to_string(),
            surface: "#12121e".to_string(),
            text: "#e0e0e0".to_string(),
            text_dim: "#888888".to_string(),
            success: "#00ff88".to_string(),
            warning: "#ffaa00".to_string(),
            critical: "#ff3333".to_string(),
        }
    }
}

impl ThemePalette {
    /// Inject all theme colors into a Tera context as theme_primary, theme_secondary, etc.
    pub fn inject_into_context(&self, context: &mut tera::Context) {
        context.insert("theme_primary", &self.primary);
        context.insert("theme_secondary", &self.secondary);
        context.insert("theme_accent", &self.accent);
        context.insert("theme_background", &self.background);
        context.insert("theme_surface", &self.surface);
        context.insert("theme_text", &self.text);
        context.insert("theme_text_dim", &self.text_dim);
        context.insert("theme_success", &self.success);
        context.insert("theme_warning", &self.warning);
        context.insert("theme_critical", &self.critical);
    }
}

pub trait ThemeSource: Send {
    fn name(&self) -> &str;
    fn load(&self) -> Result<ThemePalette>;
}

pub struct DefaultThemeSource;

impl ThemeSource for DefaultThemeSource {
    fn name(&self) -> &str { "default" }
    fn load(&self) -> Result<ThemePalette> { Ok(ThemePalette::default()) }
}

pub struct ManualThemeSource {
    palette: ThemePalette,
}

impl ManualThemeSource {
    pub fn new(palette: ThemePalette) -> Self { Self { palette } }
}

impl ThemeSource for ManualThemeSource {
    fn name(&self) -> &str { "manual" }
    fn load(&self) -> Result<ThemePalette> { Ok(self.palette.clone()) }
}
```

Config addition in `src/config.rs`:
```rust
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ThemeConfig {
    pub source: String,  // "default" or "manual"
    pub background_image: Option<String>,
    pub manual: Option<ThemePalette>,
}

// Add to Config struct:
pub theme: ThemeConfig,
```

**Confirm checklist:**
- [ ] Failing tests written FIRST — test default palette values, inject_into_context, manual source override
- [ ] ThemePalette::default() uses the neon-dash palette colors from `skills/designing-layouts/references/color-system.md`
- [ ] `inject_into_context` uses `theme_` prefix for all keys (e.g., `theme_primary`)
- [ ] Config deserializes with defaults when `[theme]` section is missing (serde default)
- [ ] ThemeSource trait is `Send` (for async context)
- [ ] Existing config tests still pass
- [ ] Committed with clear message

---

### Task 7: Review Tasks 4-6

**Trigger:** Both reviewers start simultaneously when Tasks 4-6 complete.

**Killer items (blocking):**
- [ ] Per-core CPU keys in `sysinfo_provider.rs` use 0-indexed `cpu_c{i}_util` / `cpu_c{i}_freq` — grep for the format string
- [ ] Network throughput in `sysinfo_provider.rs` uses delta-based computation with stored previous bytes + timestamp — check for `last_net_*` fields
- [ ] Network first poll returns no `net_rx`/`net_tx` (like RAPL) — check for `Option` guard
- [ ] CCD temp alias in `hwmon.rs` converts Tccd1 → `cpu_ccd0_temp` (0-indexed) — check parse logic
- [ ] `ThemePalette::default()` primary is `#e94560`, secondary is `#53d8fb` — compare against `references/color-system.md:69-74`
- [ ] `Config` struct with `theme: ThemeConfig` deserializes from existing config.toml without error — test with empty `[theme]` and missing `[theme]`
- [ ] All new tests assert on specific values, not just non-empty

**Quality items (non-blocking):**
- [ ] Network interface filtering excludes "lo" but includes common names (eth*, wlan*, enp*, wlp*)
- [ ] Per-core sensor count matches `self.sys.cpus().len()` — no off-by-one

---

### Task 8: Milestone — Foundation complete

**Present to user:**
- SensorHistory buffer working with configure/record/query/inject
- Per-core CPU (util, freq, temp), CCD temps, and network throughput sensors working
- ThemePalette with default + manual sources, config integration
- Test results across all new modules
- Run `cargo test` to show full suite passes

**Wait for user response before proceeding to Phase 2.**

---

## Phase 2: Component Library

### Task 9: Build component library foundation and graph component [READ-DO]

**Files:**
- Create: `src/render/components/mod.rs`
- Create: `src/render/components/graph.rs`
- Modify: `src/render/mod.rs:1-10` — add `pub mod components;`
- Modify: `src/render/svg.rs:1-78` — switch from Tera::one_off() to persistent Tera instance
- Test: `tests/component_tests.rs`

**Step 1: Write failing test for graph component SVG output**

```rust
// tests/component_tests.rs
use std::collections::HashMap;
use tera::{Context, Tera, Value};

#[test]
fn graph_component_emits_svg_polyline() {
    let mut tera = Tera::default();
    thermalwriter::render::components::register_all(&mut tera);

    let mut context = Context::new();
    context.insert("cpu_util_history", &vec![10.0, 30.0, 50.0, 70.0, 90.0]);

    let template = r#"{{ graph(data=cpu_util_history, x=0, y=0, w=200, h=100, style="line", stroke="#ff0000") }}"#;
    tera.add_raw_template("test.svg", template).unwrap();
    let result = tera.render("test.svg", &context).unwrap();

    assert!(result.contains("<polyline"), "Should contain a polyline element");
    assert!(result.contains("stroke=\"#ff0000\""), "Should use specified stroke color");
    assert!(result.contains("<g"), "Should be wrapped in a <g> group");
}

#[test]
fn graph_component_area_style_emits_polygon() {
    let mut tera = Tera::default();
    thermalwriter::render::components::register_all(&mut tera);

    let mut context = Context::new();
    context.insert("cpu_util_history", &vec![10.0, 50.0, 90.0]);

    let template = r#"{{ graph(data=cpu_util_history, x=10, y=10, w=100, h=50, style="area", fill="#ff000033") }}"#;
    tera.add_raw_template("test.svg", template).unwrap();
    let result = tera.render("test.svg", &context).unwrap();

    assert!(result.contains("<polygon"), "Area style should use polygon");
    assert!(result.contains("fill=\"#ff000033\""), "Should use specified fill");
}

#[test]
fn graph_component_empty_data_returns_empty_group() {
    let mut tera = Tera::default();
    thermalwriter::render::components::register_all(&mut tera);

    let mut context = Context::new();
    context.insert("empty_history", &Vec::<f64>::new());

    let template = r#"{{ graph(data=empty_history, x=0, y=0, w=200, h=100) }}"#;
    tera.add_raw_template("test.svg", template).unwrap();
    let result = tera.render("test.svg", &context).unwrap();

    // Should return valid SVG (empty group), not error
    assert!(result.contains("<g"));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --test component_tests -- --nocapture`
Expected: compilation error — components module doesn't exist

**Step 3: Create components module and graph component**

Create `src/render/components/mod.rs`:
```rust
pub mod graph;

use tera::Tera;

/// Register all component Tera functions on a Tera instance.
pub fn register_all(tera: &mut Tera) {
    tera.register_function("graph", graph::GraphFunction);
}
```

Create `src/render/components/graph.rs`:
```rust
use std::collections::HashMap;
use tera::{Function, Result, Value};

/// Tera function that emits SVG line/area graph fragments.
///
/// Arguments:
///   data: array of f64 values (from history injection)
///   x, y, w, h: bounding box (defaults: 0, 0, 480, 100)
///   style: "line" or "area" (default: "line")
///   stroke: stroke color (default: "#e94560")
///   fill: fill color for area style (default: "none")
///   stroke_width: line width (default: 2)
pub struct GraphFunction;

impl Function for GraphFunction {
    fn call(&self, args: &HashMap<String, Value>) -> Result<Value> {
        let data = match args.get("data") {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_f64())
                .collect::<Vec<f64>>(),
            _ => return Ok(Value::String("<g></g>".to_string())),
        };

        if data.is_empty() {
            return Ok(Value::String("<g></g>".to_string()));
        }

        let x = args.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let y = args.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let w = args.get("w").and_then(|v| v.as_f64()).unwrap_or(480.0);
        let h = args.get("h").and_then(|v| v.as_f64()).unwrap_or(100.0);
        let style = args.get("style").and_then(|v| v.as_str()).unwrap_or("line");
        let stroke = args.get("stroke").and_then(|v| v.as_str()).unwrap_or("#e94560");
        let fill = args.get("fill").and_then(|v| v.as_str()).unwrap_or("none");
        let stroke_width = args.get("stroke_width").and_then(|v| v.as_f64()).unwrap_or(2.0);

        // Compute min/max for Y-axis scaling
        let min_val = data.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_val = data.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let range = if (max_val - min_val).abs() < 0.001 { 1.0 } else { max_val - min_val };

        // Generate points
        let step_x = if data.len() > 1 { w / (data.len() - 1) as f64 } else { 0.0 };
        let points: Vec<String> = data
            .iter()
            .enumerate()
            .map(|(i, &val)| {
                let px = x + i as f64 * step_x;
                let normalized = (val - min_val) / range;
                let py = y + h - (normalized * h);
                format!("{:.1},{:.1}", px, py)
            })
            .collect();

        let points_str = points.join(" ");

        let svg = match style {
            "area" => {
                // Polygon: line points + bottom-right + bottom-left to close the area
                let bottom_right = format!("{:.1},{:.1}", x + w, y + h);
                let bottom_left = format!("{:.1},{:.1}", x, y + h);
                format!(
                    r#"<g><polygon points="{} {} {}" fill="{}" stroke="{}" stroke-width="{}"/></g>"#,
                    points_str, bottom_right, bottom_left, fill, stroke, stroke_width
                )
            }
            _ => {
                // Line: polyline
                format!(
                    r#"<g><polyline points="{}" fill="none" stroke="{}" stroke-width="{}" stroke-linejoin="round"/></g>"#,
                    points_str, stroke, stroke_width
                )
            }
        };

        Ok(Value::String(svg))
    }

    fn is_safe(&self) -> bool {
        true // Don't HTML-escape the SVG output
    }
}
```

**Step 4: Add module declaration**

In `src/render/mod.rs`, add after line 7 (`pub mod svg;`):
```rust
pub mod components;
```

**Step 5: Run tests to verify they pass**

Run: `cargo test --test component_tests -- --nocapture`
Expected: all PASS

**Step 6: Refactor SvgRenderer to use persistent Tera instance**

Modify `src/render/svg.rs` — replace `Tera::one_off()` with a persistent `Tera` instance that has components registered:

```rust
pub struct SvgRenderer<'a> {
    tera: Tera,
    template_name: String,
    width: u32,
    height: u32,
    options: usvg::Options<'a>,
}

impl<'a> SvgRenderer<'a> {
    pub fn new(template: &str, width: u32, height: u32) -> Result<Self> {
        let mut options = usvg::Options::default();
        options.font_family = EMBEDDED_FONT_FAMILY.to_string();
        options.fontdb_mut().load_font_data(EMBEDDED_FONT.to_vec());
        options.fontdb_mut().load_system_fonts();
        options.fontdb_mut().set_monospace_family(EMBEDDED_FONT_FAMILY);

        let mut tera = Tera::default();
        tera.autoescape_on(vec![]); // Disable autoescaping for SVG
        super::components::register_all(&mut tera);
        tera.add_raw_template("layout", template)
            .context("Failed to add template to Tera")?;

        Ok(Self {
            tera,
            template_name: "layout".to_string(),
            width,
            height,
            options,
        })
    }
}
```

Update `render()` to use `self.tera.render()` instead of `Tera::one_off()`.
Update `set_template()` to call `self.tera.add_raw_template("layout", template)`.

**Step 7: Run full test suite**

Run: `cargo test`
Expected: all tests PASS (existing SVG rendering tests should still work)

**Step 8: Commit**

```bash
git add src/render/components/ src/render/mod.rs src/render/svg.rs tests/component_tests.rs
git commit -m "feat: add component library foundation with graph Tera function"
```

---

### Task 10: Review Task 9

**Trigger:** Both reviewers start simultaneously when Task 9 completes.

**Killer items (blocking):**
- [ ] `GraphFunction::is_safe()` returns `true` — verify in `src/render/components/graph.rs`. Without this, SVG output gets HTML-escaped and breaks rendering.
- [ ] `SvgRenderer` calls `tera.autoescape_on(vec![])` — verify in `src/render/svg.rs`. Without this, Tera escapes `<` and `>` in component output.
- [ ] `GraphFunction::call()` returns `<g></g>` (not error) when data is empty or missing — verify `graph_component_empty_data_returns_empty_group` test
- [ ] Graph Y-axis scaling handles constant values (all same number) without division by zero — check `range` fallback to 1.0
- [ ] `SvgRenderer::set_template()` still works (re-adds template to persistent Tera instance) — check implementation
- [ ] Existing SVG layout tests (`cargo test --test render_tests`) still pass after SvgRenderer refactor

**Quality items (non-blocking):**
- [ ] Graph points use `{:.1}` formatting (1 decimal place) — not excessive precision that bloats SVG
- [ ] `register_all()` in mod.rs is the single registration point — no components registered elsewhere

---

### Task 11: Milestone — Graph component and SvgRenderer refactor

**Present to user:**
- Graph component generating SVG polyline/polygon from history data
- SvgRenderer switched to persistent Tera with component registration
- Render a test layout with a graph component using preview_layout (may need manual test SVG)
- Test results

**Wait for user response before proceeding to Task 12.**

---

### Task 12: Implement btop-style visualization components [READ-DO]

**Files:**
- Create: `src/render/components/btop.rs`
- Modify: `src/render/components/mod.rs` — add btop module and register functions
- Test: `tests/component_tests.rs` (add btop tests)

**Step 1: Write failing tests for btop_bars**

```rust
#[test]
fn btop_bars_emits_rect_grid() {
    let mut tera = Tera::default();
    thermalwriter::render::components::register_all(&mut tera);

    let mut context = Context::new();
    // 3 time samples, 2 cores
    context.insert("cpu_c0_util_history", &vec![20.0, 60.0, 90.0]);
    context.insert("cpu_c1_util_history", &vec![10.0, 40.0, 70.0]);

    let template = r#"{{ btop_bars(metrics=["cpu_c0_util", "cpu_c1_util"], x=0, y=0, w=120, h=40, color="#e94560") }}"#;
    tera.add_raw_template("test.svg", template).unwrap();
    let result = tera.render("test.svg", &context).unwrap();

    assert!(result.contains("<rect"), "Should contain rect elements");
    assert!(result.contains("<g"), "Should be wrapped in a group");
}

#[test]
fn btop_net_emits_mirrored_polygons() {
    let mut tera = Tera::default();
    thermalwriter::render::components::register_all(&mut tera);

    let mut context = Context::new();
    context.insert("net_rx_history", &vec![1000.0, 5000.0, 3000.0]);
    context.insert("net_tx_history", &vec![500.0, 2000.0, 1500.0]);

    let template = r#"{{ btop_net(rx_data=net_rx_history, tx_data=net_tx_history, x=0, y=0, w=200, h=100, rx_color="#53d8fb", tx_color="#e94560") }}"#;
    tera.add_raw_template("test.svg", template).unwrap();
    let result = tera.render("test.svg", &context).unwrap();

    // Should have two polygon elements (rx above, tx below center)
    let polygon_count = result.matches("<polygon").count();
    assert!(polygon_count >= 2, "Should have at least 2 polygons (rx + tx), got {}", polygon_count);
}

#[test]
fn btop_ram_emits_area_with_capacity_line() {
    let mut tera = Tera::default();
    thermalwriter::render::components::register_all(&mut tera);

    let mut context = Context::new();
    context.insert("ram_used_history", &vec![24.0, 25.0, 26.0, 24.5]);

    let template = r#"{{ btop_ram(data=ram_used_history, total=64.0, x=0, y=0, w=200, h=60, fill="#cc9eff") }}"#;
    tera.add_raw_template("test.svg", template).unwrap();
    let result = tera.render("test.svg", &context).unwrap();

    assert!(result.contains("<polygon"), "Should contain area polygon");
    assert!(result.contains("fill=\"#cc9eff\""), "Should use specified fill color");
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --test component_tests btop -- --nocapture`
Expected: compilation error or unregistered function error

**Step 3: Implement btop components**

Create `src/render/components/btop.rs` with three component structs:
- `BtopBarsFunction` — receives `metrics` (array of metric name prefixes), looks up `{name}_history` arrays from context args. Generates grid of `<rect>` elements. Each column is a time sample, each row is a metric. Rect opacity/color intensity scales with value (0-100%).
- `BtopNetFunction` — receives `rx_data` and `tx_data` arrays. Draws center axis line, RX polygon above, TX polygon below (mirrored).
- `BtopRamFunction` — receives `data` array and `total` (max value). Draws filled area polygon scaled to total.

All three follow the same pattern as `GraphFunction` — implement `tera::Function`, `is_safe() -> true`, parse args from HashMap, return SVG string.

**Step 4: Register in mod.rs**

```rust
pub mod btop;
// In register_all():
tera.register_function("btop_bars", btop::BtopBarsFunction);
tera.register_function("btop_net", btop::BtopNetFunction);
tera.register_function("btop_ram", btop::BtopRamFunction);
```

**Step 5: Run tests**

Run: `cargo test --test component_tests -- --nocapture`
Expected: all PASS

**Step 6: Commit**

```bash
git add src/render/components/btop.rs src/render/components/mod.rs tests/component_tests.rs
git commit -m "feat: add btop-style visualization components (bars, net, ram)"
```

---

### Task 13: Implement background component [READ-DO]

**Files:**
- Create: `src/render/components/background.rs`
- Modify: `src/render/components/mod.rs` — add background module and register
- Test: `tests/component_tests.rs` (add background tests)

**Step 1: Write failing tests**

```rust
#[test]
fn background_pattern_grid_emits_svg_pattern() {
    let mut tera = Tera::default();
    thermalwriter::render::components::register_all(&mut tera);

    let context = Context::new();
    let template = r#"{{ background(pattern="grid", color="#ffffff10", spacing=20) }}"#;
    tera.add_raw_template("test.svg", template).unwrap();
    let result = tera.render("test.svg", &context).unwrap();

    assert!(result.contains("<defs>"), "Should contain defs for pattern");
    assert!(result.contains("<pattern"), "Should contain pattern element");
    assert!(result.contains("<rect"), "Should contain rect using the pattern");
}

#[test]
fn background_image_emits_base64_image_tag() {
    let mut tera = Tera::default();
    thermalwriter::render::components::register_all(&mut tera);

    // Inject a pre-encoded background frame
    let mut context = Context::new();
    context.insert("__bg_image", &"iVBORw0KGgo="); // tiny base64 stub

    let template = r#"{{ background(image_data=__bg_image, w=480, h=480) }}"#;
    tera.add_raw_template("test.svg", template).unwrap();
    let result = tera.render("test.svg", &context).unwrap();

    assert!(result.contains("<image"), "Should contain image element");
    assert!(result.contains("data:image/png;base64,"), "Should use data URI");
}

#[test]
fn background_with_opacity_sets_attribute() {
    let mut tera = Tera::default();
    thermalwriter::render::components::register_all(&mut tera);

    let mut context = Context::new();
    context.insert("__bg_image", &"iVBORw0KGgo=");

    let template = r#"{{ background(image_data=__bg_image, w=480, h=480, opacity=0.3) }}"#;
    tera.add_raw_template("test.svg", template).unwrap();
    let result = tera.render("test.svg", &context).unwrap();

    assert!(result.contains("opacity=\"0.3\""), "Should set opacity attribute");
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --test component_tests background -- --nocapture`
Expected: unregistered function error

**Step 3: Implement background component**

Create `src/render/components/background.rs`:
- `BackgroundFunction` — handles three modes:
  1. `pattern=` arg: emit SVG `<defs><pattern>` + `<rect fill="url(#bg-pattern)">`. Support "grid", "dots", "carbon", "hexgrid" patterns.
  2. `image_data=` arg: emit `<image href="data:image/png;base64,{data}" ...>` (for pre-encoded raster/animation frames)
  3. `source=` arg: emit `<image href="{path}" ...>` (for file references — resvg resolves via resources_dir)
- All modes support `opacity`, `w`, `h` args (default 480x480).

**Step 4: Register in mod.rs**

```rust
pub mod background;
// In register_all():
tera.register_function("background", background::BackgroundFunction);
```

**Step 5: Run tests**

Run: `cargo test --test component_tests -- --nocapture`
Expected: all PASS

**Step 6: Commit**

```bash
git add src/render/components/background.rs src/render/components/mod.rs tests/component_tests.rs
git commit -m "feat: add background component (patterns, images, data URIs)"
```

---

### Task 14: Implement animation frame manager [DO-CONFIRM]

**Files:**
- Create: `src/render/components/animation.rs`
- Modify: `Cargo.toml` — add `image` features for GIF decoding, add `base64` dependency
- Test: `tests/animation_tests.rs`

**Implement:** An `AnimationSource` struct that:
1. Loads a GIF file, decodes all frames + per-frame delays using `image::codecs::gif::GifDecoder`
2. Stores frames as `Vec<(Duration, Vec<u8>)>` (delay, RGBA pixels) for eager mode
3. Provides `frame_at(elapsed: Duration) -> Option<&[u8]>` that returns the current frame based on elapsed time, looping automatically
4. Provides `base64_frame_at(elapsed: Duration) -> Option<String>` that returns the frame as a base64-encoded PNG string ready for SVG `<image>` embedding
5. Supports eager (all frames in memory) and streaming (placeholder for future video support — for now just eager)
6. Reports `native_fps()` based on average frame delay

Add `base64` crate to Cargo.toml dependencies. Add `"gif"` and `"png"` features to the `image` dependency.

**Confirm checklist:**
- [ ] Failing test written FIRST — create a tiny GIF in test (use `image` crate to write a 2-frame 2x2 GIF to tempfile), load it, verify frame_at returns correct frames
- [ ] Frame looping works: `frame_at(total_duration + small_offset)` returns first frame again
- [ ] `native_fps()` computed from average frame delay, not hardcoded
- [ ] `base64_frame_at()` returns a string that starts with valid base64 PNG data
- [ ] Memory: frames stored as decoded RGBA, not re-encoded each access (base64 encoding happens in `base64_frame_at`)
- [ ] GIF with 0-delay frames defaults to reasonable delay (e.g., 100ms)
- [ ] Committed with clear message

---

### Task 15: Review Tasks 12-14

**Trigger:** Both reviewers start simultaneously when Tasks 12-14 complete.

**Killer items (blocking):**
- [ ] All three btop components (`BtopBarsFunction`, `BtopNetFunction`, `BtopRamFunction`) have `is_safe() -> true` — grep `is_safe` in `btop.rs`
- [ ] `BtopBarsFunction` receives metric names and looks up `{name}_history` from context args — verify the naming convention matches what `SensorHistory::inject_into_context` produces
- [ ] `BtopNetFunction` mirroring logic: RX polygon Y values go from center line upward (y decreasing), TX polygon Y values go from center line downward (y increasing) — verify point generation math
- [ ] `BackgroundFunction` for patterns includes unique pattern IDs in `<defs>` to avoid SVG ID collisions if multiple backgrounds exist
- [ ] `AnimationSource::frame_at()` handles looping via modulo arithmetic on elapsed time — verify it doesn't panic on zero total duration
- [ ] base64 encoding in animation produces valid output — test round-trips (encode → decode should match original pixels)
- [ ] `Cargo.toml` image features include `"gif"` and `"png"` — check the features array

**Quality items (non-blocking):**
- [ ] btop_bars rect dimensions account for spacing between cells (not edge-to-edge)
- [ ] Background patterns are visually distinct (grid vs dots vs carbon)
- [ ] Animation frame cache drops old frames when layout changes (no memory leak across layout switches)

---

### Task 16: Milestone — All components implemented

**Present to user:**
- All components working: graph (line/area), btop_bars, btop_net, btop_ram, background (pattern/image/animation)
- Animation frame manager loading GIFs
- Render a test SVG using multiple components via preview_layout
- Test results across all component tests

**Wait for user response before proceeding to Phase 3.**

---

## Phase 3: Integration

### Task 17: Integrate SensorHistory and ThemePalette into tick loop and SvgRenderer [READ-DO]

**Files:**
- Modify: `src/service/tick.rs:95-164` — add SensorHistory recording, decoupled poll rate, theme/history context injection
- Modify: `src/render/svg.rs` — accept SensorHistory + ThemePalette for context injection
- Modify: `src/main.rs:1-137` — wire SensorHistory, ThemePalette, parse layout frontmatter
- Create: `src/render/frontmatter.rs` — parse `{# history: ... #}` and `{# animation: ... #}` from SVG templates
- Test: `tests/frontmatter_tests.rs`

**Step 1: Write failing test for frontmatter parser**

```rust
// tests/frontmatter_tests.rs
use thermalwriter::render::frontmatter::LayoutFrontmatter;
use std::time::Duration;

#[test]
fn parse_history_frontmatter() {
    let svg = r#"{# history: cpu_temp=60s, cpu_util=120s, net_rx=300s@0.2hz #}
<svg viewBox="0 0 480 480">...</svg>"#;

    let fm = LayoutFrontmatter::parse(svg);
    assert_eq!(fm.history_configs.len(), 3);

    let cpu_temp = &fm.history_configs["cpu_temp"];
    assert_eq!(cpu_temp.duration, Duration::from_secs(60));
    assert!(cpu_temp.sample_hz.is_none()); // uses default

    let net_rx = &fm.history_configs["net_rx"];
    assert_eq!(net_rx.duration, Duration::from_secs(300));
    assert!((net_rx.sample_hz.unwrap() - 0.2).abs() < 0.01);
}

#[test]
fn parse_animation_frontmatter() {
    let svg = r#"{# animation: fps=15, decode=stream #}
<svg viewBox="0 0 480 480">...</svg>"#;

    let fm = LayoutFrontmatter::parse(svg);
    assert_eq!(fm.animation_fps, Some(15));
    assert_eq!(fm.animation_decode.as_deref(), Some("stream"));
}

#[test]
fn missing_frontmatter_returns_defaults() {
    let svg = r#"<svg viewBox="0 0 480 480">...</svg>"#;
    let fm = LayoutFrontmatter::parse(svg);
    assert!(fm.history_configs.is_empty());
    assert!(fm.animation_fps.is_none());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test frontmatter_tests -- --nocapture`
Expected: compilation error

**Step 3: Implement frontmatter parser**

Create `src/render/frontmatter.rs`:
```rust
use std::collections::HashMap;
use std::time::Duration;

pub struct HistoryConfig {
    pub duration: Duration,
    pub sample_hz: Option<f64>,
}

pub struct LayoutFrontmatter {
    pub history_configs: HashMap<String, HistoryConfig>,
    pub animation_fps: Option<u32>,
    pub animation_decode: Option<String>,
}

impl LayoutFrontmatter {
    pub fn parse(template: &str) -> Self {
        let mut fm = Self {
            history_configs: HashMap::new(),
            animation_fps: None,
            animation_decode: None,
        };

        for line in template.lines() {
            let trimmed = line.trim();
            if let Some(inner) = trimmed.strip_prefix("{#").and_then(|s| s.strip_suffix("#}")) {
                let inner = inner.trim();
                if let Some(rest) = inner.strip_prefix("history:") {
                    fm.parse_history(rest.trim());
                } else if let Some(rest) = inner.strip_prefix("animation:") {
                    fm.parse_animation(rest.trim());
                }
            }
        }

        fm
    }

    fn parse_history(&mut self, spec: &str) {
        // Format: "cpu_temp=60s, cpu_util=120s, net_rx=300s@0.2hz"
        for part in spec.split(',') {
            let part = part.trim();
            if let Some((key, rest)) = part.split_once('=') {
                let key = key.trim();
                let rest = rest.trim();
                let (duration_str, hz) = if let Some((d, h)) = rest.split_once('@') {
                    (d.trim(), h.trim().strip_suffix("hz").and_then(|s| s.parse::<f64>().ok()))
                } else {
                    (rest, None)
                };
                if let Some(secs_str) = duration_str.strip_suffix('s') {
                    if let Ok(secs) = secs_str.parse::<u64>() {
                        self.history_configs.insert(key.to_string(), HistoryConfig {
                            duration: Duration::from_secs(secs),
                            sample_hz: hz,
                        });
                    }
                }
            }
        }
    }

    fn parse_animation(&mut self, spec: &str) {
        // Format: "fps=15, decode=stream"
        for part in spec.split(',') {
            let part = part.trim();
            if let Some((key, val)) = part.split_once('=') {
                match key.trim() {
                    "fps" => self.animation_fps = val.trim().parse().ok(),
                    "decode" => self.animation_decode = Some(val.trim().to_string()),
                    _ => {}
                }
            }
        }
    }
}
```

**Step 4: Run frontmatter tests**

Run: `cargo test --test frontmatter_tests -- --nocapture`
Expected: all PASS

**Step 5: Modify SvgRenderer::render() to accept history + theme**

Update `FrameSource::render()` signature — this is a breaking change to the trait. Two options:
- Option A: Add `SensorHistory` and `ThemePalette` as fields on `SvgRenderer` (set at construction or via setter)
- Option B: Expand `render()` signature

Go with **Option A** — add optional fields to `SvgRenderer`, inject into Tera context during render. The `FrameSource` trait signature stays the same (`render(&mut self, sensors: &SensorData) -> Result<Pixmap>`), and the SvgRenderer handles context enrichment internally.

Add to `SvgRenderer`:
```rust
pub fn set_history(&mut self, history: std::sync::Arc<std::sync::Mutex<SensorHistory>>) { ... }
pub fn set_theme(&mut self, theme: ThemePalette) { ... }
```

In `render()`, before calling `self.tera.render()`:
1. Build Tera context from sensors (existing)
2. If theme is set, call `theme.inject_into_context(&mut context)`
3. If history is set, lock it, call `history.inject_into_context(&mut context, sample_count)` where sample_count comes from frontmatter or defaults to 60

**Step 6: Modify tick loop for decoupled poll/render rates**

In `src/service/tick.rs`, modify `run_tick_loop` to:
1. Accept `SensorHistory` (behind `Arc<Mutex<>>`)
2. Track last sensor poll time separately from render tick
3. On each render tick: check if `poll_interval` has elapsed since last poll. If yes: poll sensors, record into history. If no: reuse cached data.
4. After polling/caching: render frame (existing logic)

**Step 7: Wire in main.rs**

In `src/main.rs`:
1. Parse frontmatter from loaded template: `let frontmatter = LayoutFrontmatter::parse(&template);`
2. Create `SensorHistory`, configure metrics from frontmatter
3. Create `ThemePalette` from config
4. Pass both to `SvgRenderer` via setters
5. Pass `Arc<Mutex<SensorHistory>>` to tick loop

**Step 8: Run full test suite**

Run: `cargo test`
Expected: all PASS

**Step 9: Commit**

```bash
git add src/render/frontmatter.rs src/render/svg.rs src/render/mod.rs src/service/tick.rs src/main.rs tests/frontmatter_tests.rs
git commit -m "feat: integrate sensor history, theme palette, and frontmatter into render pipeline"
```

---

### Task 18: Review Task 17

**Trigger:** Both reviewers start simultaneously when Task 17 completes.

**Killer items (blocking):**
- [ ] Frontmatter parser in `src/render/frontmatter.rs` handles missing frontmatter gracefully (returns empty configs) — verify `missing_frontmatter_returns_defaults` test
- [ ] `SvgRenderer::render()` signature unchanged (`&mut self, sensors: &SensorData`) — FrameSource trait not broken. History/theme set via fields.
- [ ] Tick loop sensor poll is decoupled from render tick — verify separate `last_poll_time` tracking in `tick.rs`. Sensor poll should NOT happen every render tick.
- [ ] `SensorHistory` is behind `Arc<Mutex<>>` in tick loop — verify thread safety for async context
- [ ] History `inject_into_context` called BEFORE `tera.render()` — verify ordering in SvgRenderer::render()
- [ ] Theme `inject_into_context` called BEFORE `tera.render()` — verify ordering
- [ ] All existing tests still pass (`cargo test`)

**Quality items (non-blocking):**
- [ ] Frontmatter parsing is lenient (doesn't crash on malformed input)
- [ ] Lock on SensorHistory Mutex is held for minimal duration (record + query, then release)

---

### Task 19: Milestone — Pipeline integrated

**Present to user:**
- Full pipeline: sensors → history → Tera context (with theme + history arrays) → components → SVG → resvg → Pixmap
- Frontmatter parsing working
- Decoupled poll/render rates in tick loop
- Create a test SVG template using graph component + theme colors, render with preview_layout
- Full test suite results

**Wait for user response before proceeding to Task 20.**

---

### Task 20: Update preview_layout and render_layout examples [DO-CONFIRM]

**Files:**
- Modify: `examples/preview_layout.rs:67-100` — add history pre-fill with synthetic data, theme support
- Modify: `examples/render_layout.rs:88-177` — add history accumulation during render loop, theme support

**Implement:**

For `preview_layout`: Since there's no time-series data available in a single-shot render, generate synthetic history data (e.g., a sine wave or random walk for each configured metric) so graph components have data to display. Parse frontmatter from loaded template, create SensorHistory, fill with synthetic samples, pass to SvgRenderer.

For `render_layout`: Accumulate real sensor history across the render loop. Parse frontmatter, create SensorHistory, record each poll cycle. Pass to SvgRenderer.

Both: Create ThemePalette from default (or from config if available), pass to SvgRenderer.

**Confirm checklist:**
- [ ] Failing test written FIRST (at minimum: a test that render_layout compiles and preview_layout compiles with new imports)
- [ ] preview_layout generates synthetic history that produces visible graphs (not empty)
- [ ] render_layout accumulates real history across the render loop duration
- [ ] Both examples pass ThemePalette to SvgRenderer
- [ ] Both examples parse frontmatter from loaded templates
- [ ] `--mock` mode in render_layout generates mock history too (not just current values)
- [ ] Committed with clear message

---

### Task 21: Review Task 20

**Trigger:** Both reviewers start simultaneously when Task 20 completes.

**Killer items (blocking):**
- [ ] preview_layout synthetic history data produces at least 30 data points per configured metric — check the generation loop
- [ ] render_layout history recording happens on each poll cycle inside the render loop — check that `history.record()` is called per iteration
- [ ] Both examples compile and run without errors: `cargo build --examples`
- [ ] preview_layout with neon-dash.svg (no frontmatter) still works — no regression from frontmatter parsing
- [ ] Theme colors appear in rendered output — render a test SVG with `{{ theme_primary }}` and verify it resolves

**Quality items (non-blocking):**
- [ ] Synthetic history data is deterministic (seeded) for reproducible previews
- [ ] render_layout prints history buffer stats at end (e.g., "Recorded 60 samples for cpu_temp")

---

### Task 22: Update designing-layouts skill with component catalog [DO-CONFIRM]

**Files:**
- Modify: `skills/designing-layouts/SKILL.md`
- Create: `skills/designing-layouts/references/components.md`

**Implement:** Update the skill with:

1. **Component catalog** in a new reference doc listing all components with their full signatures, required arguments, optional arguments with defaults, and example usage
2. **Composability rules** section in the main SKILL.md (the 8 rules from the design doc)
3. **History frontmatter documentation** — syntax and examples
4. **Theme variables** — list of `theme_*` variables available in templates
5. **Updated sensor keys** — add per-core, CCD, and network keys to the sensor table
6. **Layering guide** — background → graphs → panels → text, with SVG document order explanation
7. **Example layout** using multiple components together

**Confirm checklist:**
- [ ] All 8 composability rules from design doc are in SKILL.md
- [ ] Component catalog includes: graph, btop_bars, btop_net, btop_ram, background
- [ ] Each component entry has: description, all arguments with types and defaults, example SVG snippet
- [ ] History frontmatter syntax documented with examples
- [ ] Theme variable names match exactly what `ThemePalette::inject_into_context()` produces
- [ ] New sensor keys (cpu_c*_util, cpu_c*_temp, cpu_c*_freq, cpu_ccd*_temp, net_rx, net_tx) documented
- [ ] Example layout demonstrates at least 3 different components together
- [ ] Committed with clear message

---

### Task 23: Review Task 22

**Trigger:** Both reviewers start simultaneously when Task 22 completes.

**Killer items (blocking):**
- [ ] Composability rules in SKILL.md match the 8 rules from `docs/plans/2026-03-23-svg-component-system-design.md` — diff them
- [ ] Component signatures in `references/components.md` match actual Rust implementations — grep function arg names in `src/render/components/`
- [ ] Theme variable names in skill match `inject_into_context()` output — check `theme_primary` through `theme_critical`
- [ ] Sensor key names in skill match actual provider output — check `cpu_c{i}_util` pattern in `sysinfo_provider.rs` and `cpu_ccd{n}_temp` in `hwmon.rs`
- [ ] History frontmatter syntax documented matches parser in `src/render/frontmatter.rs`

**Quality items (non-blocking):**
- [ ] Example layout in skill is syntactically valid SVG
- [ ] Skill references link to components.md correctly

---

### Task 24: Milestone — Integration complete

**Present to user:**
- Full pipeline working end-to-end
- Examples updated with history and theme support
- Designing-layouts skill updated with component catalog and composability rules
- Full test suite results
- Render a component-rich test layout with preview_layout and show the output

**Wait for user response before proceeding to Phase 4.**

---

## Phase 4: Showcase

### Task 25: Create showcase layout using all components [DO-CONFIRM]

**Files:**
- Create: `layouts/svg/component-showcase.svg`

**Implement:** Create a layout that demonstrates all component types working together:
- Animated or patterned background
- CPU panel with temperature graph sparkline behind it
- btop_bars showing per-core CPU utilization
- btop_net showing network traffic
- btop_ram showing memory usage
- Theme-aware colors throughout
- Value-based threshold coloring on temperature

Use the designing-layouts skill for design guidance. This layout serves as both a demo and a regression test for the component system.

**Confirm checklist:**
- [ ] Layout renders without errors via `cargo run --example preview_layout layouts/svg/component-showcase.svg`
- [ ] All 5+ component types visible in rendered output
- [ ] History frontmatter declares all needed metrics
- [ ] Theme variables used for colors (not hardcoded hex everywhere)
- [ ] Threshold coloring working on at least one metric (Tera conditionals)
- [ ] Layout pushed to hardware via render_layout and visually inspected
- [ ] Committed with clear message

---

### Task 26: Review Task 25

**Trigger:** Both reviewers start simultaneously when Task 25 completes.

**Killer items (blocking):**
- [ ] Layout renders without errors in preview_layout — run it and check exit code
- [ ] All component calls use correct argument names matching the registered Tera functions
- [ ] History frontmatter metrics match the metrics referenced by components
- [ ] No text below #888888 brightness (LCD visibility rule from SKILL.md)
- [ ] Layout uses `{{ default(value="--") }}` for all sensor values

**Quality items (non-blocking):**
- [ ] Visual balance — components don't overlap or leave large empty areas
- [ ] Color hierarchy maintained (hero values bright, labels dim, backgrounds dark)
- [ ] Layout would look good on actual hardware (not just PNG preview)

---

### Task 27: Milestone — Final

**Present to user:**
- Showcase layout rendering on hardware
- Screenshot/photo of display
- Full test suite results (`cargo test`)
- Summary of everything implemented:
  - SensorHistory ring buffer
  - 5 component Tera functions (graph, btop_bars, btop_net, btop_ram, background)
  - Animation frame manager
  - ThemePalette system
  - Extended sensors (per-core CPU, CCD temps, network)
  - Frontmatter parser
  - Tick loop with decoupled poll/render rates
  - Updated designing-layouts skill
  - Showcase layout

**This is the final milestone. Wait for user approval.**
