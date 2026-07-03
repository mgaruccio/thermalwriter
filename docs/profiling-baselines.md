# Profiling baselines

Machine-generated numbers from `scripts/profile.sh --all`, committed as the
first measured baseline for the daemon's "lightweight" claim (previously
folklore: "14MB binary, 29MB RSS, 1% CPU at 2 FPS"). See `docs/profiling.md`
for how to reproduce this and how to read the columns.

**These numbers are machine-specific.** Absolute CPU-per-frame and RSS
depend on this CPU model, kernel, and the real sensor polling the daemon
does at startup/every tick (hwmon/sysinfo/nvidia/amdgpu/mangohud/rapl) — a
different machine, GPU, or sensor availability will produce different
absolute numbers even with an identical binary. Cross-machine regression
detection is the criterion micro-benches' job (`cargo bench`, per-stage,
saved/compared baselines under `target/criterion`); this document answers
"what does the whole daemon look like on the machine it was captured on,"
not "did this commit make things worse everywhere."

## Metadata

- **Commit**: `8b5fec8`
- **Date**: 2026-07-03T08:37:33Z (UTC)
- **Kernel**: 7.0.12-1-cachyos
- **CPU**: AMD Ryzen 9 9950X3D 16-Core Processor
- **GPU**: NVIDIA GeForce RTX 5080 (sensor polling active — `nvidia-smi` is
  invoked once per second by `NvidiaProvider` regardless of scenario)
- **RAM**: 60 GiB
- **Build profile**: `profiling` (CPU/RSS pass), `profiling` + `dhat-heap`
  (allocation pass) — see `[profile.profiling]` in `Cargo.toml`
- **Transport**: `NullTransport` (headless) — every scenario below is a
  fully headless run; no cooler was attached, no USB device was touched, and
  the live `thermalwriter.service` kept running untouched throughout (see
  `docs/profiling.md` for why this is safe)
- **Harness settings**: defaults (`WARMUP_SECONDS=10`, `MEASURE_SECONDS=60`,
  `STARTUP_MEASURE_SECONDS=10`, `RSS_SAMPLE_INTERVAL=0.5`,
  `SHUTDOWN_TIMEOUT_SECONDS=15`)
- **Result**: all 12 scenarios completed with `status=OK` — zero crashes,
  zero hangs, zero flamegraph-generation failures across the full sweep

**Superseded numbers note**: an earlier version of this document (commit
`ca74ec3`) understated every non-startup `cpu/frame` figure by ~15-18% —
`cpu_per_frame_ms`'s denominator (`frames_sent`) was the cumulative
whole-session frame count (including the 10s warmup) divided into a
numerator (`cpu_seconds`) that only covers the 60s measure window. Fixed in
`8b5fec8` (`frames_measure_estimate = frames_sent - tick_rate * warmup`,
used only for this ratio); see `docs/profiling.md`'s Output section for the
full formula and rationale. The numbers below are the corrected re-run.

## Results

| scenario | cpu/frame (ms) | frames sent | avg RSS (KB) | peak RSS (KB) | total allocated (bytes) | time to first frame (ms) |
|---|---|---|---|---|---|---|
| neon-dash-v2 | 10.496 | 141 | 49627 | 51964 | 847951237 | n/a |
| neon-dash | 10.413 | 141 | 49688 | 51956 | 836577589 | n/a |
| arc-gauge | 14.215 | 141 | 46971 | 49132 | 1248318535 | n/a |
| cyber-grid | 10.744 | 141 | 49795 | 50772 | 873396289 | n/a |
| system-stats | 5.455 | 141 | 54202 | 54204 | 657001039 | n/a |
| neon-dash-v2-bg-off | 9.256 | 141 | 42219 | 44580 | 850464381 | n/a |
| neon-dash-v2-15fps | 7.045 | 1040 | 48598 | 50772 | 4615935974 | n/a |
| neon-dash-v2-60fps | 6.639 | 3885 | 46327 | 48316 | 16284034208 | n/a |
| neon-dash-v2-bg-off-15fps | 6.866 | 1037 | 40980 | 43264 | 4617674876 | n/a |
| neon-dash-v2-bg-off-60fps | 6.548 | 3859 | 41655 | 43484 | 16389069420 | n/a |
| xvfb-conky | 3.631 | 1034 | 38585 | 38588 | 1707920487 | n/a |
| startup | 7.619 | 21 | 45361 | 45424 | 335670028 | 783.0 |

`total allocated (bytes)` is the dhat-heap build's lifetime total across the
whole measurement window (`dhat: Total:` from its drop-time summary), not a
steady-state figure — it scales with frames rendered (compare
`neon-dash-v2` at 141 frames / ~848 MB against `neon-dash-v2-60fps` at 3885
frames / ~16.3 GB: roughly linear with frame count, as expected for a
render pipeline that allocates fresh buffers per frame rather than reusing
them).

### Read-throughs worth calling out

- **`arc-gauge` is the most CPU-expensive built-in layout** (14.215 ms/frame
  vs. 10.4-10.7 ms/frame for the other SVG layouts) at the same 2 FPS /
  background-on config — worth a look if arc-gauge ever needs to run at a
  higher tick rate. `system-stats` (5.455 ms/frame) is cheaper still, but
  it's the legacy HTML `TemplateRenderer` path, not the SVG pipeline, so
  it's not a fair apples-to-apples comparison with the others.
- **Background compositing has a real, measurable cost**: `neon-dash-v2`
  (bg on, 10.496 ms/frame) vs. `neon-dash-v2-bg-off` (9.256 ms/frame) — a
  consistent ~1.2 ms/frame (~13%) for the background blit, matching
  intuition (compositing is strictly more work than not compositing). The
  criterion micro-benches (`cargo bench`) can isolate exactly how much of
  that is the `composite` stage itself vs. incidental variance.
- **RSS scales gently with tick rate, not frame count**: 2/15/60 FPS peak
  RSS is ~48-52 MB across the board (background on) — the daemon doesn't
  appear to leak or balloon at higher frame rates in a 60s window.
- **`xvfb-conky` has the lowest cpu/frame** (3.631 ms) — plausible, since
  Xvfb capture is a memory copy from a shared framebuffer rather than a full
  SVG parse+rasterize+composite pipeline per frame.
- **Startup (`ttff_ms=783.0`)**: ~0.78s from process spawn to the first
  frame actually being sent, dominated by fontdb's system font scan (the
  `fontdb_is_loaded_once_across_multiple_renderers` test already
  demonstrates the first `SvgRenderer::new()` call is the expensive one;
  subsequent renderer constructions are a cheap `Arc::clone`).
- **CPU flamegraphs correctly show only the daemon's own code** —
  `perf record --no-inherit` keeps `nvidia-smi` (forked once per second by
  `NvidiaProvider`, confirmed present and actively invoked on this machine)
  out of every flamegraph; spot-checked `neon-dash-v2/flamegraph.svg`
  directly for zero nvidia references despite ~70 nvidia-smi invocations
  during that scenario's warmup+measurement window.
- **The previously-reported spurious "flamegraph generation failed" on
  `xvfb-conky` did not reproduce** in this run either (second sweep in a
  row) — `status=OK`, a correctly-sized `flamegraph.svg`, and no
  `!! flamegraph generation failed` in the sweep log. Consistent with
  `--no-inherit` having eliminated the underlying cause (perf no longer
  follows/traces the Xvfb/conky children that were previously torn down
  mid-trace).

## Headless-vs-hardware cross-check

Done with explicit user go-ahead on 2026-07-03: `thermalwriter.service` was
confirmed active beforehand (PID 25349, running for 2 days), stopped by
`scripts/profile.sh --hardware neon-dash-v2` for the run's ~2m26s duration,
and confirmed restored afterward — clean SIGTERM shutdown, then a fresh
successful handshake (`Handshake OK: PM=4, SUB=5, resolution=480x480,
jpeg=true`) and tick loop restart, verified via `systemctl --user status`
and `journalctl --user -u thermalwriter`. No manual intervention was needed.

| | headless (`NullTransport`) | hardware (`BulkUsb`) | delta |
|---|---|---|---|
| cpu/frame (ms) | 10.496 | 10.579 | +0.083 ms (+0.8%) |
| frames sent | 141 | 141 | same (identical warmup/measure window) |
| avg RSS (KB) | 49627 | 55147 | +5520 KB (+11.1%) |
| peak RSS (KB) | 51964 | 55232 | +3268 KB (+6.3%) |
| total allocated (bytes) | 847951237 | 924815533 | +76864296 bytes (+9.1%) |

Same scenario (`neon-dash-v2`), same harness settings (`WARMUP_SECONDS=10`,
`MEASURE_SECONDS=60`, tick_rate=2) — only the transport differs, so this is
a like-for-like comparison of what `BulkUsb` costs on top of the
NullTransport baseline.

**Findings:**

- **RSS/allocations are consistently ~6-11% higher on real hardware.**
  Plausible attribution: `rusb`'s `DeviceHandle`, USB context, and endpoint
  descriptor bookkeeping, plus each `send_frame` building a fresh
  `Vec::with_capacity(64 + data.len())` header+payload frame and doing
  chunked `write_bulk` calls — none of which `NullTransport::send_frame`
  does (it's a no-op past the optional artificial-latency sleep). This is
  the real, expected shape of "USB transport has real costs NullTransport
  doesn't model" that headless profiling can't see.
- **cpu/frame is only ~0.8% higher** — much smaller than expected going in.
  Two honest caveats on this one: (1) this is a single-sample measurement
  per transport, not a statistically repeated one (that's what the
  criterion benches are for), so a sub-1%, same-order-of-magnitude
  difference is within plausible run-to-run noise and shouldn't be read as
  "USB writes are free"; (2) I looked for the USB bulk-write cost as a
  distinct region in the hardware flamegraph and couldn't isolate it — 82%
  of sampled stack frames in `neon-dash-v2/flamegraph.svg` (hardware run)
  resolve to `[unknown]`, most likely because the system's `libusb`
  (dynamically linked, called via `rusb`'s FFI) lacks the debug symbols
  `perf --call-graph dwarf` needs to unwind through it cleanly. The
  bulk-write cost is real (it shows up in the RSS/allocation deltas above)
  but this flamegraph capture can't visually attribute CPU time to it
  specifically — a caveat for anyone using these flamegraphs to hunt for
  USB-specific hot spots, not a claim that the cost doesn't exist.
- **No reconnect/retry overhead observed** — the daemon claimed the device
  and completed its handshake on the very first attempt in both the cpu and
  dhat passes (no `USB reconnect` or `USB display unavailable` log lines),
  because `scripts/profile.sh`'s `stop_live_service_for_hardware` had
  already cleanly released the interface (`BulkUSB device closed` in the
  live service's shutdown log) before our test daemon ever started. The
  2-second settle sleep plus the daemon's own retry logic were not needed
  on this run, but remain in place for a slower-releasing device or a
  flakier reconnect scenario.
- **`BulkUsb`'s frame counter (`18a3c90`) worked correctly on its first real
  exercise**: `BulkUsb closed: 141 frames sent` (cpu pass) / `135 frames
  sent` (dhat pass) in the daemon logs — `frames_sent` and `cpu_per_frame_ms`
  are real numbers above, not `n/a`, closing out the gap that motivated
  adding the counter in the first place.
