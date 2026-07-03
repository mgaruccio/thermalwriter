# Profiling

## Benchmarking (criterion)

`benches/pipeline.rs` and `benches/render.rs` give statistical baselines for
the hot pipeline stages: pixel rotation, JPEG encoding, and every stage of the
SVG render pipeline (Tera context build, template substitution, SVG parse,
rasterize, composite, background decode, premultiplied→straight conversion).

Run all benches:

```bash
cargo bench
```

Compare before/after a change:

```bash
cargo bench -- --save-baseline before
# ... make your change ...
cargo bench -- --baseline before
```

Criterion baselines are saved under `target/criterion` and are local-only —
they don't survive across machines. Cross-machine regression detection is
this benchmark suite's job; the scenario harness (below) captures
machine-specific, whole-daemon numbers instead.

<!-- Scenario harness usage (flamegraphs, dhat allocation profiles, RSS
     timelines) is documented here once scripts/profile.sh lands. -->

## Scenario harness (`scripts/profile.sh`)

Not a shipped subcommand — a dev-only script, invoked directly from a
checkout. It boots the daemon headlessly (`THERMALWRITER_TRANSPORT=null`, no
cooler attached, under `dbus-run-session` so it never touches the live
systemd service or the real USB device), one fresh daemon process per
scenario, and captures a CPU flamegraph + RSS timeline (`profiling` build)
and an allocation profile (`profiling` + `dhat-heap` build), then emits a
machine-generated markdown summary. Where the criterion benches above give
per-stage, cross-machine-comparable numbers, this harness gives
whole-daemon, machine-specific numbers — "how much CPU/RAM does the actual
running daemon use under this configuration."

### Usage

```bash
scripts/profile.sh --list                 # show available scenarios
scripts/profile.sh <scenario>             # profile one scenario
scripts/profile.sh --all                  # curated ~12-scenario sweep
scripts/profile.sh --hardware <scenario>  # profile against the real device
```

`--hardware` stops `thermalwriter.service` for the duration (freeing the USB
device) and restores it afterward — but only if it was actually running
beforehand, so it never starts a service you'd deliberately disabled. It
also leaves `THERMALWRITER_TRANSPORT` unset so the daemon takes the real
`BulkUsb` path instead of `NullTransport`. **Never run `--hardware` without
first confirming nothing else needs the cooler LCD** — it briefly takes over
the display.

### Scenarios in the default `--all` sweep

| scenario | layout | mode | background | tick rate |
|---|---|---|---|---|
| `neon-dash-v2` | svg/neon-dash-v2.svg | svg | dark-gradient.png | 2 |
| `neon-dash` | svg/neon-dash.svg | svg | dark-gradient.png | 2 |
| `arc-gauge` | svg/arc-gauge.svg | svg | dark-gradient.png | 2 |
| `cyber-grid` | svg/cyber-grid.svg | svg | dark-gradient.png | 2 |
| `system-stats` | system-stats.html | html | dark-gradient.png | 2 |
| `neon-dash-v2-bg-off` | svg/neon-dash-v2.svg | svg | (none) | 2 |
| `neon-dash-v2-15fps` | svg/neon-dash-v2.svg | svg | dark-gradient.png | 15 |
| `neon-dash-v2-60fps` | svg/neon-dash-v2.svg | svg | dark-gradient.png | 60 |
| `neon-dash-v2-bg-off-15fps` | svg/neon-dash-v2.svg | svg | (none) | 15 |
| `neon-dash-v2-bg-off-60fps` | svg/neon-dash-v2.svg | svg | (none) | 60 |
| `xvfb-conky` | svg/neon-dash-v2.svg (base) | xvfb (conky capture) | (none) | 15 |
| `startup` | svg/neon-dash-v2.svg | svg | dark-gradient.png | 2 (warmup=0, records from spawn) |

`xvfb-conky` is soft-skipped (not failed) in an `--all` sweep if `Xvfb`/`conky`
aren't installed — an explicit `scripts/profile.sh xvfb-conky` request without
them fails preflight instead. `startup` is the odd one out: `warmup=0` and it
additionally measures time-to-first-frame (see below) instead of just a
generic capture window.

### Preflight requirements

Checked up front with actionable messages, not assumed:

- **`dbus-run-session`** (dbus package) — hard requirement, every scenario
  needs a private D-Bus session bus.
- **`perf`** — hard requirement for CPU flamegraphs. Also checks
  `kernel.perf_event_paranoid <= 2` (`sudo sysctl kernel.perf_event_paranoid=2`
  if it's higher).
- **`inferno-collapse-perf` + `inferno-flamegraph`** (`cargo install inferno`)
  — hard requirement, turns `perf script` output into the flamegraph SVG.
- **`Xvfb` + `conky`** — soft requirement, only for the `xvfb-conky` scenario
  (see above).

### Env overrides

| var | default | meaning |
|---|---|---|
| `WARMUP_SECONDS` | 10 | settle time before measuring (fontdb load, seeding) — excluded from the captured window |
| `MEASURE_SECONDS` | 60 | measurement window length |
| `STARTUP_MEASURE_SECONDS` | 10 | measurement window for the `startup` scenario (which has no warmup) |
| `RSS_SAMPLE_INTERVAL` | 0.5 | seconds between `/proc/<pid>/status` VmRSS samples |
| `SHUTDOWN_TIMEOUT_SECONDS` | 15 | how long to wait for a clean SIGTERM exit before giving up on a scenario (never SIGKILL — dhat needs a clean exit to write its report) |

### Output

Per scenario, under `profiling-results/<scenario>/` (gitignored):

- `cpu_daemon.log` / `dhat_daemon.log` — full daemon stdout/stderr for each pass
- `cpu_metrics.txt` — `status=OK|ERROR`, `cpu_seconds`, `frames_sent`, `cpu_per_frame_ms`
- `dhat_metrics.txt` — `status=OK|ERROR`, `frames_sent`, `total_bytes_allocated`, `total_blocks_allocated`, `gmax_bytes`
- `rss_timeline.csv` — `elapsed_seconds,rss_kb` samples
- `perf.data` / `flamegraph.svg` — CPU profile and rendered flamegraph
- `dhat-heap.json` — full dhat allocation report (viewable at
  [dhat/dh_view.html](https://github.com/nnethercote/dhat-rs))
- `cpu_ttff.txt` / `dhat_ttff.txt` (`startup` scenario only) — `ttff_ms`,
  time from process spawn (`/proc/<pid>/stat` starttime) to the first frame
  actually being sent

A scenario that crashes or hangs mid-measurement gets `status=ERROR` (with a
reason) instead of a derived number — a mid-window daemon exit makes
`cpu_per_frame_ms` computation meaningless, so the harness refuses to
compute one rather than print something misleadingly precise. `--all` keeps
going past a single scenario's `ERROR`; it only stops on a preflight failure
or a signal.

`profiling-results/summary.md` is the machine-generated rollup: a metadata
block (commit hash, date, kernel, CPU model, build profile, transport) and
one markdown table row per scenario with `cpu/frame (ms)`, `frames sent`,
`avg/peak RSS (KB)`, `total allocated (bytes)`, and `time to first frame
(ms)` (the last is `n/a` outside the `startup` scenario). Refreshing the
baselines is "re-run the sweep and commit the diff" — see
`docs/profiling-baselines.md` for the current committed numbers.

### Heap profiling: dhat vs. alternatives

dhat (via the `dhat-heap` cargo feature) is the primary tool — pure Rust,
checked-in, deterministic output file, zero setup beyond
`cargo build --profile profiling --features dhat-heap`. It's what
`scripts/profile.sh` automates.

For deeper manual investigation, two alternatives are worth knowing about
(not integrated into the harness):

- **[heaptrack](https://github.com/KDE/heaptrack)** — `heaptrack
  target/profiling/thermalwriter daemon`, then `heaptrack_gui` on the
  resulting file. Better call-tree visualization than dhat's HTML viewer,
  higher overhead.
- **valgrind massif** — `valgrind --tool=massif target/profiling/thermalwriter
  daemon`, then `ms_print massif.out.<pid>`. Much higher overhead (10-50x
  slowdown), but useful for a second opinion when dhat's numbers look
  surprising, since it instruments at a completely different layer.
