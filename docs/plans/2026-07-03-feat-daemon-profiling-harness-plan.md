---
title: "feat: Daemon CPU + Memory Profiling Harness"
type: feat
date: 2026-07-03
brainstorm: docs/brainstorms/2026-07-03-daemon-profiling-brainstorm.md
---

# feat: Daemon CPU + Memory Profiling Harness

## Overview

Build repeatable, checked-in profiling infrastructure for the thermalwriter daemon so "lightweight" (14 MB binary, 29 MB RSS, ~1% CPU at 2 FPS) becomes a measured, diffable property instead of folklore. Four workstreams:

1. **NullTransport** — run the full daemon headless (no cooler attached)
2. **Scenario harness** — scripts that boot the daemon per scenario and capture flamegraphs, dhat allocation profiles, and RSS/CPU timelines
3. **Criterion micro-benches** — statistical baselines for the hot pipeline stages
4. **Baseline document + real-hardware validation**

**Headline constraint:** the shipped default binary must be effectively unchanged — no new runtime dependencies, no new CLI surface, no measurable size growth.

## Problem Statement

There is no way to measure where the daemon's CPU cycles or memory go, no baselines to diff after a change, and no way to notice slow regressions. The only performance tooling is `thermalwriter bench` (`src/cli.rs:153`), which measures USB wire throughput only.

## Key Design Decisions (settled during research/spec analysis)

1. **One daemon process per scenario.** Every scenario axis (layout, mode, background, tick rate) is expressible in `config.toml`, which the harness writes into an isolated `XDG_CONFIG_HOME`. Fresh boot per scenario, SIGTERM after capture. No D-Bus switching in the harness — switching inside one process contaminates dhat accumulation, RSS peaks, and perf attribution across scenarios.
2. **Two builds, two runs per scenario.** The dhat `#[global_allocator]` swap changes allocation behavior, so: flamegraphs + RSS timelines come from the `profiling` cargo profile build; allocation profiles from a separate `--features dhat-heap` build. The summary table records which build produced which column. Always SIGTERM (never SIGKILL) — dhat writes its output on `Profiler` drop, and the daemon already shuts down cleanly on SIGTERM (`src/main.rs:569`).
3. **Transport selection via env var** `THERMALWRITER_TRANSPORT=null`, read once at startup in `main.rs`, parsed by a pure function taking `Option<&str>` (unit-testable without env mutation — avoids the `#[serial]` trap). No CLI/help surface change; NullTransport is ~30 dependency-free lines, always compiled, so CI covers it and all build variants behave identically.
4. **Headless runs use `dbus-run-session`** — the daemon hard-fails without a session bus (`dbus::serve` at `src/main.rs:271`). Corollary: null-transport runs do NOT stop the live systemd service (private bus, no USB claim) — the LCD keeps running during headless profiling. Only real-hardware mode stops the service, and restores it unconditionally afterward.
5. **Curated default sweep (~12 scenarios), not the full 36-cell matrix.** Full cross ≈ 1.5 h with mostly uninformative cells. Single-scenario invocation is the primary UX; full matrix behind `--all`.
6. **"Idle" scenario dropped** (the tick loop always renders — there is no idle state). "Startup" is its own capture mode: record from spawn, measure time-to-first-frame and startup allocation peak.

## Technical Approach

### Phase 1 — Foundations: NullTransport + build plumbing

**`src/transport/null.rs` (new)**
- `NullTransport` implementing `Transport` (`src/transport/mod.rs:19`): `handshake()` returns synthetic `DeviceInfo` (480×480, `use_jpeg=true`); `send_frame` counts frames and discards (optional `THERMALWRITER_NULL_LATENCY_MS` sleep — safe, called via `block_in_place`; default off); `close()` logs total frames sent (exact denominator for CPU-per-frame; catches tick overruns at 60 FPS). Trait defaults (`is_connected()=true`, `try_reconnect` bails) are correct as-is — reconnect path never fires.
- Pure selection function, e.g. `transport_from_env(value: Option<&str>) -> TransportKind` with unit tests (no env mutation).

**`src/main.rs:97`**
- Branch construction on the env var: `Box<dyn Transport>` (NullTransport skips the entire `BulkUsb::new()`/`disconnected()` block). `run_tick_loop` already takes `&mut dyn Transport` (`src/service/tick.rs:83`); `close()` at `main.rs:577` works through the trait.

**`Cargo.toml`**
- `[profile.profiling]`: `inherits = "release"`, `debug = true`, `strip = false` (workspace root; changes nothing about default builds).
- `dhat` as optional dependency behind a new `dhat-heap` feature; `#[global_allocator]` + `Profiler` init in `main.rs` under `#[cfg(feature = "dhat-heap")]`.

**`src/sensor/mock.rs` (new, `#[doc(hidden)]`)**
- Promote the duplicated `mock_sensors()` / `mock_sensors_varying()` / synthetic-history fills from `examples/preview_layout.rs:81,108` and `examples/render_layout.rs:89,106` into the library so benches can import them; examples switch to the shared helper (bench targets can't import example code).

**Acceptance for Phase 1:**
- [ ] `THERMALWRITER_TRANSPORT=null dbus-run-session -- cargo run -- daemon` runs the full daemon headless with no hardware
- [ ] Selection logic unit-tested via pure function; NullTransport unit tests in `tests/`
- [ ] `cargo build --release` binary size and `cargo tree` output unchanged vs. master (mechanical check, recorded in PR)
- [ ] Existing 279 workspace tests + `--no-default-features` (178) still pass

### Phase 2 — Criterion micro-benches

**SvgRenderer stage extraction (prerequisite refactor — own task, test-verified)**
- `SvgRenderer::render` is one ~94-line trait method (`src/render/svg.rs:152-246`). Extract the sub-stages into named functions exposed via a `#[doc(hidden)]` pub internals module so external bench targets can call them: Tera context build + render (svg.rs:154-198), `usvg::Tree::from_str` parse (svg.rs:201), `resvg::render` rasterize (svg.rs:214), bg blit/composite (svg.rs:217-241), `RawFrame::from_pixmap` premultiplied→straight conversion (`src/render/mod.rs:36-56`).
- Behavior-preserving: existing render tests must pass unchanged; add a golden-pixmap test if coverage is thin.

**`benches/` (new dir, criterion as dev-dependency)**
- `benches/pipeline.rs` (`required-features = ["daemon"]` — `encode_jpeg`/`rotate_pixels` live in daemon-gated `service::tick`): `rotate_pixels` 0/90/180/270 (`src/service/tick.rs:19`), `encode_jpeg` at quality 85 + a quality sweep (`tick.rs:65`).
- `benches/render.rs`: per built-in SVG layout full render (neon-dash-v2, neon-dash, arc-gauge, cyber-grid) using `sensor::mock` + synthetic history; each extracted sub-stage individually; `background::decode_to_pixmap` (`src/render/background.rs:29`); `RawFrame::from_pixmap`.

**CI (`.github/workflows/ci.yml` + `Cargo.toml`)**
- Add `/benches/**` to `package.include` (else `cargo package` fails on declared `[[bench]]` targets)
- Bench code must be clippy-clean (`--all-targets -D warnings` already compiles it)
- Add `cargo check --features dhat-heap` step so the gated allocator code can't rot

**Documented compare workflow** (in the harness README section): `cargo bench -- --save-baseline before` → change → `cargo bench -- --baseline before`. Criterion baselines are local-only (`target/criterion`); cross-machine comparability comes from these benches, not the harness numbers.

**Acceptance for Phase 2:**
- [ ] `cargo bench` runs all benches green; sub-stage extraction passes existing render tests
- [ ] CI passes: fmt, test (default + no-default-features), clippy --all-targets, package, dhat-heap check

### Phase 3 — Scenario harness

**`scripts/profile.sh` (or `scripts/profile/` with helpers) — NOT a shipped subcommand**

Per-scenario flow:
1. Write scenario `config.toml` into a scratch `XDG_CONFIG_HOME`; isolate `XDG_RUNTIME_DIR` too (xvfb scenarios write `last.jpg` per tick via `frame_dump` — must not race the live daemon's runtime dir)
2. Boot the daemon under `dbus-run-session` with `THERMALWRITER_TRANSPORT=null`
3. Warm up ~10 s (seeding, sensor priming, font-db load), then capture for the measurement window; startup scenario records from spawn instead
4. **CPU:** `perf record --call-graph dwarf -p <pid>` (daemon PID only — nvidia-smi forks a child per poll; don't follow forks) → flamegraph via inferno; **CPU-per-frame** from `/proc/<pid>/stat` utime+stime delta ÷ frames sent (NullTransport's logged count)
5. **RSS:** sample `/proc/<pid>/status` VmRSS on an interval → timeline CSV
6. SIGTERM, collect artifacts into `profiling-results/<scenario>/` (gitignored)
7. Repeat with the `dhat-heap` build for the allocation profile

**Preflight checks (fail fast, actionable messages):** perf present; `kernel.perf_event_paranoid` ≤ 2 (suggest the sysctl); inferno/flamegraph tool present; Xvfb + conky present (else skip xvfb scenarios with a notice, don't fail the sweep); `dbus-run-session` present.

**Default sweep (~12 runs, full matrix behind `--all`):**
- Layout axis: each of the 4 SVG layouts + `system-stats.html`, bg-off, 2 FPS
- Rate/bg axis: neon-dash-v2 across bg on/off × 2/15/60 FPS
- xvfb-conky at its configured rate (mark local-only if binaries missing)
- Startup (time-to-first-frame + allocation peak)

**Summary emitter:** harness writes a machine-generated markdown table (per scenario: CPU-per-frame, avg/peak RSS, allocation totals, frames sent) with a metadata block — commit hash, CPU/GPU model, kernel, build profile, date. "Refresh baselines" = re-run one command and commit the diff.

**Acceptance for Phase 3:**
- [ ] `scripts/profile.sh <scenario>` produces flamegraph + dhat profile + RSS timeline headlessly, while the live systemd service keeps running untouched
- [ ] `scripts/profile.sh --all` completes the curated sweep and emits the summary table
- [ ] Preflight failures produce actionable errors; missing Xvfb/conky skips (not fails) those scenarios

### Phase 4 — Baselines, real-hardware mode, docs

**Real-hardware mode (`scripts/profile.sh --hardware <scenario>`):**
- Record whether `thermalwriter.service` is active; stop it; `trap` on EXIT restores it **only if it was previously running** (never start a service the user disabled)
- Short sleep/retry after stop — the service may not have released the USB claim yet when the daemon-under-test handshakes
- Unset `THERMALWRITER_TRANSPORT` so the real `BulkUsb` path runs

**Baseline document `docs/profiling-baselines.md`:**
- First full sweep committed with the metadata block; explicitly note baselines are machine-specific (real sensor polling is part of whole-daemon truth) — cross-machine regression detection is criterion's job
- At least one headless scenario cross-checked against a `--hardware` run of the same scenario; deltas noted (USB send cost, reconnect polling)
- Not added to the crate `package.include` (repo documentation, not crate content)

**Docs:** harness usage + criterion compare workflow in `docs/` (e.g. `docs/profiling.md`), linked from CLAUDE.md commands section.

**Acceptance for Phase 4:**
- [ ] `docs/profiling-baselines.md` exists with measured numbers for every default-sweep scenario, replacing the folklore "29 MB RSS, 1% CPU"
- [ ] One headless-vs-hardware cross-check recorded
- [ ] systemd service verified restored after a `--hardware` run (including on script failure mid-run)

## Alternative Approaches Considered

- **`thermalwriter profile` subcommand** — rejected: adds shipped code, violating the headline constraint.
- **D-Bus-driven scenario switching in one daemon process** — rejected: cross-scenario contamination of dhat/RSS/perf attribution; config-file-per-process achieves every axis with cleaner isolation.
- **cfg-feature-gated NullTransport** — rejected: forces another build variant CI wouldn't exercise by default; a ~30-line always-compiled transport is cheaper and identically behaved across builds.
- **heaptrack / valgrind massif as primary memory tool** — dhat chosen for checked-in ergonomics (pure Rust, feature-gated, deterministic output file); document the others as manual alternatives in `docs/profiling.md`.
- **cargo-flamegraph** — plain `perf record -p` + inferno chosen because we attach to an already-running warmed-up daemon rather than wrapping the spawn.

## Acceptance Criteria (roll-up)

### Functional
- [ ] Full daemon runs headless via `THERMALWRITER_TRANSPORT=null` under `dbus-run-session`
- [ ] One command profiles any scenario (CPU flamegraph + dhat allocations + RSS timeline); `--all` runs the curated sweep and emits the summary table
- [ ] `cargo bench` gives per-stage numbers diffable via criterion save/load baselines
- [ ] `--hardware` mode profiles against the real device and restores the systemd service afterward

### Non-functional (the headline constraint)
- [ ] Default `cargo build --release` binary size and `cargo tree` unchanged vs. master (checked mechanically in the PR)
- [ ] No new default-features dependencies; dhat only under `dhat-heap`; criterion is dev-only

### Quality gates
- [ ] All CI steps green, including new `cargo check --features dhat-heap`
- [ ] NullTransport + env parsing unit-tested without env mutation; SvgRenderer stage extraction passes existing render tests
- [ ] Baseline doc committed with metadata block

## Dependencies & Risks

- **Host tooling:** perf, inferno, dbus-run-session, (optional) Xvfb + conky — preflight-checked, never assumed
- **perf_event_paranoid** varies by distro — preflight with actionable fix
- **Sensor noise:** real polling makes harness numbers jittery; measurement windows ≥ 60 s and the criterion layer compensate
- **SvgRenderer refactor risk:** hot-path extraction could subtly change rendering — mitigated by behavior-preserving tests before benches land
- **Risk of blocking on hardware:** only Phase 4's cross-check needs the device; everything else is headless

## References

### Internal
- Brainstorm (decisions + constraints): `docs/brainstorms/2026-07-03-daemon-profiling-brainstorm.md`
- Transport trait + defaults: `src/transport/mod.rs:19-35`; existing test mock: `tests/transport_tests.rs:10-69`
- Construction site: `src/main.rs:97-120`; tick loop (already `&mut dyn Transport`): `src/service/tick.rs:82-98`
- Bench targets: `src/service/tick.rs:19` (rotate), `:65` (encode); `src/render/svg.rs:152-246` (render stages); `src/render/mod.rs:36-56` (from_pixmap); `src/render/background.rs:29` (decode)
- Mock sensors to promote: `examples/preview_layout.rs:81,108`, `examples/render_layout.rs:89,106`
- CI: `.github/workflows/ci.yml`; package include list: `Cargo.toml:13-43`; feature precedent: `Cargo.toml:90-96`
- Config/path isolation: `src/main.rs:51-53` (XDG_CONFIG_HOME), `src/service/frame_dump.rs:20-22` (XDG_RUNTIME_DIR fails closed)

### External
- dhat-rs (feature-gated heap profiling), criterion.rs (save/compare baselines), inferno (flamegraph from perf script), Linux perf `--call-graph dwarf`
