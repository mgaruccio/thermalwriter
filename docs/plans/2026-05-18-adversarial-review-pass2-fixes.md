# Adversarial Review Pass-2 Fixes — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use `forge:executing-plans` to implement this plan task-by-task.

**Goal:** Resolve the four confirmed CRITICAL/MAJOR findings from the 2026-05-18 second-pass adversarial review (Gemini gemini-3-pro-preview + independent Claude critique). Verified false positives and lower-severity findings are tracked separately in the Out-of-Scope section.

**Architecture:** An 8-task implementation campaign, executed by 2 devs + 1 reviewer. Tasks are grouped into 2 phases that respect shared-file constraints (F1 and F4 both touch the D-Bus interface and must run sequentially; F2 and F3 are fully independent). Each phase ends with a milestone where the user reviews progress before proceeding.

**Tech Stack:** Rust 2024 edition, tokio async (multi-thread), zbus 5, rusb, tiny-skia, tera, image, libc.

**Required Skills:**
- `forge:executing-plans` — invoke before Task 1 — drives the dev-pipeline + review/milestone cadence.
- `forge:writing-tests` — invoke before any TDD step in any task — every fix here has a regression test, not a shape check.
- `forge:verification-before-completion` — invoke before each Commit step. F1 + F4 touch behavior that's easy to break silently; tests prove the fix, hardware proves it doesn't regress.
- `forge:systematic-debugging` — invoke if any test fails unexpectedly during the campaign.

## Context for Executor

### Source review (the inputs to this plan)

The full review is in the conversation transcript that produced this plan. The four findings being addressed:

- **F1 — CRITICAL — Concurrent config writes corrupt `config.toml`.** `src/config.rs:219, 275, 335` build the temp file name as `format!("{}.tmp.{}", file_name, std::process::id())`. Inside one daemon process all three writers (`save_layout_vars`, `save_display_layout`, `save_background_image`) collide on the same temp path. `File::create` opens with `O_TRUNC`, so writer B truncates writer A's in-progress content. Compounding this, each writer performs an independent read-modify-write of `config.toml`, so a second writer's read before the first's rename loses the first's changes. Trigger: any concurrent GUI Apply that touches both background and layout vars.

- **F2 — MAJOR — `nvidia-smi` synchronous subprocess can freeze the tick loop.** `src/sensor/nvidia.rs:96` uses `std::process::Command::output()` with no timeout. `sensor_hub.poll()` is called inline in the tick loop with no `block_in_place` or `spawn_blocking` wrapper. Documented Linux failure: GPU enters D3 / driver hung → `nvidia-smi` blocks indefinitely → LCD freezes.

- **F3 — MAJOR — `SensorHistory::record` only prunes when a fresh value arrives.** `src/sensor/history.rs:42-54` nests the pruning loop inside `if let Some(val_str) = data.get(key) { if let Ok(val) = val_str.parse() { … } }`. If a sensor key disappears from the map (provider dropout, `nvidia-smi` returns `N/A`), the buffer keeps the last samples forever. The LCD graph shows ghost values for the rest of the session.

- **F4 — MAJOR — `set_background` race between concurrent calls.** `src/service/dbus.rs:471-502` releases the state lock before `spawn_blocking` decode, calls `apply_background_outside_lock` (disk write + channel send) outside any lock, then re-acquires the state lock to set `state.current_background`. Two concurrent invocations can interleave disk writes, channel sends, and in-memory commits arbitrarily. Final state can show disk = b / tick loop = a / `current_background` = b. Persistent divergence between on-disk, on-screen, and D-Bus views.

### Key Files (with line anchors)

- `src/config.rs:175-243` — `Config::save_layout_vars`. PID-only temp name at :219-223. `File::create` at :227.
- `src/config.rs:247-300` — `Config::save_display_layout`. PID-only temp name at :275-279. `File::create` at :283.
- `src/config.rs:304-360` — `Config::save_background_image`. PID-only temp name at :335-339. `File::create` at :343.
- `src/service/dbus.rs:33-58` — `ServiceState` struct. Add `config_write_lock` and `bg_change_lock` here.
- `src/service/dbus.rs:219-234` — `apply_background` (`&mut Config` variant). Called by `clear_background`.
- `src/service/dbus.rs:242-255` — `apply_background_outside_lock` (no state-mutex variant). Called by `set_background`.
- `src/service/dbus.rs:296-309` — `apply_layout_vars`. Calls `Config::save_layout_vars` inside the state lock.
- `src/service/dbus.rs:316-327` — `save_default_layout_impl`. Calls `Config::save_display_layout` outside the state lock.
- `src/service/dbus.rs:425-446` — `DisplayInterface::set_layout_vars`. Brief lock + apply + send.
- `src/service/dbus.rs:451-468` — `DisplayInterface::set_default_layout`. Brief lock + disk write + brief commit lock.
- `src/service/dbus.rs:471-503` — `DisplayInterface::set_background`. Brief lock + decode + disk write + channel send + brief commit lock. THE F4 SITE.
- `src/service/dbus.rs:506-513` — `DisplayInterface::clear_background`. Holds lock across disk write and channel send (a minor F4-adjacent issue; cleaned up in the same task).
- `src/sensor/nvidia.rs:89-108` — `NvidiaProvider::poll`. `Command::output()` at :90-99 is the F2 site.
- `src/sensor/history.rs:38-55` — `SensorHistory::record`. Pruning gated on parse at :47-51 is the F3 site.
- `src/service/tick.rs:79-209` — tick loop. F2 may add a wrapper around `sensor_hub.poll()` at :147.
- `src/main.rs:188-205` — `ServiceState` construction. Initialize `config_write_lock` and `bg_change_lock` here.

### Research Findings (verified during planning)

- **`tokio::sync::Mutex<()>` vs `std::sync::Mutex<()>` for `config_write_lock`** — D-Bus methods are async, so a tokio mutex is the right call. The inner save logic itself is synchronous; the mutex is acquired in async context, then the sync save runs inside the held guard. The guard is `Send` so it can be held across the (brief) sync section without `block_in_place`. **Do not** add `spawn_blocking` here — the read+modify+write costs ~1 ms for a kilobyte-sized TOML file, well under any tokio yield-budget concern.

- **Unique temp-file naming** — Three options weighed:
  - `tempfile::NamedTempFile::new_in(parent)` — canonical, but `tempfile` is currently dev-only (`Cargo.toml:75`). Promoting it to a regular dep adds ~6 transitive crates.
  - `fastrand::u64(..)` in the temp name — `fastrand` is transitively present (used by `tempfile`); adding it as a direct dep is one line, zero new transitive deps.
  - Process-local `AtomicU64` counter — zero new deps. `format!("{}.tmp.{}.{}", file_name, pid, COUNTER.fetch_add(1, Ordering::Relaxed))` is sufficient. With F1's mutex in place, only one writer runs at a time anyway, so the counter is belt-and-suspenders.

  **Recommended: `AtomicU64` counter.** Zero new deps, makes the test for collision-free behavior trivial. The mutex is the real fix; the counter is defense in depth.

- **`nvidia-smi` timeout strategy** — Three options:
  - `wait-timeout = "0.2"` crate — popular, ~1 KB code, gives `Child::wait_timeout(d) -> io::Result<Option<ExitStatus>>`. Kill the child on timeout. Pure-sync, fits the existing `SensorProvider::poll` signature without async refactor.
  - `tokio::process::Command` + `tokio::time::timeout` — requires making `SensorProvider::poll` async, which cascades through `SensorHub` and `tick.rs`. Larger surface.
  - Hand-rolled thread + channel + `Child::kill` — works but reinvents `wait-timeout`.

  **Recommended: `wait-timeout`.** Keeps `SensorProvider::poll` synchronous; minimal blast radius. Apply the same wrapper later to `mangohud` and any future shell-based provider as a follow-up MINOR (out of scope here).

- **`bg_change_lock` design** — The state Mutex was deliberately released across `spawn_blocking` decode in the first review pass. Reintroducing a hold across that boundary would regress D-Bus responsiveness. Instead, add a **second** mutex `bg_change_lock: Arc<tokio::sync::Mutex<()>>` whose ONLY job is to serialize set_background bodies end-to-end. Concurrent `set_background` calls queue on this mutex; D-Bus methods that don't write the background (set_layout, get_status, list_layouts, etc.) are unaffected and continue to use the state Mutex independently.

- **`SensorHistory` test pattern** — The existing `tests/sensor_history_tests.rs` covers happy paths but not dropout. F3's regression test simulates dropout by calling `record()` repeatedly with a metric present, then calling it with that metric absent, then asserting the buffer drops to empty after `max_duration`.

- **`tokio::sync::Mutex` guard across the sync save** — Holding a tokio mutex across a *short* sync call (no `.await` between lock and drop) is the explicit recommended pattern in the tokio docs. The guard is held for ~1 ms; this is fine.

### Relevant Patterns (existing in codebase)

- `src/config.rs:211-241` — atomic temp+rename pattern. Reuse verbatim; only the temp-name construction changes.
- `src/service/dbus.rs:425-446` — "brief lock, drop, do work, brief commit lock" pattern from `set_layout_vars`. Mirror this in `clear_background` (currently the odd one out — see Task 5).
- `src/sensor/history.rs:60-78` — `query` already handles empty buffers correctly. F3's fix only changes `record`; `query` semantics are unchanged.
- `tests/config_tests.rs:317-348` — `save_layout_vars_writes_atomically_via_same_dir_tempfile` — existing single-write atomicity test. The new concurrent-writer test should live next to it in the same file.

## Execution Architecture

**Team:** 2 devs, 1 reviewer.

**Task dependencies:**
- Task 1 (F2 nvidia timeout) — fully independent.
- Task 2 (F3 history pruning) — fully independent.
- Task 3 (review Tasks 1+2) — runs once both are in.
- Task 4 (Phase A milestone).
- Task 5 (F1 config write lock + unique temp name) — blocks Task 6 (F4 needs the same plumbing pattern).
- Task 6 (F4 bg_change_lock + clear_background symmetry) — sequential after Task 5.
- Task 7 (review Tasks 5+6).
- Task 8 (Phase B milestone — final integration check).

**Phases:**
- **Phase A (Tasks 1–4):** Independent quick wins — nvidia timeout + history pruning. Same review pass.
- **Phase B (Tasks 5–8):** Config write serialization — F1 first, F4 next (uses the same mutex pattern + adds `bg_change_lock`). Sequential. Same review pass after both.

**Milestones:**
- After Phase A: confirm nvidia driver hang no longer freezes the LCD; confirm sensor dropout decays history correctly.
- After Phase B: full smoke test on the real cooler — concurrent GUI applies, rapid set_background calls, daemon stop/start cycles.

---

## Task 1: nvidia-smi subprocess timeout [READ-DO]

**Files:**
- Modify: `Cargo.toml` (add `wait-timeout = "0.2"` to `[dependencies]`).
- Modify: `src/sensor/nvidia.rs:89-108`.
- Test: `tests/sensor_tests.rs` — add a unit test that runs against a synthetic slow-binary stand-in.

**Step 1: Invoke `forge:writing-tests`.**

The killer test asserts that `NvidiaProvider::poll` returns `Ok(Vec::new())` and bounded wall-clock when the underlying subprocess blocks past the timeout. Synthetic approach: temporarily set `$PATH` to a directory containing a `nvidia-smi` shim that runs `sleep 5`. Assert poll returns in < 1 second.

**Step 2: Write the failing test.**

Add to `tests/sensor_tests.rs`:

```rust
#[test]
fn nvidia_poll_times_out_on_hung_subprocess() {
    use std::time::Instant;
    use tempfile::TempDir;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let shim = dir.path().join("nvidia-smi");
    let mut f = std::fs::File::create(&shim).unwrap();
    writeln!(f, "#!/bin/sh\nsleep 5").unwrap();
    let mut perms = std::fs::metadata(&shim).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&shim, perms).unwrap();

    let original_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", dir.path().display(), original_path);

    // Safety: this test is single-threaded by virtue of the assertions below; the
    // env mutation is local to this process. If tests start running in parallel
    // and conflict, gate with a serial_test crate.
    unsafe { std::env::set_var("PATH", &new_path); }

    let mut provider = NvidiaProvider::new();
    let start = Instant::now();
    let result = provider.poll().unwrap();
    let elapsed = start.elapsed();

    unsafe { std::env::set_var("PATH", original_path); }

    assert!(elapsed < std::time::Duration::from_millis(1500),
        "poll took {:?}, expected < 1.5s", elapsed);
    assert!(result.is_empty(),
        "poll should return empty on timeout, got {:?}", result);
}
```

Run: `cargo test --features daemon --test sensor_tests nvidia_poll_times_out_on_hung_subprocess`
Expected: FAIL (current code blocks for 5s).

**Step 3: Implement the timeout.**

In `src/sensor/nvidia.rs:89-108`, replace `Command::output()` with `Command::spawn()` + `wait_timeout`:

```rust
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

const NVIDIA_SMI_TIMEOUT: Duration = Duration::from_millis(500);

fn poll(&mut self) -> Result<Vec<SensorReading>> {
    let mut child = match Command::new("nvidia-smi")
        .args([
            "--query-gpu=temperature.gpu,utilization.gpu,power.draw,memory.used,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return Ok(Vec::new()), // nvidia-smi not installed
    };

    match child.wait_timeout(NVIDIA_SMI_TIMEOUT) {
        Ok(Some(status)) if status.success() => {
            // Drain stdout — wait_timeout doesn't auto-collect.
            let mut buf = String::new();
            if let Some(mut out) = child.stdout.take() {
                use std::io::Read;
                let _ = out.read_to_string(&mut buf);
            }
            let line = buf.trim();
            if line.is_empty() { Ok(Vec::new()) } else { Ok(parse_csv_line(line)) }
        }
        Ok(Some(_)) => Ok(Vec::new()), // non-zero exit
        Ok(None) => {
            // Timed out — kill the child and warn once.
            let _ = child.kill();
            let _ = child.wait();
            log::warn!("nvidia-smi timed out after {:?} — GPU may be in deep sleep", NVIDIA_SMI_TIMEOUT);
            Ok(Vec::new())
        }
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            log::warn!("nvidia-smi wait failed: {}", e);
            Ok(Vec::new())
        }
    }
}
```

**Step 4: Run the test.** Should pass with elapsed ~500 ms.

**Step 5: Run the full sensor test suite.**

`cargo test --features daemon --test sensor_tests`

**Commit:**

```
git add Cargo.toml src/sensor/nvidia.rs tests/sensor_tests.rs
git commit -m "fix(sensor): time out nvidia-smi after 500ms to survive driver hangs"
```

---

## Task 2: SensorHistory prune unconditionally on record [READ-DO]

**Files:**
- Modify: `src/sensor/history.rs:38-55`.
- Test: `tests/sensor_history_tests.rs` — extend.

**Step 1: Invoke `forge:writing-tests`.**

The killer test simulates sensor dropout: configure a metric, record several values, then record an empty-map (or map without that key) and advance time past `max_duration`. Assert the buffer is empty.

Because the existing code uses `Instant::now()` directly, the test must use a `max_duration` short enough to advance with `std::thread::sleep(...)` — keep this under 200 ms for test speed. If `Instant` is mocked in another test, follow that convention; otherwise just sleep.

**Step 2: Write the failing test.**

Add to `tests/sensor_history_tests.rs`:

```rust
#[test]
fn record_prunes_stale_data_when_sensor_drops_out() {
    use std::collections::HashMap;
    use std::time::Duration;
    use thermalwriter::sensor::history::SensorHistory;

    let mut history = SensorHistory::new();
    history.configure_metric("gpu_power", Duration::from_millis(100));

    // Initial samples.
    let mut data = HashMap::new();
    data.insert("gpu_power".to_string(), "150".to_string());
    history.record(&data);
    history.record(&data);
    assert_eq!(history.query("gpu_power", 10).len(), 2);

    // Sensor drops out — caller no longer includes the key.
    std::thread::sleep(Duration::from_millis(150));
    let empty: HashMap<String, String> = HashMap::new();
    history.record(&empty);

    // Buffer should have been pruned even though the key was absent.
    assert!(
        history.query("gpu_power", 10).is_empty(),
        "buffer should be empty after sensor dropout + cutoff"
    );
}

#[test]
fn record_prunes_when_value_becomes_non_numeric() {
    use std::collections::HashMap;
    use std::time::Duration;
    use thermalwriter::sensor::history::SensorHistory;

    let mut history = SensorHistory::new();
    history.configure_metric("gpu_temp", Duration::from_millis(100));

    let mut data = HashMap::new();
    data.insert("gpu_temp".to_string(), "75".to_string());
    history.record(&data);

    std::thread::sleep(Duration::from_millis(150));
    data.insert("gpu_temp".to_string(), "N/A".to_string());
    history.record(&data);

    assert!(history.query("gpu_temp", 10).is_empty());
}
```

Run: `cargo test --features daemon --test sensor_history_tests record_prunes`
Expected: BOTH FAIL.

**Step 3: Fix `record`.**

In `src/sensor/history.rs:40-55`, restructure so pruning runs unconditionally for every configured metric:

```rust
pub fn record(&mut self, data: &HashMap<String, String>) {
    let now = Instant::now();
    for (key, config) in &self.configs {
        let buf = self.buffers.entry(key.clone()).or_insert_with(VecDeque::new);

        // Push fresh sample if available and numeric.
        if let Some(val_str) = data.get(key) {
            if let Ok(val) = val_str.parse::<f64>() {
                buf.push_back(Sample { time: now, value: val });
            }
        }

        // Prune unconditionally — covers both happy path and sensor dropout.
        let cutoff = now - config.max_duration;
        while buf.front().is_some_and(|s| s.time < cutoff) {
            buf.pop_front();
        }
    }
}
```

Note: this can't use `&self.configs` for iteration AND `&mut self.buffers` at the same time. Restructure with a borrow scope or collect keys first. One workable shape:

```rust
pub fn record(&mut self, data: &HashMap<String, String>) {
    let now = Instant::now();
    let cutoffs: Vec<(String, Instant)> = self.configs
        .iter()
        .map(|(k, c)| (k.clone(), now - c.max_duration))
        .collect();
    for (key, cutoff) in cutoffs {
        let buf = self.buffers.entry(key.clone()).or_insert_with(VecDeque::new);
        if let Some(val_str) = data.get(&key) {
            if let Ok(val) = val_str.parse::<f64>() {
                buf.push_back(Sample { time: now, value: val });
            }
        }
        while buf.front().is_some_and(|s| s.time < cutoff) {
            buf.pop_front();
        }
    }
}
```

Either shape is acceptable. The dev should pick the one that's most idiomatic given the surrounding code.

**Step 4: Run tests, observe pass.**

`cargo test --features daemon --test sensor_history_tests`

**Commit:**

```
git add src/sensor/history.rs tests/sensor_history_tests.rs
git commit -m "fix(sensor): prune SensorHistory on every record, not just on numeric updates

Previously the prune loop lived inside the parse-success branch, so a
sensor dropout (key absent from data) or non-numeric value (e.g.
nvidia-smi 'N/A') left the buffer frozen with stale samples for the
remainder of the session."
```

---

## Task 3: Review Tasks 1 + 2

**Trigger:** Both reviewers start when Tasks 1 and 2 are committed.

**Killer items (blocking):**
- [ ] `nvidia_poll_times_out_on_hung_subprocess` passes; elapsed < 1.5 s.
- [ ] `nvidia.rs` uses `wait_timeout`; the kill-on-timeout branch logs `nvidia-smi timed out` (verify by reading the impl, not just by running).
- [ ] `record_prunes_stale_data_when_sensor_drops_out` and `record_prunes_when_value_becomes_non_numeric` both pass.
- [ ] `SensorHistory::record` prune loop runs unconditionally — `grep -B 5 -A 10 'while buf.front' src/sensor/history.rs` shows the prune is NOT nested inside a parse-success guard.
- [ ] `cargo test --features daemon --workspace` is green.

**Quality items (non-blocking):**
- [ ] `NVIDIA_SMI_TIMEOUT` is a named constant with a comment explaining the 500 ms choice.
- [ ] Kill-on-timeout uses `let _ = child.kill(); let _ = child.wait();` so the zombie is reaped.
- [ ] History test uses `Duration::from_millis(100)` so the suite stays fast.

**Validation Data:**
- `time cargo test --features daemon --test sensor_tests nvidia_poll_times_out_on_hung_subprocess` — wall clock < 2 s.
- `cargo test --features daemon --test sensor_history_tests` — all pass.

**Resolution:** Killer findings block.

---

## Task 4: Milestone — Phase A complete

**Present to user:**
- Test counts: `cargo test --features daemon --workspace` totals.
- One-line summary of behavior change observable on hardware: "nvidia driver hang now logs and skips the tick instead of freezing the LCD; sensor dropouts no longer leave ghost values on history graphs."
- Hardware sanity check (optional): `systemctl --user restart thermalwriter`, run a workload that exits, watch a history-bearing layout (svg/neon-dash.svg) settle.

**Wait for user response. After approval, proceed to Phase B.**

---

## Task 5: Config write serialization + unique temp name [READ-DO]

**Files:**
- Modify: `src/config.rs:175-360` — the three `save_*` methods. Replace PID-only temp name with PID + atomic counter.
- Modify: `src/service/dbus.rs:33-58` — `ServiceState` gains `config_write_lock: Arc<tokio::sync::Mutex<()>>`.
- Modify: `src/service/dbus.rs:296-327` — `apply_layout_vars` and `save_default_layout_impl` take the lock before the sync save call. Update signatures.
- Modify: `src/service/dbus.rs:219-255` — `apply_background` and `apply_background_outside_lock` take the lock before the sync save call.
- Modify: `src/main.rs:188-205` — initialize `config_write_lock` in `ServiceState`.
- Test: `tests/config_tests.rs` — add a concurrent-writer regression test.

**Step 1: Invoke `forge:writing-tests`.**

The killer test spawns N threads, each calling a different `save_*` method concurrently against the same `config.toml`, then asserts:
1. The final file parses as valid TOML.
2. The final file contains contributions from at least one writer per family (proves none silently no-op'd).
3. No stray `.tmp.*` files remain in the directory.

The test should fail on the current PID-only temp name path AND on the missing mutex (file contents will be corrupted under load).

**Step 2: Write the failing test.**

Add to `tests/config_tests.rs`:

```rust
#[test]
fn concurrent_config_writes_do_not_corrupt_file() {
    use std::sync::Arc;
    use std::thread;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let path = Arc::new(dir.path().join("config.toml"));
    std::fs::write(&*path, "[display]\ntick_rate = 2\n").unwrap();

    let mut handles = Vec::new();
    for i in 0..16 {
        let path = Arc::clone(&path);
        handles.push(thread::spawn(move || {
            let mut vars = HashMap::new();
            vars.insert(format!("var_{}", i), format!("value_{}", i));
            // Alternate between the three writers to exercise all temp paths.
            match i % 3 {
                0 => Config::save_layout_vars(&path, &format!("layout_{}.svg", i), &vars).unwrap(),
                1 => Config::save_display_layout(&path, &format!("layout_{}.svg", i), "svg").unwrap(),
                _ => Config::save_background_image(&path, Some(&format!("bg_{}.png", i))).unwrap(),
            }
        }));
    }
    for h in handles { h.join().unwrap(); }

    // Final file must parse.
    let contents = std::fs::read_to_string(&*path).unwrap();
    let _: toml::Value = toml::from_str(&contents)
        .expect("config.toml must be valid TOML after concurrent writes");

    // No stray temp files.
    let stragglers: Vec<_> = std::fs::read_dir(dir.path()).unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
        .collect();
    assert!(stragglers.is_empty(),
        "expected no stray temp files, found {:?}",
        stragglers.iter().map(|e| e.file_name()).collect::<Vec<_>>());
}
```

Run: `cargo test --features daemon --test config_tests concurrent_config_writes_do_not_corrupt_file`
Expected: FAIL — file may have mixed content, parse may fail intermittently, or stragglers remain.

**Step 3: Add the atomic counter to `config.rs`.**

At top of `src/config.rs`:

```rust
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_tmp_suffix() -> u64 {
    TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
}
```

Replace the three temp-name constructions:

```rust
let tmp_name = format!(
    "{}.tmp.{}.{}",
    file_name.to_string_lossy(),
    std::process::id(),
    next_tmp_suffix(),
);
```

Run the new test. It MAY now pass because each writer uses a unique temp file. But the lost-update race remains — verify by reading the final file: it should contain ALL writers' contributions, not just the last to rename. If contributions are missing, proceed to Step 4 (the mutex is still required).

**Step 4: Add `config_write_lock` to `ServiceState`.**

In `src/service/dbus.rs:33-58`:

```rust
pub struct ServiceState {
    // ... existing fields ...
    /// Serializes all writes to config.toml so concurrent D-Bus calls don't lose
    /// each other's edits (each writer does a read-modify-write).
    pub config_write_lock: Arc<tokio::sync::Mutex<()>>,
}
```

Update `main.rs:188-205` construction:

```rust
let state = Arc::new(Mutex::new(ServiceState {
    // ... existing fields ...
    config_write_lock: Arc::new(tokio::sync::Mutex::new(())),
}));
```

**Step 5: Wire the lock into the four D-Bus methods that write config.**

The pattern: each method must acquire `config_write_lock` before calling `Config::save_*`, and may hold it across the brief sync save. Do NOT hold the state Mutex at the same time as `config_write_lock` — acquire ordering is `state` → `config_write_lock`, but in practice clone the lock handle out of state first, then drop the state guard, then take the write lock.

For `set_layout_vars` (dbus.rs:425-446):

```rust
async fn set_layout_vars(
    &self,
    name: String,
    vars: HashMap<String, String>,
) -> zbus::fdo::Result<()> {
    let (write_lock, tx, layout_dir, config_path) = {
        let state = self.state.lock().await;
        (
            state.config_write_lock.clone(),
            state.mode_change_tx.clone(),
            state.layout_dir.clone(),
            state.config_path.clone(),
        )
    };

    // Serialize config writes across all D-Bus methods.
    let _write_guard = write_lock.lock().await;
    validate_layout_path(&layout_dir, &name)?;
    Config::save_layout_vars(&config_path, &name, &vars)
        .map_err(|e| zbus::fdo::Error::Failed(format!("Failed to persist layout vars: {}", e)))?;

    // Update in-memory mirror under the state lock — separate brief acquisition.
    {
        let mut state = self.state.lock().await;
        state.config.layout_vars.insert(name.clone(), vars.clone());
    }
    drop(_write_guard);

    tx.send(ModeChange::Layout { name, vars }).await
        .map_err(|e| zbus::fdo::Error::Failed(format!("Failed to notify tick loop: {}", e)))?;
    Ok(())
}
```

Mirror this shape in `set_default_layout`, `clear_background`, and `set_background` (the F4 work in Task 6 elaborates on `set_background`).

The free-function helpers `apply_layout_vars` and `apply_background` no longer make sense as the call surface — they bundled state-mutex work with disk-write work. Either:
- Inline them into the D-Bus methods (~5-10 line bodies each), OR
- Refactor them to take `&tokio::sync::Mutex<()>` and acquire internally. Inlining is simpler.

The dev should choose inlining unless tests in `tests/dbus_tests.rs` depend on calling these helpers directly. If they do, keep the helpers and pass the lock through.

**Step 6: Run the concurrent-writer test, observe pass.**

`cargo test --features daemon --test config_tests concurrent_config_writes_do_not_corrupt_file`

Also re-run all existing dbus tests to confirm no regression:

`cargo test --features daemon --test dbus_tests`
`cargo test --features daemon --test config_tests`

**Commit:**

```
git add src/config.rs src/service/dbus.rs src/main.rs tests/config_tests.rs
git commit -m "fix(config): serialize config.toml writes via tokio mutex + unique temp suffix

- AtomicU64 counter in the temp-file name removes PID-only collision risk.
- New ServiceState::config_write_lock serializes all D-Bus methods that
  read-modify-write config.toml so concurrent GUI applies can't lose
  each other's edits.
- Inline the apply_layout_vars and apply_background helpers; the new
  lock-then-write pattern reads more clearly at the call site."
```

---

## Task 6: set_background bg_change_lock + clear_background symmetry [READ-DO]

**Files:**
- Modify: `src/service/dbus.rs:33-58` — `ServiceState` gains `bg_change_lock: Arc<tokio::sync::Mutex<()>>`.
- Modify: `src/service/dbus.rs:471-513` — `set_background` and `clear_background`.
- Modify: `src/main.rs:188-205` — initialize `bg_change_lock`.
- Test: `tests/dbus_tests.rs` — add a concurrent-set_background test (or, if the test harness can't easily spin up zbus, add a unit test of an extracted helper that mirrors the body).

**Step 1: Invoke `forge:writing-tests`.**

Spinning up zbus in a test is heavy. The easier surface to test is the underlying `apply_background` pattern — extract a helper that takes the lock and runs the sequence (decode is mocked or pre-computed), then drive two concurrent calls against it and assert the final state is one of the inputs and the disk + tick-channel + in-memory mirror agree.

If extracting is awkward, fall back to a manual hardware test in Task 7's review and document why.

**Step 2: Write the failing test (if feasible).**

Skeleton (adapt to existing test utilities):

```rust
#[tokio::test]
async fn concurrent_set_background_keeps_state_consistent() {
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let bg_dir = dir.path().join("backgrounds");
    let config_path = dir.path().join("config.toml");
    std::fs::create_dir_all(&bg_dir).unwrap();
    std::fs::write(&config_path, "[display]\ntick_rate = 2\n").unwrap();

    // Two distinct backgrounds (built-in seed bytes, decoded once).
    let bg_a_path = bg_dir.join("a.png");
    let bg_b_path = bg_dir.join("b.png");
    std::fs::write(&bg_a_path, thermalwriter::config::builtin_layouts::BG_DARK_SOLID).unwrap();
    std::fs::write(&bg_b_path, thermalwriter::config::builtin_layouts::BG_DARK_GRADIENT).unwrap();

    let bg_change_lock: Arc<Mutex<()>> = Arc::new(Mutex::new(()));
    let config_write_lock: Arc<Mutex<()>> = Arc::new(Mutex::new(()));
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);

    // Concurrently run two "set_background"-shaped futures.
    let l1 = bg_change_lock.clone();
    let w1 = config_write_lock.clone();
    let tx1 = tx.clone();
    let cfg1 = config_path.clone();
    let path_a = bg_a_path.clone();
    let h1 = tokio::spawn(async move {
        let _g = l1.lock().await;
        let pixmap = tokio::task::spawn_blocking(move || {
            thermalwriter::render::background::decode_from_file(&path_a)
        }).await.unwrap().unwrap();
        let _wg = w1.lock().await;
        thermalwriter::config::Config::save_background_image(&cfg1, Some("a.png")).unwrap();
        drop(_wg);
        tx1.send(("a", pixmap)).await.unwrap();
    });

    let l2 = bg_change_lock.clone();
    let w2 = config_write_lock.clone();
    let tx2 = tx;
    let cfg2 = config_path.clone();
    let path_b = bg_b_path.clone();
    let h2 = tokio::spawn(async move {
        let _g = l2.lock().await;
        let pixmap = tokio::task::spawn_blocking(move || {
            thermalwriter::render::background::decode_from_file(&path_b)
        }).await.unwrap().unwrap();
        let _wg = w2.lock().await;
        thermalwriter::config::Config::save_background_image(&cfg2, Some("b.png")).unwrap();
        drop(_wg);
        tx2.send(("b", pixmap)).await.unwrap();
    });

    h1.await.unwrap();
    h2.await.unwrap();

    // Drain the channel; the LAST send wins for the tick loop.
    let mut last_name: Option<&'static str> = None;
    while let Ok((name, _px)) = rx.try_recv() {
        last_name = Some(name);
    }

    // Disk side: read the config back.
    let final_contents = std::fs::read_to_string(&config_path).unwrap();
    let doc: toml::Value = toml::from_str(&final_contents).unwrap();
    let disk_name = doc.get("background").and_then(|b| b.get("image")).and_then(|i| i.as_str()).unwrap();

    // Invariant: disk and tick channel agree on the final selection.
    assert_eq!(disk_name, last_name.unwrap(),
        "disk says {:?}, channel last says {:?}", disk_name, last_name);
}
```

Run; expected to FAIL on the current `apply_background_outside_lock` ordering (which writes disk before sending). Even with the writes serialized by Task 5, the channel send is NOT serialized — so the order of disk renames and channel sends can diverge.

**Step 3: Add `bg_change_lock` to `ServiceState`.**

```rust
pub struct ServiceState {
    // ... existing fields, including config_write_lock from Task 5 ...
    /// Serializes set_background bodies end-to-end so concurrent calls don't
    /// interleave their decode → disk-write → channel-send → state-mirror
    /// commits.
    pub bg_change_lock: Arc<tokio::sync::Mutex<()>>,
}
```

Initialize in `main.rs`.

**Step 4: Refactor `set_background` to hold `bg_change_lock` end-to-end.**

```rust
async fn set_background(&self, name: String) -> zbus::fdo::Result<()> {
    let (bg_lock, write_lock, background_dir, config_path, tx) = {
        let state = self.state.lock().await;
        (
            state.bg_change_lock.clone(),
            state.config_write_lock.clone(),
            state.background_dir.clone(),
            state.config_path.clone(),
            state.mode_change_tx.clone(),
        )
    };

    // Serialize the entire set_background sequence so disk + channel + state
    // stay consistent under concurrent invocations.
    let _bg_guard = bg_lock.lock().await;

    let bg_path = validate_background_path(&background_dir, &name)?;

    let pixmap = tokio::task::spawn_blocking(move || {
        crate::render::background::decode_from_file(&bg_path)
    })
    .await
    .map_err(|e| zbus::fdo::Error::Failed(format!("decode task panicked: {}", e)))?
    .map_err(|e| zbus::fdo::Error::Failed(format!("Failed to decode background '{}': {}", name, e)))?;

    // Persist to disk under the config-write lock — bg_change_lock keeps other
    // set_background calls queued behind us.
    {
        let _w = write_lock.lock().await;
        Config::save_background_image(&config_path, Some(&name))
            .map_err(|e| zbus::fdo::Error::Failed(format!("Failed to persist background image: {}", e)))?;
    }

    // Notify the tick loop. Ordering here is fixed because bg_change_lock is held.
    tx.send(ModeChange::Background { image: Some(pixmap.clone()) })
        .await
        .map_err(|e| zbus::fdo::Error::Failed(format!("Failed to notify tick loop: {}", e)))?;

    // Commit in-memory state mirror.
    {
        let mut state = self.state.lock().await;
        state.current_background = Some(pixmap);
        state.config.background.image = Some(name);
    }
    Ok(())
}
```

**Step 5: Refactor `clear_background` to mirror the same pattern.**

```rust
async fn clear_background(&self) -> zbus::fdo::Result<()> {
    let (bg_lock, write_lock, config_path, tx) = {
        let state = self.state.lock().await;
        (
            state.bg_change_lock.clone(),
            state.config_write_lock.clone(),
            state.config_path.clone(),
            state.mode_change_tx.clone(),
        )
    };

    let _bg_guard = bg_lock.lock().await;

    {
        let _w = write_lock.lock().await;
        Config::save_background_image(&config_path, None)
            .map_err(|e| zbus::fdo::Error::Failed(format!("Failed to clear background: {}", e)))?;
    }

    tx.send(ModeChange::Background { image: None }).await
        .map_err(|e| zbus::fdo::Error::Failed(format!("Failed to notify tick loop: {}", e)))?;

    {
        let mut state = self.state.lock().await;
        state.current_background = None;
        state.config.background.image = None;
    }
    Ok(())
}
```

**Step 6: Run the concurrency test, observe pass.**

`cargo test --features daemon --test dbus_tests concurrent_set_background_keeps_state_consistent`

Also re-run full suite:

`cargo test --features daemon --workspace`

**Commit:**

```
git add src/service/dbus.rs src/main.rs tests/dbus_tests.rs
git commit -m "fix(dbus): serialize set_background bodies end-to-end with bg_change_lock

Adds a dedicated bg_change_lock that wraps each set_background/clear_background
body from validation through state-mirror commit. Concurrent invocations now
queue instead of interleaving disk writes, channel sends, and in-memory
commits, which previously could leave disk/tick-channel/state-mirror views
disagreeing about the active background.

Also brings clear_background onto the same brief-state-lock + clone-out
pattern as set_background (the first review pass fixed set_background but
not its counterpart)."
```

---

## Task 7: Review Tasks 5 + 6

**Trigger:** Both reviewers start when Tasks 5 and 6 are committed.

**Killer items (blocking):**
- [ ] `concurrent_config_writes_do_not_corrupt_file` passes; no stragglers; resulting TOML parses.
- [ ] `concurrent_set_background_keeps_state_consistent` passes (or, if the test was skipped due to harness limits, a documented manual hardware reproduction exists and was run).
- [ ] `ServiceState` has both `config_write_lock` and `bg_change_lock`; both are `Arc<tokio::sync::Mutex<()>>` (not `std::sync::Mutex`).
- [ ] The state Mutex and the new locks are NEVER both held simultaneously: `grep -B 3 -A 20 'config_write_lock.lock' src/service/dbus.rs` confirms each call site clones the handles out of the state lock first.
- [ ] Temp file names include the atomic-counter suffix — `grep -n 'tmp_name = format' src/config.rs` shows three sites, all with the counter.
- [ ] `set_background` and `clear_background` use the same brief-state-lock + clone-out + heavy-work + brief-commit-lock shape.
- [ ] `cargo test --features daemon --workspace` is green.

**Quality items (non-blocking):**
- [ ] The `apply_background`/`apply_background_outside_lock` helpers are either deleted or kept-but-locked; no caller bypasses the new locks.
- [ ] `bg_change_lock` is documented with the rationale ("serialize end-to-end so disk + channel + state agree").
- [ ] No new clippy warnings (`cargo clippy --features daemon --workspace -- -D warnings`).

**Validation Data:**
- Run the concurrent-writes test in a loop to catch flakiness: `for i in {1..20}; do cargo test --features daemon --test config_tests concurrent_config_writes_do_not_corrupt_file --release || break; done`. All 20 iterations must pass.
- Manual hardware sequence: with the daemon running, fire two rapid `busctl --user call com.thermalwriter.Service /com/thermalwriter/display com.thermalwriter.Display SetBackground s "..."` calls back-to-back; verify the LCD shows the last one, disk reflects the last one, `get_status` reflects the last one.

**Resolution:** Killer findings block. Quality items queue.

---

## Task 8: Milestone — Phase B complete

**Present to user:**
- Full `cargo test --features daemon --workspace` green.
- Concurrent-writer stress test (Task 7 validation) — 20/20 green.
- Manual end-to-end sweep on the real cooler:
  - Daemon starts cleanly, LCD renders.
  - Trigger nvidia driver hang (e.g., suspend GPU, run a CUDA stress, or `sudo systemctl restart nvidia-persistenced` while watching) — LCD continues updating with cpu metrics; journalctl shows `nvidia-smi timed out`.
  - Unplug a hwmon-reported fan / disconnect MangoHud — history-bearing layout graph drops to empty within `max_duration` of the configured retention.
  - Fire two rapid set_background calls — final state on disk, on screen, and via `get_status` all agree.
  - Stop daemon via `systemctl --user stop thermalwriter` — clean exit (< 1s).

**Wait for user response. After approval, the campaign is complete.**

---

## Out of Scope / Deferred

The following findings from the same review pass are intentionally NOT in this campaign:

- **F5 — Xvfb mmap missing bounds check** (MAJOR-leaning). Practical risk is low because Xvfb mmaps the screen file as its own backing store with full size from the start. A defensive `mmap.len() >= pixel_data_offset + height * bytes_per_line` check is worth adding but doesn't warrant a dedicated phase. Track as a follow-up MINOR.
- **F6 — Tick loop has no unconditional yield** (MINOR). Only matters under sustained over-budget ticks (slow CPU at 60 FPS xvfb). At the current 2 FPS default, the existing `sleep` is hit every iteration. Trivial fix (`tokio::task::yield_now().await` at loop bottom) — fold into the next tick-loop change.
- **F7 — `set_layout` holds state lock across channel send** (MINOR). Real but rarely hit with buffer=4 and a fast listener. Fold into the next dbus refactor.
- **F8 — `kill_process_group` ESRCH-then-SIGKILL** (NITPICK). PID-reuse window is sub-millisecond and the leader stays zombie during the loop, so the failure path is unreachable in practice. One-line hygiene fix when the file is next touched.
- **F9 — `device_connected`/`device_disconnected`/`error` signals declared but never emitted** (MINOR). Clients can poll the `Connected` property. Wire signals up OR remove the declarations — defer to a deliberate D-Bus interface review.
- **F10 — USB ZLP hardcoded to 512** (NITPICK). Cooler is USB 2.0 High-Speed; the assumption holds. Revisit only if a different model with Full-Speed bulk endpoints ships.
- **MangoHud / hwmon subprocess timeouts** — same pattern as F2 but lower-risk (no documented hang scenario). Apply `wait-timeout` to any future shell-based provider; existing hwmon reads are filesystem-only and don't need this.
