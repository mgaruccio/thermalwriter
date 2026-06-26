# Adversarial Review Fixes — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use `forge:executing-plans` to implement this plan task-by-task.

**Goal:** Resolve all confirmed findings from the 2026-05-10 adversarial review (6 internal reviewers + codex + gemini). Verified false positives are *not* in scope.

**Architecture:** A 13-task implementation campaign, executed by a 3-dev / 2-reviewer pipeline. Tasks are grouped into 7 phases that respect shared-file constraints (USB, D-Bus, main.rs lifecycle). Each phase ends with a milestone where the user reviews progress before proceeding.

**Tech Stack:** Rust 2024 edition, tokio async, zbus 5, rusb, resvg/usvg + tiny-skia, tera, image, Tauri 2 + Svelte 5.

**Required Skills:**
- `forge:executing-plans` — invoke before Task 1 — drives the dev-pipeline + review/milestone cadence.
- `forge:writing-tests` — invoke before any TDD step in any task — the killer-items checklists assume real assertions, not shape checks.
- `forge:verification-before-completion` — invoke before each "Commit" step in tasks that touch the daemon hot path (Tasks 5, 6, 7, 9) — hardware-affecting changes need a real-device smoke test, not just `cargo test`.
- `forge:systematic-debugging` — invoke if a test fails unexpectedly during any task — do not silently rewrite the test to pass.

## Context for Executor

### Source review (the inputs to this plan)
The full review with cross-reviewer consensus counts is in the conversation transcript that produced this plan. The CRITICAL/MAJOR findings being addressed are:
- **#1** USB partial writes silently truncated — `src/transport/bulk_usb.rs:176-179`
- **#2** No USB reconnect / hot-replug recovery — `src/main.rs:86-87`, `src/service/tick.rs:156-158`
- **#3** Blocking I/O on async runtime: nvidia-smi `src/sensor/nvidia.rs:24-32`, fontdb reload `src/render/svg.rs:45`, USB sync send `src/transport/bulk_usb.rs:177`
- **#4** Image decode unbounded — `src/render/background.rs:24-27`
- **#5** Xvfb mode hardcodes `initial_sensor_history = None` — `src/main.rs:131`
- **#6** Tauri `devtools` feature unconditional in release — `gui/src-tauri/Cargo.toml:16`
- **#7** No SIGTERM/SIGINT handler — `src/main.rs:309-326`
- **#8** Xvfb child not in process group — `src/service/xvfb.rs:110-116`, `:31-44`
- **#9** State mutex held across heavy I/O in D-Bus methods — `src/service/dbus.rs::set_background` (line 409), `set_layout_vars` (line 385)
- **#10** GUI and daemon both rewrite `config.toml` — `gui/src-tauri/src/commands.rs::save_config` (line 187), `Config::save_*` in `src/config.rs`
- **#11** RAPL counter rollover fallback to `u64::MAX` — `src/sensor/rapl.rs:92`
- **#12** GUI's `apply_to_daemon` calls both `set_layout_vars` and `set_layout` — `gui/src-tauri/src/commands.rs:209-240`
- **#13** Brittle nvidia-smi / mangohud parsers — `src/sensor/nvidia.rs:35-90`, `src/sensor/mangohud.rs:62-90`
- **#14** Tick loop drains only one `source_rx` per tick — `src/service/tick.rs:118`
- Plus MINOR sweep: config validation, `tick_rate` setter dead-end, error message path hygiene, hwmon log dedup, sensor cache invalidation on layout switch.

### Key Files (with line anchors)
- `src/transport/bulk_usb.rs:69-119` — `BulkUsb::new` (open + claim + endpoint discovery).
- `src/transport/bulk_usb.rs:162-190` — `send_frame` (the chunked write loop that ignores partial writes).
- `src/transport/mod.rs` — `Transport` trait surface (only 26 lines; extend here for `is_connected()` if needed).
- `src/service/tick.rs:79-180` — the frame loop. Where reconnect logic, single-message-drain fix, sensor cache invalidation will live.
- `src/service/tick.rs:118` — `if let Ok(new_source) = source_rx.try_recv()` — change to `while let`.
- `src/service/tick.rs:156-158` — `warn!("Failed to send frame: {}", e)` is the silent-failure site for #2.
- `src/main.rs:31-46` — main entry; subcommands branch.
- `src/main.rs:86-89` — single-shot `BulkUsb::new()` + `handshake()`.
- `src/main.rs:131` — the xvfb branch hardcoding `initial_sensor_history = None`.
- `src/main.rs:213` — `reload_history = initial_sensor_history.clone()` captured into the spawn closure; if None at startup, layout switches lose history.
- `src/main.rs:214-303` — the unsupervised `tokio::spawn` mode-change listener.
- `src/main.rs:309-326` — `tick::run_tick_loop(...).await?` — currently no `select!` with signals.
- `src/render/svg.rs:38-76` — `SvgRenderer::new` — fontdb load happens here every call.
- `src/render/svg.rs:44-49` — `options.fontdb_mut().load_font_data(...)` + `load_system_fonts()` + `set_monospace_family(...)`. The shared cache must reproduce all three.
- `src/render/background.rs:24-43` — `decode_to_pixmap`. Image-decode bounds added here.
- `src/sensor/rapl.rs:48-53` — `read_max_energy_uj` — read-once-and-cache target.
- `src/sensor/rapl.rs:88-94` — the rollover branch using `u64::MAX` fallback.
- `src/sensor/nvidia.rs:21-32` — `Command::new("nvidia-smi").output()` — wrap in spawn_blocking + timeout.
- `src/sensor/mangohud.rs:62-90` — 4 KB tail seek, no newline-boundary scan.
- `src/service/xvfb.rs:31-44` — `Drop` kills only direct children.
- `src/service/xvfb.rs:110-116` — `sh -c command` spawn site.
- `src/service/dbus.rs:289-510` — `#[interface]` impl. All D-Bus method bodies; lock-scope cleanup target.
- `src/service/dbus.rs:409-421` — `set_background` decodes + holds lock.
- `src/service/dbus.rs:478-492` — `set_tick_rate` setter that doesn't propagate.
- `src/config.rs:106-205` — `Config::load`, `save_layout_vars` (atomic temp+rename, no flock).
- `gui/src-tauri/Cargo.toml:16` — the Tauri feature line.
- `gui/src-tauri/src/commands.rs:186-206` — `save_config` writes layout_vars + display_layout (both directly to disk).
- `gui/src-tauri/src/commands.rs:208-240` — `apply_to_daemon` makes the redundant double D-Bus call.
- `gui/src-tauri/tauri.conf.json:25` — current CSP; we can tighten `script-src` here for hygiene.

### Research Findings (verified during planning)
- **Tauri devtools feature** — In Tauri 2, the `devtools` cargo feature ships the inspector unconditionally. The right gate is moving it into `[target.'cfg(debug_assertions)'.dependencies]` or a custom feature. Confirmed via Tauri 2 source.
- **`autoescape_on(vec![])`** — Tera autoescaping is *off* in `svg.rs:52`. Sensor strings flow into Tera context unescaped. Tera with `add_raw_template` has no filesystem loader, so `{% include %}` cannot read disk — that part of the dbus reviewer's finding was a false positive.
- **`Tera::add_raw_template`** vs `add_template_file` — we use the former, so SSTI-via-include is structurally not possible. We still need to escape sensor strings in case a value carries `<`, `&` etc., but the threat model is "malformed value breaks the SVG render," not "remote-code-exec via template engine."
- **`tiny_skia::Pixmap::data()`** — invariant returns exactly `width*height*4` bytes. The `chunks(4)` vs `chunks_exact(4)` finding in the render reviewer is cosmetic, not a real bug. Skip.
- **Edition 2024** — Rust 1.85 stabilized edition 2024. The concurrency reviewer's "edition 2024 invalid" finding is a false positive. Skip.
- **CSP `script-src`** — `default-src 'self'` *does* cover `script-src` when `script-src` is absent. The GUI reviewer's claim that "any inline script is permitted" is wrong. We'll add an explicit `script-src 'self'` for hygiene but it's not blocking.
- **`image` crate decoder limits** — `image::io::Reader::with_guessed_format()` followed by `.limits(image::Limits { max_image_width: Some(N), max_image_height: Some(N), max_alloc: Some(N), ..Default::default() })` is the supported bounded path in image 0.25. Use this instead of `image::load_from_memory` directly.
- **`tokio::signal::unix::signal(SignalKind::terminate())`** — the standard way to await SIGTERM in tokio; pair with `tokio::signal::ctrl_c()` via `tokio::select!` for SIGINT.
- **`std::os::unix::process::CommandExt::process_group`** — sets the child to a new process group at fork; then `nix::sys::signal::kill(Pid::from_raw(-pid), Signal::SIGTERM)` on Drop kills the whole tree. The repo already pulls `libc 0.2`, so add `nix` (or use `libc::killpg` directly).
- **Lock-across-await in D-Bus methods** — `tokio::sync::Mutex` is technically safe across await, but holding it during `image::load_from_memory` + `Lanczos3 resize` (~50–200 ms) blocks every other D-Bus call. The fix is the standard pattern: clone needed paths/handles out of the lock, drop the guard, do the work, then re-acquire only to commit.

### Relevant Patterns (existing in codebase)
- `src/config.rs:172-203` — Atomic temp-file rename pattern. Reuse this for any new file-write code.
- `src/service/dbus.rs:78-112` — `validate_path_within_dir` — canonical-path traversal guard. Reuse for any new D-Bus method that accepts a filename.
- `src/service/tick.rs:111-122` — `watch::Receiver::has_changed` + `borrow_and_update` pattern for `background_rx`. Apply the same shape for any new watch channel (e.g. `tick_rate_rx` in Task 12).
- `src/service/dbus.rs:412-419` — `set_background` already canonicalizes via `validate_background_path` before calling `decode_from_file`. Keep this when relocating decode work to spawn_blocking.

## Execution Architecture

**Team:** 3 devs, 1 spec reviewer, 1 quality reviewer.
**Task dependencies:**
- Task 1 (image bounds) — independent.
- Task 2 (fontdb shared cache) — independent of 1 but shares `src/render/` mental model.
- Task 3 (xvfb process group) — independent.
- Task 4 (RAPL rollover) + Task 5 (sensor parsers) — independent of 1-3 and of each other; same reviewer pass.
- Task 6 (USB partial-write loop) blocks Task 7 (USB reconnect state) — same module, sequential.
- Task 7 blocks Task 8 (USB send via `spawn_blocking` / async wrapper) — Task 8 needs the new connection-state to know when to bail vs retry.
- Task 9 (SIGTERM handler + xvfb history init) is independent but touches `src/main.rs`; sequence after Tasks 6-8 to avoid merge churn in `main.rs`.
- Task 10 (D-Bus lock-scope cleanup) and Task 11 (single config writer) both touch `src/service/dbus.rs` — coordinate, then run sequentially.
- Task 12 (Tauri devtools gate + remove redundant set_layout call) is independent.
- Task 13 (tick loop polish + config validation) — `src/service/tick.rs` and `src/config.rs`. Independent of all but Task 8 (which also touches tick.rs); sequence after Task 8.

**Phases:**
- **Phase 1 (Tasks 1–3):** Independent quick wins — image bounds, fontdb cache, xvfb process group.
- **Phase 2 (Tasks 4–5):** Sensor accuracy — RAPL + parsers.
- **Phase 3 (Tasks 6–8):** USB resilience — partial writes, reconnect state, async wrapper.
- **Phase 4 (Task 9):** Lifecycle — signals + xvfb history parity.
- **Phase 5 (Tasks 10–11):** D-Bus refactor — lock scope, single config writer.
- **Phase 6 (Task 12):** GUI hygiene.
- **Phase 7 (Task 13):** Tick loop polish + config validation MINOR sweep.

**Milestones:**
- After Phase 1 (before Task 4): low-risk fixes confirmed working on hardware.
- After Phase 3 (before Task 9): biggest behavior change — verify a deliberate USB unplug+replug recovers cleanly.
- After Phase 5 (before Task 12): D-Bus interface stability check.
- After Phase 7 (final): full smoke test on the real cooler.

---

## Task 1: Image decode bounds [DO-CONFIRM]

**Files:**
- Modify: `src/render/background.rs:24-43`
- Test: `tests/render_tests.rs` (extend) — there is no existing `tests/background_tests.rs`; tests should live alongside the existing render tests.

**Implement:** Replace the unbounded `image::load_from_memory(bytes)` call with a bounded `image::io::Reader` chain. Reject files > 8 MB on disk before decode. Cap decoded dimensions via `image::Limits` so a decompression bomb errors out before allocation. Errors should propagate as `anyhow::Error` with a short message that does not leak the source filename (the caller already includes the path in its outer context).

Suggested limit values: `max_image_width: Some(8192), max_image_height: Some(8192), max_alloc: Some(256 * 1024 * 1024)`. These are well above any realistic background while still preventing OOM.

For `decode_from_file`, also add an early `std::fs::metadata(path)?.len() > 8 * 1024 * 1024` reject before reading. Pre-size message: `"background file too large: {} bytes (max 8 MB)"`.

**Confirm checklist (killer items):**
- [ ] Failing test written FIRST: a test that constructs a multi-megabyte fake-image byte vector and asserts decode returns `Err`, plus a test that a normal 480x480 PNG decodes fine.
- [ ] `image::io::Reader::with_guessed_format()` is used (not `load_from_memory`) so `Limits` actually apply.
- [ ] File-size pre-check is in `decode_from_file`, not buried inside `decode_to_pixmap` — both paths must reject oversized inputs.
- [ ] Error message does NOT include the filename (caller's `with_context` adds it).
- [ ] Existing tests in `tests/render_tests.rs` still pass — no regression on legitimate backgrounds.
- [ ] Premultiply-alpha logic in `:32-37` is preserved verbatim.
- [ ] Committed with message like `fix(render): bound background image decode to prevent decompression bombs`.

---

## Task 2: Review Task 1

**Trigger:** Both reviewers start when Task 1 completes.

**Killer items (blocking):**
- [ ] `decode_to_pixmap` rejects a 100 MB synthetic PNG via the `Limits` path (not via OOM).
- [ ] `decode_from_file` rejects files where `metadata.len() > 8 * 1024 * 1024` BEFORE reading them — verify by stat-ing a sparse file.
- [ ] Existing seeded backgrounds (`~/.config/thermalwriter/backgrounds/*`) still decode — run `cargo test` and check `bg_gallery_returns_seeded_files` style tests pass.
- [ ] Premultiply loop at `background.rs:32-37` is byte-identical to before.
- [ ] No `unwrap()` introduced; all error paths return `Err(anyhow::Error)`.

**Quality items (non-blocking):**
- [ ] Limit constants are named (e.g. `MAX_BG_FILE_BYTES`, `MAX_DECODED_DIMENSION`) and documented.
- [ ] `MAX_BG_FILE_BYTES` is at least 4× the largest seeded background.

**Validation Data:**
- Run `cargo test --test render_tests` — must pass.
- Manually run `cargo run --example preview_layout layouts/svg/neon-dash-v2.svg` — must produce a frame.

**Resolution:** Killer findings block. Quality items queue.

---

## Task 3: Fontdb shared cache [READ-DO]

**Files:**
- Modify: `src/render/svg.rs:38-76` (constructor) and `src/render/svg.rs:1-36` (top-of-file additions).
- Test: `tests/render_tests.rs` — extend with a "second SvgRenderer is faster than first" assertion via `Instant::now()`.

**Step 1: Invoke `forge:writing-tests` skill**

Before writing the timing-based test, read the test conventions in `tests/render_tests.rs`. The skill will guide you on what to assert (an assertion on time-difference is flaky; assert that the system fontdb has at most one entry for our embedded family across N renderer constructions, since duplicate `load_font_data` would double-register).

**Step 2: Write the failing test**

Add this to `tests/render_tests.rs`:

```rust
#[test]
fn fontdb_is_loaded_once_across_multiple_renderers() {
    // The shared fontdb cache should mean that constructing N SvgRenderers
    // does not reload system fonts N times. We approximate this by timing:
    // first construction primes the cache, second is much faster.
    let svg = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"480\" height=\"480\"></svg>";

    let t0 = std::time::Instant::now();
    let _r1 = thermalwriter::render::svg::SvgRenderer::new(svg, 480, 480).unwrap();
    let first = t0.elapsed();

    let t1 = std::time::Instant::now();
    let _r2 = thermalwriter::render::svg::SvgRenderer::new(svg, 480, 480).unwrap();
    let second = t1.elapsed();

    // System-font scan dominates first call; second should be at least 4x faster.
    assert!(
        second * 4 < first,
        "second renderer construction was not noticeably faster: first={:?}, second={:?}",
        first,
        second
    );
}
```

**Step 3: Run test, observe failure**

Run: `cargo test --test render_tests fontdb_is_loaded_once_across_multiple_renderers -- --nocapture`
Expected: FAIL — second is roughly the same speed as first because `load_system_fonts()` runs both times.

**Step 4: Build the shared fontdb**

In `src/render/svg.rs`, near the top add:

```rust
use std::sync::OnceLock;
use resvg::usvg::fontdb::Database;

static SHARED_FONTDB: OnceLock<Database> = OnceLock::new();

fn shared_fontdb() -> &'static Database {
    SHARED_FONTDB.get_or_init(|| {
        let mut db = Database::new();
        db.load_font_data(EMBEDDED_FONT.to_vec());
        db.load_system_fonts();
        db.set_monospace_family(EMBEDDED_FONT_FAMILY);
        db
    })
}
```

In `SvgRenderer::new`, replace the three `options.fontdb_mut()` calls (lines 44-49) with:

```rust
let mut options = usvg::Options::default();
options.font_family = EMBEDDED_FONT_FAMILY.to_string();
// Replace the per-construction fontdb with the shared, lazy-initialized one.
// usvg::Options uses an Arc<Database> internally so cloning is cheap.
options.fontdb = std::sync::Arc::new(shared_fontdb().clone());
```

The `clone()` on the Arc-wrapped DB is cheap (refcount bump). If `usvg::Options::fontdb` is not directly assignable in your crate version, use `options.fontdb = shared_fontdb().clone().into()` or replace via `options.fontdb_mut() = ...`. Check `cargo doc --open` for the exact field name.

**Step 5: Run test, observe pass**

Run: `cargo test --test render_tests fontdb_is_loaded_once_across_multiple_renderers -- --nocapture`
Expected: PASS — second construction is dramatically faster.

**Step 6: Run the full render test suite**

Run: `cargo test --test render_tests`
Expected: All tests pass. If any assertion about font rendering changes, investigate before "fixing" it.

**Step 7: Sanity-check on hardware**

```bash
systemctl --user stop thermalwriter
cargo run --example render_layout layouts/svg/neon-dash-v2.svg 5
systemctl --user start thermalwriter
```

Watch the LCD for any glyph regression (missing characters, fallback fonts). The shared db must contain the embedded JetBrainsMono/DejaVu-Sans-Mono variant, exactly as before.

**Step 8: Commit**

```bash
git add src/render/svg.rs tests/render_tests.rs
git commit -m "perf(render): cache fontdb across SvgRenderer instances

Move font loading to a OnceLock-backed shared Database so layout
switches no longer re-scan /usr/share/fonts/. Eliminates the
multi-hundred-millisecond latency spike that previously blocked the
async runtime on every D-Bus set_layout call."
```

---

## Task 4: Xvfb process group cleanup [DO-CONFIRM]

**Files:**
- Modify: `src/service/xvfb.rs:31-44` (Drop) and `:108-119` (spawn).
- Modify: `Cargo.toml` to add `nix = { version = "0.29", features = ["signal"] }` *or* use raw `libc::killpg` (already pulled). Either is fine; choose `libc` to avoid a new dep.
- Test: extend `src/service/xvfb.rs::tests` (the existing `#[cfg(test)]` module at the bottom).

**Implement:** Use `std::os::unix::process::CommandExt::process_group(0)` on the `Command::new("sh")` builder so the child shell starts in its own process group. On `Drop`, instead of `child.kill()` (which only signals the direct child), call `libc::killpg(child.id() as i32, libc::SIGTERM)` then a short `wait_timeout` then `libc::killpg(..., libc::SIGKILL)` on remaining processes. Same for the Xvfb child.

Pattern to follow:

```rust
use std::os::unix::process::CommandExt;
// ...
let child_process = Command::new("sh")
    .args(["-c", command])
    .env("DISPLAY", &display)
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .process_group(0)  // new pgid = child pid
    .spawn()
    .with_context(...)?;
```

```rust
fn kill_process_group(pid: u32) {
    unsafe {
        libc::killpg(pid as i32, libc::SIGTERM);
    }
    // Optional: short wait, then SIGKILL stragglers.
}
```

**Confirm checklist (killer items):**
- [ ] Failing test written FIRST: spawn `sh -c 'sleep 30 & sleep 30'` in a `start()` call (or use a fixture that fork-detaches), drop the handle, assert no leftover `sleep` processes belong to the spawning daemon's pgid. Use `pgrep -g <pid>` via `Command::new("pgrep")`.
- [ ] `process_group(0)` is set on BOTH the Xvfb child AND the user command child.
- [ ] `Drop` impl uses `killpg` not `kill` for both processes.
- [ ] Drop still removes `fbdir` (`std::fs::remove_dir_all`) — that line at `:42` must remain.
- [ ] `find_unused_display` test (line 128-135) still passes.
- [ ] Manual smoke test: run `thermalwriter ctl mirror "xclock"`, verify xclock appears, then `thermalwriter ctl set-mode svg svg/neon-dash-v2.svg`, verify the `xclock` process is gone (`pgrep xclock` returns nothing).
- [ ] Committed with message like `fix(xvfb): kill the entire process group on handle drop`.

---

## Task 5: RAPL rollover + sensor parser hardening [READ-DO]

**Files:**
- Modify: `src/sensor/rapl.rs:14-54` (cache `max_energy_uj` at construction) and `:88-94` (rollover branch).
- Modify: `src/sensor/nvidia.rs:35-90` (skip rows with `N/A` and log once).
- Modify: `src/sensor/mangohud.rs:62-90` (find first `\n` after seek before parsing).
- Test: extend `tests/sensor_tests.rs`.

**Step 1: Invoke `forge:writing-tests` skill**

These are sensor code paths with heavy environmental dependencies. The skill's "test the meaning, not the shape" rule is critical: don't just assert "function returns Ok"; assert that a synthetic `energy_uj` rollover produces a *plausible* watts value, not 1.8 × 10^13.

**Step 2: Write the failing RAPL test**

Add to `tests/sensor_tests.rs`:

```rust
#[test]
fn rapl_rollover_with_unreadable_max_does_not_explode() {
    // Synthesize a RAPL provider whose base_path points to a tempdir where
    // energy_uj rolls over but max_energy_range_uj is missing. Assert the
    // computed wattage is either absent (no reading) or within sane bounds
    // (< 10kW), NOT 1.8e13 watts.
    use std::fs;
    let tmp = tempfile::tempdir().unwrap();
    let rapl_dir = tmp.path().join("intel-rapl:0");
    fs::create_dir_all(&rapl_dir).unwrap();

    let energy_path = rapl_dir.join("energy_uj");
    // Tick 1: large prev value
    fs::write(&energy_path, "1000000000000").unwrap();

    let mut provider = thermalwriter::sensor::rapl::RaplProvider::with_base_path(
        tmp.path().to_path_buf()
    );
    let _ = provider.poll(); // primes prev_energy

    // Tick 2: smaller value (rollover) — but max_energy_range_uj does NOT exist
    fs::write(&energy_path, "100").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(60));

    let readings = provider.poll().unwrap();
    if let Some(power) = readings.iter().find(|r| r.key == "cpu_power") {
        let watts: f64 = power.value.parse().expect("watts must parse as number");
        assert!(
            watts.is_finite() && watts >= 0.0 && watts < 10_000.0,
            "rollover with missing max produced insane wattage: {} W",
            watts
        );
    }
    // It's also acceptable for the reading to be absent — both are sane.
}
```

**Step 3: Run test, observe failure**

Run: `cargo test --test sensor_tests rapl_rollover_with_unreadable_max_does_not_explode`
Expected: FAIL — current code returns ~1.8e13 watts.

**Step 4: Fix RAPL**

Refactor `RaplProvider` to cache `max_energy_uj` at construction:

```rust
pub struct RaplProvider {
    base_path: PathBuf,
    max_energy_uj: Option<u64>,  // cached at startup
    last_energy_uj: Option<u64>,
    last_poll: Option<Instant>,
    access_warned: bool,
}

impl RaplProvider {
    pub fn new() -> Self {
        let base_path = PathBuf::from(DEFAULT_POWERCAP_PATH);
        let max_energy_uj = Self::read_max_at(&base_path);
        Self {
            base_path,
            max_energy_uj,
            last_energy_uj: None,
            last_poll: None,
            access_warned: false,
        }
    }

    pub fn with_base_path(base: PathBuf) -> Self {
        let max_energy_uj = Self::read_max_at(&base);
        Self { base_path: base, max_energy_uj, last_energy_uj: None, last_poll: None, access_warned: false }
    }

    fn read_max_at(base: &Path) -> Option<u64> {
        let path = base.join("intel-rapl:0/max_energy_range_uj");
        fs::read_to_string(path).ok().and_then(|s| s.trim().parse().ok())
    }
    // ...
}
```

In the rollover branch (`:88-94`), replace `unwrap_or(u64::MAX)` with: if `self.max_energy_uj` is `None`, skip emitting a reading for this tick (return `Ok(readings)` with no `cpu_power` entry).

**Step 5: Run RAPL test, observe pass**

Run: `cargo test --test sensor_tests rapl_rollover_with_unreadable_max_does_not_explode`
Expected: PASS.

**Step 6: Write the failing nvidia-smi N/A test**

Add to `tests/sensor_tests.rs`:

```rust
#[test]
fn nvidia_parser_skips_na_fields_without_emitting_nan() {
    // Simulate nvidia-smi output where power.draw is "N/A" (driver hung).
    // The parser should NOT emit a gpu_power reading with "NaN" or "0".
    let line = "65, 30, N/A, 4096, 16384";
    let readings = thermalwriter::sensor::nvidia::parse_csv_line(line);
    let power = readings.iter().find(|r| r.key == "gpu_power");
    assert!(power.is_none(), "N/A power must not produce a reading; got {:?}", power);

    let temp = readings.iter().find(|r| r.key == "gpu_temp");
    assert_eq!(temp.map(|r| r.value.as_str()), Some("65"), "valid fields still parse");
}
```

This requires extracting the inline `fields[i].parse()` body of `nvidia.rs::poll` into a `pub fn parse_csv_line(line: &str) -> Vec<SensorReading>` so it can be unit-tested without spawning nvidia-smi.

**Step 7: Run test, observe failure (function doesn't exist yet)**

**Step 8: Refactor + add N/A guard**

Extract the parser, and at each field check `fields[i] != "N/A" && fields[i].parse::<f64>().is_ok()` before pushing the reading.

**Step 9: Run test, observe pass**

**Step 10: Mangohud line-boundary scan**

In `src/sensor/mangohud.rs:62-90`, after the seek + read-to-end into `tail_bytes`, call `tail_bytes.iter().position(|&b| b == b'\n')` and slice from there. If no newline is found, abandon the read for this tick (return empty readings) — better to skip than to parse a partial leading line.

Add a small test that constructs a synthetic CSV with a partial leading line and asserts the parser drops it.

**Step 11: Commit**

```bash
git add src/sensor/rapl.rs src/sensor/nvidia.rs src/sensor/mangohud.rs tests/sensor_tests.rs
git commit -m "fix(sensors): harden RAPL rollover, nvidia N/A handling, mangohud tail scan

- RAPL: cache max_energy_uj at construction; skip reading on rollover
  if the max is unreadable (was substituting u18.4 EJ leading to
  ~18 TW spurious power readings).
- nvidia: skip rows where any queried field is 'N/A' instead of
  emitting NaN/0.
- mangohud: scan forward to the first newline after seek so a
  partial leading line is discarded, not parsed."
```

---

## Task 6: Review Tasks 3-5

**Trigger:** Both reviewers start when Tasks 3, 4, AND 5 are all complete (Phase 1 + first half of Phase 2).

**Killer items (blocking):**
- [ ] `cargo test` runs cleanly (`cargo test --workspace`).
- [ ] `tests/render_tests.rs::fontdb_is_loaded_once_across_multiple_renderers` actually exercises the cache (second renderer construction is ≥ 4× faster than first on the test machine).
- [ ] No `load_system_fonts()` call remains inside `SvgRenderer::new` — `grep -n load_system_fonts src/` returns only the OnceLock initializer.
- [ ] `RaplProvider` no longer calls `read_max_energy_uj` from `poll()` (only from constructor) — `grep -n read_max_energy_uj src/sensor/rapl.rs` confirms it's only in `new()`/`with_base_path()`.
- [ ] `tests/sensor_tests.rs::rapl_rollover_with_unreadable_max_does_not_explode` asserts `watts < 10_000.0`, not just `is_ok()`.
- [ ] Manual smoke test on hardware: `systemctl --user restart thermalwriter`, watch `journalctl --user -u thermalwriter -f`, verify no panic and no terawatt power readings on the LCD.
- [ ] `nvidia::parse_csv_line` is `pub` (or `pub(crate)`) so the test can reach it.

**Quality items (non-blocking):**
- [ ] `MAX_BG_FILE_BYTES` named constant in `src/render/background.rs`.
- [ ] Drop impl in `src/service/xvfb.rs` short-waits between SIGTERM and SIGKILL (e.g. 200 ms) so well-behaved children exit cleanly.
- [ ] `nvidia::parse_csv_line` has a doc comment explaining the N/A semantics.

**Validation Data:**
- `cargo test --workspace` — must pass with no test marked `#[ignore]`.
- `cargo run --example preview_layout layouts/svg/neon-dash-v2.svg` — must produce a valid PNG.
- `pgrep xclock` after the xvfb mode-switch smoke test — must be empty.

**Resolution:** Killer findings block. Quality items queue.

---

## Task 7: Milestone — Phase 1 + Phase 2 complete

**Present to user:**
- Image decode bombs are bounded.
- Layout switches no longer re-scan /usr/share/fonts.
- Xvfb child trees clean up properly on mode change.
- RAPL no longer produces terawatt-scale readings on rollover.
- nvidia/mangohud parsers tolerate N/A and partial-line tail reads.
- Hardware smoke test results.

**Wait for user response before proceeding to Task 8.**

---

## Task 8: USB partial-write loop [READ-DO]

**Files:**
- Modify: `src/transport/bulk_usb.rs:162-190` (`send_frame`).
- Test: extend `tests/transport_tests.rs`. Note: this is a hardware-touching module — the test must NOT depend on a real device. Test the *helper logic* at the function level, not the rusb side.

**Step 1: Extract the chunked-write logic**

Refactor so the loop calls a small helper `fn write_all_bulk(handle: &DeviceHandle<...>, ep: u8, data: &[u8], timeout: Duration) -> Result<()>` that loops until `data` is exhausted or `write_bulk` returns `n == 0` (then bail with "device returned zero-length write — likely disconnected"). Keep the existing 16 KB outer chunking; the new helper handles the inner partial-write loop on each chunk.

**Step 2: Write the failing test**

In `tests/transport_tests.rs`, add a test for the helper using a fake handle. If you can't easily stub `DeviceHandle`, factor the loop into a generic `fn write_all<W: FnMut(&[u8]) -> rusb::Result<usize>>(...)` with `W` being a closure — then the test can pass a closure that fakes a partial write on the first call and a full write on the second.

```rust
#[test]
fn write_all_handles_partial_writes_by_continuing() {
    let data = vec![0u8; 16 * 1024];
    let mut call_count = 0;
    let result = thermalwriter::transport::bulk_usb::write_all(&data, |chunk| {
        call_count += 1;
        if call_count == 1 {
            Ok(chunk.len() / 2) // partial write of half
        } else {
            Ok(chunk.len()) // full write of remainder
        }
    });
    assert!(result.is_ok());
    assert_eq!(call_count, 2, "must call writer twice for the partial then remainder");
}

#[test]
fn write_all_bails_on_zero_length_write() {
    let data = vec![0u8; 100];
    let result = thermalwriter::transport::bulk_usb::write_all(&data, |_| Ok(0));
    assert!(result.is_err());
    let err = result.err().unwrap();
    assert!(err.to_string().contains("zero-length") || err.to_string().contains("disconnected"));
}
```

**Step 3: Run, observe failure (function doesn't exist)**

**Step 4: Implement `write_all`**

```rust
pub fn write_all<W>(data: &[u8], mut write: W) -> Result<()>
where
    W: FnMut(&[u8]) -> rusb::Result<usize>,
{
    let mut sent = 0;
    while sent < data.len() {
        let n = write(&data[sent..]).context("Bulk write failed")?;
        if n == 0 {
            bail!("device returned zero-length write — likely disconnected (after {} bytes)", sent);
        }
        sent += n;
    }
    Ok(())
}
```

In `send_frame`, replace the chunk loop:

```rust
for chunk in frame.chunks(CHUNK_SIZE) {
    write_all(chunk, |buf| handle.write_bulk(self.ep_out, buf, WRITE_TIMEOUT))?;
}
```

**Step 5: Run, observe pass**

Run: `cargo test --test transport_tests`
Expected: PASS (both new tests + existing).

**Step 6: Hardware smoke test**

```bash
systemctl --user restart thermalwriter
# Watch frames appear normally for 60+ seconds
journalctl --user -u thermalwriter --since "1 min ago" | grep -i error
```

Expected: zero errors, frames rendering as before.

**Step 7: Commit**

```bash
git add src/transport/bulk_usb.rs tests/transport_tests.rs
git commit -m "fix(usb): loop on partial bulk writes instead of silently truncating

write_bulk's return value was discarded, so a short write (USB stall,
timeout-recovered partial) was treated as success. The next chunk
would then start at the wrong offset and the device would receive a
truncated/misaligned frame. Now retries until the buffer is drained
or a zero-length write signals disconnection."
```

---

## Task 9: USB reconnect / connection state [READ-DO]

**Coordination required:**
Before starting, confirm with the dev who completed Task 8 that the new `write_all` helper bubbles up `rusb::Error::NoDevice` and `rusb::Error::Pipe` faithfully (without wrapping them in a way that hides the underlying error kind). The reconnect logic in this task needs to distinguish "transient stall" (retry write) from "device gone" (drop handle and re-open).

**Files:**
- Modify: `src/transport/bulk_usb.rs` — add `is_connected(&self) -> bool` and `try_reconnect(&mut self) -> Result<()>` methods.
- Modify: `src/transport/mod.rs` — extend the `Transport` trait with `fn is_connected(&self) -> bool { true }` and `fn try_reconnect(&mut self) -> Result<()> { Ok(()) }` (default impls so non-USB transports don't break).
- Modify: `src/service/tick.rs:150-164` — wrap the send-frame block with reconnect logic.
- Modify: `src/service/dbus.rs` — add a way for the tick loop to update `state.connected` (today line 36 of `ServiceState`). Either share a `watch::Sender<bool>` for `connected`, or have the tick loop hold an `Arc<Mutex<ServiceState>>` clone and update directly.
- Test: extend `tests/transport_tests.rs`.

**Step 1: Pick the cross-task-coordination signal**

Use a `watch::Sender<bool>` for `connected` — it composes with the existing `shutdown` and `background` watch pattern. Add `connected_tx: watch::Sender<bool>` to `ServiceState` and a corresponding `connected_rx` passed into `run_tick_loop`.

**Step 2: Add Transport extension**

Add to `src/transport/mod.rs`:

```rust
pub trait Transport: Send {
    fn handshake(&mut self) -> Result<DeviceInfo>;
    fn send_frame(&mut self, data: &[u8]) -> Result<()>;
    fn close(&mut self);
    /// Whether the underlying device is currently usable. Default: always true.
    fn is_connected(&self) -> bool { true }
    /// Attempt to re-establish the connection. Default: error.
    fn try_reconnect(&mut self) -> Result<()> {
        anyhow::bail!("reconnect not supported by this transport")
    }
}
```

**Step 3: Implement on BulkUsb**

```rust
impl Transport for BulkUsb {
    // ... existing methods ...

    fn is_connected(&self) -> bool {
        self.handle.is_some() && self.info.is_some()
    }

    fn try_reconnect(&mut self) -> Result<()> {
        self.close();
        let new = BulkUsb::new()?;
        // Move new fields into self.
        self.handle = new.handle;
        self.ep_out = new.ep_out;
        self.ep_in = new.ep_in;
        self.handshake()?;
        Ok(())
    }
}
```

When `send_frame` returns `Err`, downcast to `rusb::Error` and on `NoDevice | Pipe | Other(_)` (treat them all as fatal for now), set `self.handle = None` so `is_connected()` becomes `false`. Do this in a tiny `mark_disconnected_if_fatal(err: &anyhow::Error)` helper.

**Step 4: Wire into the tick loop**

In `src/service/tick.rs`, replace:

```rust
if let Err(e) = transport.send_frame(&jpeg) {
    warn!("Failed to send frame: {}", e);
}
```

with:

```rust
if let Err(e) = transport.send_frame(&jpeg) {
    warn!("Failed to send frame: {}", e);
    if !transport.is_connected() {
        let _ = connected_tx.send(false);
        // Try to reconnect with simple backoff. Cap retry rate so a
        // permanently absent device doesn't pin the CPU.
        match transport.try_reconnect() {
            Ok(()) => {
                info!("USB device reconnected");
                let _ = connected_tx.send(true);
            }
            Err(e) => {
                warn!("USB reconnect failed: {} — will retry next tick", e);
                // Sleep so we don't spin on a missing device.
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}
```

The `connected_tx` is a `watch::Sender<bool>` passed into `run_tick_loop`.

**Step 5: Test (mock Transport)**

Add a mock Transport in tests that returns `Err` on `send_frame` once, then `Ok`, and asserts `try_reconnect` was called between them. Use `tests/transport_tests.rs`.

**Step 6: Hardware test**

```bash
systemctl --user restart thermalwriter
# Wait for the LCD to display
# Physically unplug the cooler USB cable for ~5 seconds, plug back in
# Watch journalctl
journalctl --user -u thermalwriter -f
```

Expected: warnings during disconnection, "USB device reconnected" within seconds of replug, LCD resumes.

**Step 7: Commit**

```bash
git add src/transport/ src/service/tick.rs src/service/dbus.rs tests/transport_tests.rs
git commit -m "feat(usb): reconnect after device disappearance

Transport gains is_connected() and try_reconnect(). Tick loop now
detects a fatal USB error, closes the handle, and attempts to
re-open + handshake on subsequent ticks. Daemon survives suspend/
resume and physical cable bumps without manual restart."
```

---

## Task 10: USB send via spawn_blocking [READ-DO]

**Coordination required:**
Confirm with the Task 9 dev that `try_reconnect` is **not** held while the tick loop awaits. The new spawn_blocking pattern must not capture `&mut transport` in a way that conflicts with the tick loop's outer mutable use.

**Files:**
- Modify: `src/service/tick.rs:79-180` — wrap the synchronous send_frame call in `tokio::task::spawn_blocking` *or* `tokio::task::block_in_place` (pick one; `block_in_place` is simpler since `transport` is `&mut dyn`).

**Implement:** Replace direct `transport.send_frame(&jpeg)` calls with `tokio::task::block_in_place(|| transport.send_frame(&jpeg))`. This keeps the borrow checker happy (no `'static` requirement) and keeps the multithreaded runtime responsive during USB writes that take 5+ seconds.

The reconnect block from Task 9 should also be wrapped — `transport.try_reconnect()` is synchronous and IO-heavy.

**Step 1: Read the tokio docs**

`tokio::task::block_in_place` requires the multi-thread runtime. Confirm `#[tokio::main]` in `src/main.rs:31-32` uses the multi-thread flavor (it uses `#[tokio::main]` with default features in `Cargo.toml:38`, which IS multi-thread). If for any reason this is single-thread, switch to `spawn_blocking` + a `tokio::sync::Mutex<Box<dyn Transport + Send>>`.

**Step 2: Wrap the send + reconnect call sites**

```rust
let send_result = tokio::task::block_in_place(|| transport.send_frame(&jpeg));
if let Err(e) = send_result {
    warn!("Failed to send frame: {}", e);
    if !transport.is_connected() {
        let _ = connected_tx.send(false);
        let reconnect_result = tokio::task::block_in_place(|| transport.try_reconnect());
        match reconnect_result {
            Ok(()) => { ... }
            Err(e) => {
                warn!("USB reconnect failed: {}", e);
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
}
```

**Step 3: Run the existing test suite**

`cargo test --workspace` — should still pass.

**Step 4: Hardware verification**

While the daemon is running, in another terminal call a slow D-Bus method:

```bash
time busctl --user call com.thermalwriter.Service /com/thermalwriter/Display \
    com.thermalwriter.Display GetStatus
```

Should respond in well under a second even when the USB write hits a stall (vs 5+ seconds with the old blocking call).

**Step 5: Commit**

```bash
git add src/service/tick.rs
git commit -m "perf(tick): yield the runtime during USB writes

USB send_frame can stall for the full 5s WRITE_TIMEOUT on a busy
bus. Doing it directly on the async executor blocks D-Bus call
handling. block_in_place lets the multi-thread runtime relocate
other tasks during the syscall."
```

---

## Task 11: Review Tasks 8-10

**Trigger:** Both reviewers start when Tasks 8, 9, AND 10 are complete (Phase 3).

**Killer items (blocking):**
- [ ] `tests/transport_tests.rs::write_all_handles_partial_writes_by_continuing` and `::write_all_bails_on_zero_length_write` both exist and pass.
- [ ] `Transport` trait has `is_connected` and `try_reconnect` with default impls; non-USB transports compile without changes.
- [ ] Hardware test: physically disconnect USB during `journalctl -f`, reconnect within 30s, verify "USB device reconnected" log line and LCD resumes within 5 seconds of replug.
- [ ] D-Bus `GetStatus` responds in < 1s even while USB is stalled — measure with `time busctl` during a deliberately stalled state (e.g. unplug for 1 second mid-call).
- [ ] `transport.send_frame` is wrapped in `block_in_place` at every call site in `tick.rs` — `grep -n 'send_frame' src/service/tick.rs` returns only block_in_place'd calls.
- [ ] Reconnect attempts have a sleep (≥ 1 s) between retries so a permanently absent device doesn't peg a CPU.

**Quality items (non-blocking):**
- [ ] `BulkUsb::try_reconnect` logs the error chain on failure, not just the top-level.
- [ ] The `connected_tx` watch channel is also published as a D-Bus signal (`device_disconnected`/`device_connected`).
- [ ] Reconnect backoff is exponential, not constant 2s.

**Validation Data:**
- Manual: unplug/replug test described above.
- `cargo test --workspace` — must pass.

**Resolution:** Killer findings block. Quality items queue.

---

## Task 12: Milestone — USB resilience verified

**Present to user:**
- Demonstrate suspend/resume recovery (or unplug/replug).
- Show D-Bus call latency under USB stall before/after.
- Confirm test suite green.

**Wait for user response before proceeding to Task 13.**

---

## Task 13: Lifecycle — SIGTERM handler + xvfb history parity [READ-DO]

**Files:**
- Modify: `src/main.rs:309-326` (signal handling around the tick loop) and `:120-178` (initial sensor history for xvfb).

**Step 1: Move sensor history out of the if/else**

The current code at `main.rs:122-178` only allocates `SensorHistory` in the non-xvfb branch. The simplest fix: always allocate it, then for xvfb mode just don't `set_history` on any renderer at startup. The `reload_history` capture (line 213) becomes meaningful when the user later switches from xvfb back to a layout via D-Bus.

Concretely: hoist the `let sensor_history = Some(Arc::new(std::sync::Mutex::new(SensorHistory::new())));` *before* the if/else, configure metrics from the layout's frontmatter only in the layout branch, and pass `sensor_history.clone()` into the xvfb-branch's `initial_sensor_history` too.

**Step 2: Write the failing test**

Hard to unit-test (lives in main.rs). Acceptable to make this a manual scenario test: documented in the commit message and verified by hand.

Manual scenario:
1. Set `mode = "xvfb"` in config.
2. Start the daemon.
3. `thermalwriter ctl set-layout svg/neon-dash-v2.svg`.
4. Verify the LCD shows the layout *and* its sensor history graphs populate over the next minute.

Before this fix: the graph stays blank because `reload_history` is `None`.

**Step 3: Add SIGTERM/SIGINT handling**

Replace the bottom of `main.rs`:

```rust
let tick_handle = tokio::spawn(tick::run_tick_loop(
    // ... existing args ...
));

// Handle shutdown signals.
let shutdown_tx_for_signals = state.lock().await.shutdown_tx.clone();
tokio::spawn(async move {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => info!("SIGINT received"),
        _ = sigterm.recv() => info!("SIGTERM received"),
    }
    let _ = shutdown_tx_for_signals.send(true);
});

tick_handle.await??;
```

There's a subtlety: `run_tick_loop` borrows `&mut transport` and `&mut sensor_hub` and `&mut source_rx`. To `tokio::spawn` it you'd need `'static` lifetimes. Easier path: don't spawn the tick loop — keep it inline as today, but `select!` the await on it against the signal futures:

```rust
tokio::select! {
    res = tick::run_tick_loop(...) => { res?; }
    _ = tokio::signal::ctrl_c() => {
        info!("SIGINT received, shutting down");
    }
    _ = async {
        let mut s = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        s.recv().await;
    } => {
        info!("SIGTERM received, shutting down");
    }
}

// Common shutdown: signal the tick loop, give it a moment to drain,
// then explicitly close transport (Drop will fire too, but be explicit).
let _ = state.lock().await.shutdown_tx.send(true);
tokio::time::sleep(std::time::Duration::from_millis(500)).await;
transport.close();
info!("thermalwriter shutdown complete");
```

**Step 4: Verify**

Manual:

```bash
systemctl --user restart thermalwriter
# In another terminal:
systemctl --user stop thermalwriter
journalctl --user -u thermalwriter --since "10 sec ago"
```

Expected: log lines for "SIGTERM received", "Tick loop shutdown requested", "BulkUSB device closed", "thermalwriter shutdown complete" — in that order, with no `Killed` or unclean termination.

**Step 5: Commit**

```bash
git add src/main.rs
git commit -m "fix(daemon): handle SIGTERM gracefully and seed sensor history in xvfb mode

- main: select! the tick loop against ctrl_c() and SIGTERM streams,
  then signal shutdown_tx and let Drop chains run cleanly. Previously
  systemctl stop killed mid-frame, orphaning xvfb children and leaving
  the USB interface claimed.
- xvfb mode: always allocate SensorHistory at startup so a later
  D-Bus set_layout into a history-using layout populates correctly.
  Mirrors the fix in 3882a7c for the layout-startup path."
```

---

## Task 14: Review Task 13

**Killer items (blocking):**
- [ ] `systemctl --user stop thermalwriter` produces a clean shutdown sequence in journalctl: SIGTERM → tick loop shutdown → transport closed.
- [ ] After `set_layout svg/neon-dash-v2.svg` from a daemon that started in xvfb mode, the history-driven graphs populate within 60 seconds.
- [ ] `tokio::select!` includes both `ctrl_c()` and `SignalKind::terminate()`.
- [ ] `transport.close()` is called (or relied-upon-Drop) BEFORE `info!("thermalwriter shutdown complete")` — verify by reading the lines.
- [ ] No regression in startup logs — `systemctl --user start thermalwriter && journalctl --user -u thermalwriter --since "10 sec ago"` shows the same boot lines as before.

**Quality items (non-blocking):**
- [ ] systemd unit (`systemd/thermalwriter.service`) declares `KillSignal=SIGTERM` and `TimeoutStopSec=5` for explicitness.
- [ ] Add `StartLimitIntervalSec=600 StartLimitBurst=3` to the unit so a config that crashes-on-start doesn't restart-loop.

**Resolution:** Killer findings block. Quality items queue.

---

## Task 15: Milestone — Lifecycle clean

**Present to user:**
- Demonstrate `systemctl stop` cleanup.
- Show the xvfb→layout history population.
- Test suite green.

**Wait for user response before proceeding to Task 16.**

---

## Task 16: D-Bus lock-scope + spawn_blocking for image work [READ-DO]

**Files:**
- Modify: `src/service/dbus.rs:385-421` (`set_layout_vars`, `set_background`).

**Step 1: Identify the heavy-work segments**

In `set_background` (`:409-421`):
```rust
let mut state = self.state.lock().await;
let bg_path = validate_background_path(&state.background_dir, &name)?;  // fast
let pixmap = crate::render::background::decode_from_file(&bg_path)?;    // SLOW (50-200 ms)
let config_path = state.config_path.clone();
let tx = state.mode_change_tx.clone();
apply_background(...)?;                                                  // fast
state.current_background = Some(pixmap);
```

The decode happens inside the lock. We need to:
1. Lock briefly to clone `background_dir`, `config_path`, `mode_change_tx`.
2. Drop the guard.
3. Validate + decode (in `spawn_blocking` since it's CPU-bound).
4. Re-lock briefly to commit the result.

**Step 2: Refactor**

```rust
async fn set_background(&self, name: String) -> zbus::fdo::Result<()> {
    // Acquire lock briefly to clone the inputs we need.
    let (background_dir, config_path, tx) = {
        let state = self.state.lock().await;
        (state.background_dir.clone(), state.config_path.clone(), state.mode_change_tx.clone())
    };

    // Heavy work outside the lock, on a blocking thread.
    let bg_path = validate_background_path(&background_dir, &name)?;
    let pixmap = tokio::task::spawn_blocking(move || {
        crate::render::background::decode_from_file(&bg_path)
    })
    .await
    .map_err(|e| zbus::fdo::Error::Failed(format!("decode task panicked: {}", e)))?
    .map_err(|e| zbus::fdo::Error::Failed(format!("Failed to decode background '{}': {}", name, e)))?;

    // Persist + signal the tick loop. apply_background is async; do this
    // outside the state lock too — it only needs config_path and tx, which
    // we already cloned.
    apply_background_outside_lock(&config_path, Some(&name), Some(pixmap.clone()), &tx).await?;

    // Final brief lock to commit the in-memory state mirror.
    {
        let mut state = self.state.lock().await;
        state.current_background = Some(pixmap);
        state.config.background.image = Some(name.clone());
    }
    Ok(())
}
```

You'll need a new `apply_background_outside_lock` helper that takes `&Path, Option<&str>, Option<Pixmap>, &mpsc::Sender<ModeChange>` rather than `&mut Config`. The in-memory `Config` mutation moves into the final brief lock above.

**Step 3: Same pattern for `set_layout_vars`**

`set_layout_vars` holds the lock across `mode_change_tx.send().await` (line 399-403). Drop the lock before the send: clone `vars` and `name`, drop the guard, then `tx.send(...).await`.

**Step 4: Run tests**

`cargo test --workspace` — must still pass. Several existing dbus tests (`tests/dbus_tests.rs`) need to remain green.

**Step 5: Verify D-Bus responsiveness**

While daemon is running:

```bash
# Set a 4K background that takes a while to resize:
busctl --user call com.thermalwriter.Service /com/thermalwriter/Display \
    com.thermalwriter.Display SetBackground s "huge.png" &

# Immediately try to get status:
time busctl --user call com.thermalwriter.Service /com/thermalwriter/Display \
    com.thermalwriter.Display GetStatus
```

`GetStatus` should respond in < 50 ms regardless of the parallel decode.

**Step 6: Commit**

```bash
git add src/service/dbus.rs
git commit -m "perf(dbus): release the state lock during image decode/resize

set_background previously held the ServiceState mutex through the
full Lanczos3 resize. set_layout_vars held it across an .await on
the mode_change channel. Both are now: clone the data we need,
drop the guard, do the heavy work, re-acquire briefly to commit."
```

---

## Task 17: Single config writer [READ-DO]

**Coordination required:**
Confirm with the Task 16 dev that `dbus.rs::set_background`'s commit-step has access to the in-memory `state.config`, since this task adds *another* in-memory write path (`set_default_layout` D-Bus method). They should not race; the brief commit-lock pattern from Task 16 is the right shape for both.

**Files:**
- Modify: `src/service/dbus.rs` — add a new D-Bus method `set_default_layout(name: String) -> Result<()>`.
- Modify: `src/dbus_types.rs` — extend the proxy with `set_default_layout`.
- Modify: `src/config.rs:208+` — `save_display_layout` is fine as-is; just call it from the daemon side.
- Modify: `gui/src-tauri/src/commands.rs:186-206` — `save_config` should call `set_default_layout` over D-Bus instead of writing the config directly.
- Test: extend `tests/dbus_tests.rs`.

**Step 1: Add the D-Bus method**

```rust
async fn set_default_layout(&self, name: String) -> zbus::fdo::Result<()> {
    // Validate + persist.
    let (layout_dir, config_path) = {
        let state = self.state.lock().await;
        (state.layout_dir.clone(), state.config_path.clone())
    };
    validate_layout_path(&layout_dir, &name)?;
    let mode = if name.ends_with(".html") { "html" } else { "svg" };
    Config::save_display_layout(&config_path, &name, mode)
        .map_err(|e| zbus::fdo::Error::Failed(format!("save_display_layout: {}", e)))?;

    // Update in-memory mirror.
    {
        let mut state = self.state.lock().await;
        state.config.display.default_layout = name.clone();
        state.config.display.mode = mode.to_string();
    }
    Ok(())
}
```

**Step 2: Update the GUI proxy**

In `src/dbus_types.rs`, add the method to the proxy macro.

**Step 3: Have GUI call the new method**

In `gui/src-tauri/src/commands.rs::save_config` (line 187-206), replace the direct `Config::save_display_layout` call with a D-Bus call to the new `set_default_layout`. Keep the direct `Config::save_layout_vars` call only if the daemon isn't running; if the daemon IS running, route both writes through D-Bus (`apply_to_daemon` already does the layout_vars write via `set_layout_vars` over D-Bus).

The simplest reshape: `save_config` becomes a fallback path used only when the daemon is unreachable. When the daemon is up, `apply_to_daemon` is sufficient (after Task 18 removes the redundant call) — and a new D-Bus method `set_default_layout` makes that idempotent.

**Step 4: Test**

Add a `tests/dbus_tests.rs` test that calls `set_default_layout` against a tempdir-rooted config and asserts the on-disk file contains the new value.

**Step 5: Commit**

```bash
git add src/service/dbus.rs src/dbus_types.rs gui/src-tauri/src/commands.rs tests/dbus_tests.rs
git commit -m "fix(config): make daemon the sole writer of config.toml

GUI no longer writes to config.toml directly while the daemon is
running. New D-Bus SetDefaultLayout method routes 'sticky' layout
selection through the daemon. Concurrent GUI Apply + ctl writes
no longer clobber each other."
```

---

## Task 18: Review Tasks 16-17

**Killer items (blocking):**
- [ ] `set_background` does NOT hold `state.lock()` across `decode_from_file` — confirm by reading the code; the lock guard scope ends before the decode call.
- [ ] `set_layout_vars` does NOT hold `state.lock()` across `mode_change_tx.send().await` — same check.
- [ ] D-Bus method latency check: `time busctl ... GetStatus` while a `SetBackground "large.png"` is in flight — must be < 50 ms.
- [ ] GUI no longer calls `Config::save_display_layout` directly when the daemon is running — `grep -n save_display_layout gui/src-tauri/` returns only the daemon-down fallback path.
- [ ] `tests/dbus_tests.rs` has a test for `set_default_layout` that asserts both on-disk and in-memory state update.
- [ ] `cargo test --workspace` passes.

**Quality items (non-blocking):**
- [ ] `apply_background_outside_lock` gets a doc comment explaining why the locked `apply_background` was split.
- [ ] `Config::save_layout_vars` and `save_display_layout` get an `flock`-based file lock as belt-and-braces.

**Resolution:** Killer findings block.

---

## Task 19: Milestone — D-Bus refactor verified

**Wait for user response before proceeding to Task 20.**

---

## Task 20: Tauri devtools gate + remove redundant set_layout [DO-CONFIRM]

**Files:**
- Modify: `gui/src-tauri/Cargo.toml:14-25`.
- Modify: `gui/src-tauri/src/commands.rs:208-240` (`apply_to_daemon`).
- Modify: `gui/src-tauri/tauri.conf.json:25` (add explicit `script-src 'self'` for hygiene).

**Implement:**
1. Move the `devtools` feature behind `cfg(debug_assertions)`. Tauri's `devtools` feature controls inspector availability; in Tauri 2 the cleanest pattern is:
   ```toml
   [dependencies]
   tauri = { version = "2" }
   # devtools only in debug builds
   [target.'cfg(debug_assertions)'.dependencies]
   tauri = { version = "2", features = ["devtools"] }
   ```
   If cargo refuses the duplicate `tauri` key in `[target.cfg]`, switch to a project feature: define `[features] devtools = ["tauri/devtools"]` and only enable it on debug builds via `cargo build --features devtools` in the dev workflow (Tauri's CLI does this automatically when invoked via `cargo tauri dev`).

2. In `apply_to_daemon`, remove the redundant second `proxy.set_layout(&layout).await` (line 235-238). `set_layout_vars` already triggers a `ModeChange::Layout` in the daemon. Add a comment explaining why the second call was removed.

3. In `tauri.conf.json:25`, add `script-src 'self'` to the CSP for explicitness:
   ```json
   "csp": "default-src 'self' ipc: http://ipc.localhost; script-src 'self'; img-src 'self' blob: data:; connect-src ipc: http://ipc.localhost; style-src 'self' 'unsafe-inline'"
   ```

**Confirm checklist (killer items):**
- [ ] Failing test written FIRST: not strictly testable in unit form, but a manual `cargo build --release -p thermalwriter-gui` followed by checking the binary lacks `wry::devtools` symbols (use `nm` or `strings`) is the verification.
- [ ] Release build of the GUI does not show "Inspect Element" on right-click. Test: `cd gui && npm run tauri build`, run the bundled app, right-click the window — no devtools menu item.
- [ ] `apply_to_daemon` calls `set_layout_vars` exactly once and does NOT call `set_layout` after — `grep -A 30 'pub async fn apply_to_daemon' gui/src-tauri/src/commands.rs` confirms.
- [ ] CSP in `tauri.conf.json` has explicit `script-src 'self'`.
- [ ] `npm run check` (svelte-check) passes — the GUI typechecks.
- [ ] Daemon receives one `ModeChange::Layout` per Apply click, not two — verify via journalctl during a manual GUI Apply.
- [ ] Committed with a clear message.

---

## Task 21: Review Task 20

**Killer items (blocking):**
- [ ] Release build does not enable Tauri devtools. `cargo build -p thermalwriter-gui --release` succeeds and the resulting binary has no inspector wired up.
- [ ] `apply_to_daemon` body has a single `proxy.set_layout_vars` call followed by `Ok(())`; no `set_layout` call.
- [ ] CSP string in `tauri.conf.json` contains `script-src 'self'`.
- [ ] `cargo test --workspace` passes.
- [ ] GUI Apply sends exactly one `ModeChange::Layout` (verified by enabling debug logging and counting log lines for one click).

**Quality items (non-blocking):**
- [ ] Add a CI step that runs `cargo build -p thermalwriter-gui --release` and fails if `devtools` shows up in `cargo tree`.
- [ ] Frontend disables the Apply button while a request is in flight (defensive against double-click).

**Resolution:** Killer findings block.

---

## Task 22: Milestone — GUI hygiene done

**Wait for user response before proceeding to Task 23.**

---

## Task 23: Tick loop polish + config validation [READ-DO]

**Coordination required:**
Confirm with the Task 9-10 dev that the `connected_tx` watch channel can co-exist with a new `tick_rate_rx` watch channel passed into `run_tick_loop`. If `run_tick_loop` is getting too many parameters, group them in a `TickContext` struct.

**Files:**
- Modify: `src/service/tick.rs:79-180` — `while let` drain on `source_rx`; reset `cached_sensors` on source swap; honor a `tick_rate_rx` watch channel.
- Modify: `src/service/dbus.rs::set_tick_rate` (around line 478) — actually push the new value through `tick_rate_tx`.
- Modify: `src/config.rs:106-117` — add `validate()` method called by `load()` that bounds `tick_rate ∈ [1, 60]`, `jpeg_quality ∈ [10, 100]`, `rotation ∈ {0, 90, 180, 270}`, `poll_interval_ms ∈ [100, 60_000]`, `xvfb.tick_rate ∈ [1, 60]`. Return `Err` with which-field-was-bad on invalid values.
- Test: `tests/config_tests.rs` extension.

**Step 1: `while let` drain in tick.rs**

Replace:
```rust
if let Ok(new_source) = source_rx.try_recv() {
    info!("Frame source swapped to: {}", new_source.name());
    frame_source = new_source;
    frame_source.set_background(cached_background.clone());
}
```

with:
```rust
let mut latest_source: Option<Box<dyn FrameSource>> = None;
while let Ok(new_source) = source_rx.try_recv() {
    latest_source = Some(new_source);
}
if let Some(new_source) = latest_source {
    info!("Frame source swapped to: {} (drained queue)", new_source.name());
    frame_source = new_source;
    frame_source.set_background(cached_background.clone());
    cached_sensors.clear();  // NEW: invalidate cache on source swap
}
```

**Step 2: tick_rate watch channel**

Add `tick_rate_rx: tokio::sync::watch::Receiver<u32>` to `run_tick_loop`'s parameters and to `ServiceState`. At the top of each loop iteration, recompute `tick_duration` from the latest value:

```rust
let current_fps = *tick_rate_rx.borrow();
let tick_duration = Duration::from_secs_f64(1.0 / current_fps.max(1) as f64);
```

In `dbus.rs::set_tick_rate`, after updating `state.tick_rate`, also send through the watch channel.

**Step 3: Config validation**

In `Config::load`, after `toml::from_str`, call `cfg.validate()?`. Implement `validate(&self) -> Result<()>` returning a clear `anyhow::Error` for each out-of-range field.

**Step 4: Tests**

Three tests:
- `config_load_rejects_zero_tick_rate` — write a config with `tick_rate = 0`, assert load returns Err.
- `config_load_rejects_invalid_rotation` — `rotation = 45` returns Err.
- `set_tick_rate_actually_changes_loop_rate` — harder; document as a manual scenario test.

**Step 5: Commit**

```bash
git add src/service/tick.rs src/service/dbus.rs src/config.rs tests/config_tests.rs
git commit -m "fix(daemon): drain pending sources, propagate tick_rate, validate config

- tick: while-let drain so 5 rapid GUI applies don't take 2.5s to settle
- tick: clear cached_sensors on source swap so the new layout doesn't
  render with the old layout's sensor snapshot
- dbus: set_tick_rate now actually changes the loop rate via watch channel
- config: reject tick_rate=0, jpeg_quality>100, rotation∉{0,90,180,270},
  out-of-range poll intervals at load time"
```

---

## Task 24: Review Task 23

**Killer items (blocking):**
- [ ] `tests/config_tests.rs::config_load_rejects_zero_tick_rate` exists and passes.
- [ ] `tests/config_tests.rs::config_load_rejects_invalid_rotation` exists and passes.
- [ ] `Config::load` calls `validate()` after `toml::from_str` — `grep -A 5 'pub fn load' src/config.rs` confirms.
- [ ] In `run_tick_loop`, the source-swap path uses `while let` not `if let` — `grep -B 1 -A 5 'source_rx.try_recv' src/service/tick.rs` confirms.
- [ ] `cached_sensors.clear()` runs after a source swap.
- [ ] `set_tick_rate` D-Bus method actually changes the loop rate — verify by setting it to 5 FPS and observing `journalctl` (debug log shows new tick duration) or by a stopwatch test on a non-default value.
- [ ] No regression in `cargo test --workspace`.

**Quality items (non-blocking):**
- [ ] Validation errors quote the offending field name AND value: `"display.tick_rate=0 out of range [1,60]"`.
- [ ] `latest_source` drain logs the count of dropped sources for visibility.

**Resolution:** Killer findings block.

---

## Task 25: Milestone — Final integration check

**Present to user:**
- Full `cargo test --workspace` green.
- Manual end-to-end sweep:
  - Daemon starts cleanly, LCD renders.
  - GUI Apply with rapid changes settles immediately.
  - `set_tick_rate 5` actually slows the LCD.
  - Config with `tick_rate = 0` is rejected at load with a clear message.
  - USB unplug/replug recovers cleanly.
  - `systemctl --user stop` is graceful.
  - Background image switching is responsive even with a 4K source.

**Wait for user response. After approval, the campaign is complete.**

---

## Out of Scope / Deferred

The following review findings are intentionally NOT addressed in this campaign — deferred or de-scoped after analysis during planning:

- **D-Bus authorization model** — session bus is the user's own; no boundary to enforce. Document and move on.
- **TOCTOU between path validation and read** — requires write access to the layouts dir, which already implies user-level compromise.
- **Hwmon CPU alias robustness across kernel renames** — needs domain decisions about fallback policy; tracked as a future MINOR enhancement.
- **Repeated sensor failure log spam dedup** — quality-of-life only; no functional impact.
- **`autoescape_on(vec![])` SVG sensor injection** — sensor sources are internal (hwmon/nvidia-smi). Theoretical risk only; deferred.
- **Animation frame buffer reuse** — performance optimization; defer until profiling shows it matters.
- **`chunks(4)` vs `chunks_exact(4)` in `RawFrame::from_pixmap`** — the `tiny_skia::Pixmap::data()` invariant makes this safe today. Cosmetic.
- **Edition 2024 "invalid"** — false positive; Rust 1.85 stabilized it.
- **CSP "missing script-src"** — false positive; `default-src 'self'` covers it. We add explicit `script-src` in Task 20 for hygiene only.
- **OOB read on `resp[36]`** — false positive; the preceding `n < 41` check guarantees the index is in bounds, and `resp` is a 1024-byte stack array regardless.
- **Tera SSTI via `{% include %}`** — false positive; `add_raw_template` has no filesystem loader.
