# Performance tuning and profiling

This is the working README for keeping `thermalwriter` lightweight. It covers
the two profiling layers in this repo, how to run them safely, what each output
means, and the repeatable autoresearch loop to use when chasing a regression.

> **Layout-engine backend gate: PASS (resvg).** The flagship `neon-composer` document passed the pixel-scaling and default display-tick gates with `cargo bench --bench render -- --quick layout_document_scaling`; the run measured 480×480 **1.143 ms / 4.962 ms/MP**, 480×1280 **1.343 ms / 2.186 ms/MP**, 1280×480 **1.338 ms / 2.178 ms/MP**, and 2400×1080 curved **5.432 ms / 2.096 ms/MP**. Each result stayed below the 500 ms (2 FPS) default tick deadline, and no direct tiny-skia backend switch is required. These are host-specific gate measurements, not an automatic refresh of the human interpretation baselines.

Use the two layers for different questions:

| Tool | Command | Answers | Best for |
|---|---|---|---|
| Whole-daemon scenario harness | `scripts/profile.sh ...` | "What does the real daemon cost on this machine?" | CPU flamegraphs, RSS, allocations, startup time, transport comparisons |
| Criterion micro-benches | `cargo bench` | "Did this hot stage get faster or slower?" | Statistical before/after checks for render/tick internals |

Do not treat the whole-daemon harness as a statistical benchmark. It includes
the async runtime, D-Bus, sensor polling, renderer setup, logging, transport
behavior, and host-specific kernel/tooling effects. That is exactly why it is
useful for end-to-end research, but it also means the absolute numbers are
machine-specific. Use Criterion for tight regression comparisons.

## Quick start

Safe discovery:

```bash
scripts/profile.sh --list
scripts/profile.sh --help
```

Short headless smoke run:

```bash
WARMUP_SECONDS=2 MEASURE_SECONDS=5 STARTUP_MEASURE_SECONDS=5 scripts/profile.sh neon-dash-v2
```

Normal single-scenario run:

```bash
scripts/profile.sh neon-dash-v2
```

Curated headless sweep:

```bash
scripts/profile.sh --all
```

Per-stage statistical benches:

```bash
cargo bench
```

Before/after bench workflow:

```bash
cargo bench -- --save-baseline before
# make the change
cargo bench -- --baseline before
```

## Safety model

Default `scripts/profile.sh` runs are headless:

- set `THERMALWRITER_TRANSPORT=null`
- use `NullTransport`
- run the daemon under `dbus-run-session`
- use isolated scratch `XDG_CONFIG_HOME` and `XDG_RUNTIME_DIR`
- do not touch USB hardware
- do not stop the live `thermalwriter.service`
- write artifacts under gitignored `profiling-results/`

Hardware runs are explicit and disruptive:

```bash
scripts/profile.sh --hardware neon-dash-v2
```

`--hardware` leaves `THERMALWRITER_TRANSPORT` unset so the daemon uses the real
`BulkUsb` path. It stops `thermalwriter.service` for the duration, restores it
afterward only if it was active before the run, and briefly takes over the LCD.
Never use `--hardware` if something else needs the cooler display right now.

The script also protects the host while it runs:

- `.profiling-harness.lock` prevents concurrent harness runs from confusing PID
  detection.
- `INT`, `TERM`, and `EXIT` traps clean up the RSS sampler, daemon process,
  `dbus-run-session` wrapper, scratch dirs, lock dir, and hardware service.
- Hardware service restoration runs first during cleanup because it is the only
  user-visible side effect.
- Daemons are terminated with SIGTERM, not SIGKILL, so `dhat` can flush its
  allocation report.
- Hung or crashed scenarios are marked `status=ERROR`; the harness refuses to
  print misleading derived metrics.
- `perf record --no-inherit` keeps forked sensor/tool children such as
  `nvidia-smi`, `Xvfb`, or `conky` out of daemon flamegraphs.

## Preflight requirements

The scenario harness checks these before profiling:

| Requirement | Purpose | Install hint |
|---|---|---|
| `dbus-run-session` | private D-Bus session bus for each daemon run | distro `dbus` package |
| `perf` | CPU samples and call stacks | distro perf/linux-tools package |
| `kernel.perf_event_paranoid <= 2` | permission for `perf record -p` | `sudo sysctl kernel.perf_event_paranoid=2` |
| `inferno-collapse-perf` | collapse `perf script` stacks | `cargo install inferno` |
| `inferno-flamegraph` | render `flamegraph.svg` | `cargo install inferno` |
| `Xvfb` + `conky` | `xvfb-conky` scenario only | distro packages |

`Xvfb`/`conky` are a soft requirement for `--all`: missing tools skip the
`xvfb-conky` scenario. They are a hard requirement when `xvfb-conky` is
requested explicitly.

Useful local preflight check:

```bash
command -v dbus-run-session perf inferno-collapse-perf inferno-flamegraph cargo rustc
cat /proc/sys/kernel/perf_event_paranoid
```

## Scenario matrix

`scripts/profile.sh --list` prints the supported scenarios:

| scenario | layout | mode | background | tick rate |
|---|---|---|---|---|
| `neon-dash-v2` | `svg/neon-dash-v2.svg` | SVG | `dark-gradient.png` | 2 |
| `neon-dash` | `svg/neon-dash.svg` | SVG | `dark-gradient.png` | 2 |
| `arc-gauge` | `svg/arc-gauge.svg` | SVG | `dark-gradient.png` | 2 |
| `cyber-grid` | `svg/cyber-grid.svg` | SVG | `dark-gradient.png` | 2 |
| `system-stats` | `system-stats.html` | HTML template | `dark-gradient.png` | 2 |
| `neon-dash-v2-bg-off` | `svg/neon-dash-v2.svg` | SVG | none | 2 |
| `neon-dash-v2-15fps` | `svg/neon-dash-v2.svg` | SVG | `dark-gradient.png` | 15 |
| `neon-dash-v2-60fps` | `svg/neon-dash-v2.svg` | SVG | `dark-gradient.png` | 60 |
| `neon-dash-v2-bg-off-15fps` | `svg/neon-dash-v2.svg` | SVG | none | 15 |
| `neon-dash-v2-bg-off-60fps` | `svg/neon-dash-v2.svg` | SVG | none | 60 |
| `xvfb-conky` | `svg/neon-dash-v2.svg` base | Xvfb capture | none | 15 |
| `startup` | `svg/neon-dash-v2.svg` | SVG | `dark-gradient.png` | 2 |

`startup` is special: it has no warmup window and records time-to-first-frame.

## Environment overrides

| Variable | Default | Meaning |
|---|---:|---|
| `WARMUP_SECONDS` | `10` | settle time before measurement; excluded from steady-state CPU/RSS metrics |
| `MEASURE_SECONDS` | `60` | normal scenario measurement window |
| `STARTUP_MEASURE_SECONDS` | `10` | measurement window for `startup` |
| `RSS_SAMPLE_INTERVAL` | `0.5` | seconds between `/proc/<pid>/status` RSS samples |
| `SHUTDOWN_TIMEOUT_SECONDS` | `15` | clean SIGTERM wait before marking a scenario unreliable |
| `THERMALWRITER_NULL_LATENCY_MS` | unset | optional artificial per-frame delay in `NullTransport::send_frame` |

Use short windows while researching hypotheses. Use defaults when refreshing
committed baselines.

## What one scenario captures

Each scenario runs two separate daemon processes.

### CPU/RSS pass

Build:

```bash
cargo build --profile profiling
```

Run:

1. Generate scratch config for the scenario.
2. Start the daemon under `dbus-run-session`.
3. Sleep the warmup window, except for `startup`.
4. Read daemon CPU ticks from `/proc/<pid>/stat`.
5. Sample RSS from `/proc/<pid>/status`.
6. Attach `perf record --call-graph dwarf --no-inherit`.
7. SIGTERM the daemon.
8. Verify clean shutdown.
9. Parse final transport frame count.
10. Convert `perf.data` to `flamegraph.svg`.

### Allocation pass

Build:

```bash
cargo build --profile profiling --features dhat-heap
```

Run:

1. Start a fresh daemon process with the same scenario config.
2. Let `dhat`'s global allocator record heap activity.
3. SIGTERM the daemon so `dhat::Profiler` drops cleanly.
4. Move `dhat-heap.json` out of the scratch dir.
5. Parse dhat's stderr summary into flat metrics.

The two-pass split is intentional: swapping in `dhat`'s global allocator changes
allocation behavior, so CPU/RSS and allocation profiles must not be mixed.

## Output files

Per scenario:

```text
profiling-results/<scenario>/
  cpu_daemon.log
  dhat_daemon.log
  cpu_metrics.txt
  dhat_metrics.txt
  rss_timeline.csv
  perf.data
  flamegraph.svg
  dhat-heap.json
  cpu_ttff.txt      # startup only
  dhat_ttff.txt     # startup only
```

Rollup:

```text
profiling-results/summary.md
```

Committed baseline:

```text
docs/profiling-baselines.md
```

`dhat-heap.json` can be opened with
[dhat/dh_view.html](https://github.com/nnethercote/dhat-rs).

## Metric definitions

`cpu_metrics.txt` contains:

- `status=OK|ERROR`
- `cpu_seconds`
- `frames_sent`
- `frames_measure_estimate`
- `cpu_per_frame_ms`
- `device_never_claimed` in hardware mode

`frames_sent` is cumulative since daemon spawn because it comes from the final
transport shutdown log. `cpu_seconds` covers only the post-warmup measurement
window. The harness therefore corrects the denominator:

```text
frames_measure_estimate = frames_sent - tick_rate * warmup
cpu_per_frame_ms = cpu_seconds * 1000 / frames_measure_estimate
```

This is an estimate. It assumes the warmup window hit roughly `tick_rate *
warmup` frames, which is good enough for whole-daemon sanity checks. Use
Criterion if the question needs tighter precision.

`dhat_metrics.txt` contains lifetime allocation totals for the allocation pass.
Those totals scale with duration and frame count. They are not steady-state RSS.

`rss_timeline.csv` is the steady-state memory source for average and peak RSS.

`startup` writes `ttff_ms`, measured from the daemon process start time in
`/proc/<pid>/stat` to the first `NullTransport: first frame sent` log marker.

## NullTransport

`NullTransport` lives in `src/transport/null.rs` and is selected only by:

```text
THERMALWRITER_TRANSPORT=null
```

Behavior:

- exact, case-sensitive match; anything else defaults to USB
- synthetic 480x480 JPEG-capable handshake
- frame bytes are discarded
- frame count is retained for metrics
- first frame logs `NullTransport: first frame sent`
- shutdown logs `NullTransport closed: N frames sent`
- optional artificial latency via `THERMALWRITER_NULL_LATENCY_MS`

This lets the full daemon run headlessly while still exercising D-Bus, sensor
polling, rendering, JPEG encoding, tick scheduling, and shutdown.

## Cargo profiling setup

`Cargo.toml` contains:

```toml
dhat = { version = "0.3", optional = true }

[features]
dhat-heap = ["dep:dhat"]

[profile.profiling]
inherits = "release"
debug = true
strip = false
```

The default shipped build does not include `dhat`, does not use the profiling
profile, and does not expose a profiling subcommand.

`src/main.rs` installs the `dhat` allocator only under `dhat-heap`, and creates
the profiler only for real daemon runs after CLI dispatch. Non-daemon commands
such as `bench`, `ctl`, and `setup-udev` do not instantiate the profiler.

## Criterion micro-benches

`benches/pipeline.rs` covers tick-loop hot paths:

- `rotate_pixels` at 0/90/180/270 degrees
- `encode_jpeg` at quality 85, rotation 180
- JPEG quality sweep at 10/50/85/100

The input is a deterministic 480x480 RGB gradient so JPEG entropy work is
representative.

`benches/render.rs` covers SVG render hot paths:

- full render for `neon-dash-v2`, `neon-dash`, `arc-gauge`, `cyber-grid`
- `build_context`
- `render_template`
- `parse_svg`
- `rasterize`
- `composite`
- background decode for 480x480 and 1920x1080 PNG inputs
- `RawFrame::from_pixmap`

Render benches use shared mock sensors and synthetic history so layouts with
history frontmatter look like the daemon/preview path.

## Autoresearch loop

Use this loop when tuning performance. Start only from a clean working tree so
each measurement can be tied to exactly one code state.

0. **Confirm the repo is clean.**

   ```bash
   git status --short
   ```

   The command must print nothing. Commit, stash, or revert unrelated work
   before profiling; otherwise the results are not attributable.

1. **Pick the smallest representative scenario.**
   - Render pipeline: `neon-dash-v2`
   - Background compositing: compare `neon-dash-v2` vs `neon-dash-v2-bg-off`
   - Layout-specific cost: target that layout scenario
   - Startup cost: `startup`
   - Transport cost: headless first, then `--hardware` only with explicit intent

2. **Run a short headless probe.**

   ```bash
   WARMUP_SECONDS=2 MEASURE_SECONDS=5 STARTUP_MEASURE_SECONDS=5 scripts/profile.sh neon-dash-v2
   ```

3. **Read the artifacts.**
   - `profiling-results/summary.md` for the headline row
   - `profiling-results/<scenario>/cpu_metrics.txt` for raw CPU/frame inputs
   - `profiling-results/<scenario>/rss_timeline.csv` for memory shape
   - `profiling-results/<scenario>/flamegraph.svg` for where CPU samples land
   - `profiling-results/<scenario>/dhat_metrics.txt` and `dhat-heap.json` for allocation pressure

4. **Turn the flamegraph into a narrow hypothesis.**
   - If the stack is render-heavy, move to `cargo bench --bench render`.
   - If JPEG/rotation dominates, move to `cargo bench --bench pipeline`.
   - If RSS grows over time, inspect allocation profile and long-lived owners.
   - If startup dominates, separate font/config/layout seeding from the tick loop.

5. **Save a Criterion baseline before changing code.**

   ```bash
   cargo bench --bench render -- --save-baseline before
   # or:
   cargo bench --bench pipeline -- --save-baseline before
   ```

6. **Make one change at the source.**
   Avoid broad refactors during measurement. One hypothesis, one change.

7. **Compare Criterion first.**

   ```bash
   cargo bench --bench render -- --baseline before
   # or:
   cargo bench --bench pipeline -- --baseline before
   ```

8. **Re-run the same short scenario.**

   ```bash
   WARMUP_SECONDS=2 MEASURE_SECONDS=5 STARTUP_MEASURE_SECONDS=5 scripts/profile.sh neon-dash-v2
   ```

9. **Promote only if both layers agree.**
   Criterion should show the local stage improved or stayed neutral. The
   scenario harness should show the whole daemon did not regress unexpectedly.

10. **Refresh committed baselines only after the tuning is done.**

    ```bash
    scripts/profile.sh --all
    ```

    Use `profiling-results/summary.md` as the machine-generated input, then
    manually refresh `docs/profiling-baselines.md` while preserving human-written
    caveats, metadata, interpretation notes, and hardware cross-check context.

## Reading current baselines

`docs/profiling-baselines.md` is the committed reference run. At the time of
writing, its key findings are:

- all 12 headless scenarios completed with `status=OK`
- `arc-gauge` is the most expensive built-in SVG layout
- background compositing costs about 1.2 ms/frame on `neon-dash-v2`
- RSS stays roughly stable across the 2/15/60 FPS runs
- startup time-to-first-frame is dominated by first fontdb system scan
- the real hardware cross-check shows modest extra RSS/allocation cost over
  `NullTransport`

Those numbers are host-specific. Use them as local context for this machine,
not as universal performance budgets.

## Common interpretation mistakes

- Do not compare dhat total bytes across different durations or frame counts as
  if they were steady-state memory.
- Do not read a hardware flamegraph with many `[unknown]` frames as proof that
  USB has no CPU cost; unresolved dynamic libraries can hide call stacks.
- Do not compare short smoke-run numbers to committed default-window baselines
  as a regression verdict.
- Do not use `--hardware` to validate normal render changes unless the transport
  itself is part of the hypothesis.
- Do not optimize for a single scenario if the change hurts the Criterion stage
  or another shipped layout.

## Manual heap alternatives

`dhat` is the checked-in allocation profiler. For deeper manual investigation:

- **heaptrack**:

  ```bash
  heaptrack target/profiling/thermalwriter daemon
  heaptrack_gui heaptrack.*.gz
  ```

- **valgrind massif**:

  ```bash
  valgrind --tool=massif target/profiling/thermalwriter daemon
  ms_print massif.out.<pid>
  ```

Both have higher overhead than the built-in dhat pass. Use them as second
opinions when dhat output is surprising.
