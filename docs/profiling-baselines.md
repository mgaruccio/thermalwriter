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

- **Commit**: `18a3c90`
- **Date**: 2026-07-03T07:46:07Z (UTC)
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

## Results

| scenario | cpu/frame (ms) | frames sent | avg RSS (KB) | peak RSS (KB) | total allocated (bytes) | time to first frame (ms) |
|---|---|---|---|---|---|---|
| neon-dash-v2 | 6.879 | 141 | 48490 | 50836 | 859240310 | n/a |
| neon-dash | 7.092 | 141 | 46916 | 49212 | 844551028 | n/a |
| arc-gauge | 13.191 | 141 | 47156 | 49296 | 1256640332 | n/a |
| cyber-grid | 9.362 | 141 | 46907 | 47748 | 877842761 | n/a |
| system-stats | 4.539 | 141 | 54378 | 54380 | 659350314 | n/a |
| neon-dash-v2-bg-off | 9.220 | 141 | 42255 | 42680 | 849395516 | n/a |
| neon-dash-v2-15fps | 5.857 | 1033 | 47141 | 49332 | 4635184023 | n/a |
| neon-dash-v2-60fps | 5.628 | 3859 | 47718 | 49492 | 16377015974 | n/a |
| neon-dash-v2-bg-off-15fps | 5.874 | 1035 | 42579 | 44784 | 4665450029 | n/a |
| neon-dash-v2-bg-off-60fps | 5.654 | 3838 | 42795 | 44696 | 16585966015 | n/a |
| xvfb-conky | 3.091 | 1032 | 40171 | 40172 | 1721506313 | n/a |
| startup | 7.619 | 21 | 46728 | 48932 | 366410308 | 731.5 |

`total allocated (bytes)` is the dhat-heap build's lifetime total across the
whole measurement window (`dhat: Total:` from its drop-time summary), not a
steady-state figure — it scales with frames rendered (compare
`neon-dash-v2` at 141 frames / ~859 MB against `neon-dash-v2-60fps` at 3859
frames / ~16.4 GB: roughly linear with frame count, as expected for a
render pipeline that allocates fresh buffers per frame rather than reusing
them).

### Read-throughs worth calling out

- **`arc-gauge` is the most CPU-expensive built-in layout** (13.2 ms/frame
  vs. 6.9-9.4 ms/frame for the others) at the same 2 FPS / background-on
  config — worth a look if arc-gauge ever needs to run at a higher tick
  rate.
- **Background compositing costs relatively little**: `neon-dash-v2`
  (bg on) vs. `neon-dash-v2-bg-off` is 6.879 ms vs. 9.220 ms/frame — the
  "off" run is actually *higher*, which is more likely run-to-run sensor
  polling / scheduler noise than compositing overhead, given the criterion
  micro-benches (`cargo bench`) show `composite` as a small fraction of
  total render time. Machine-specific, single-sample harness numbers like
  these are exactly why criterion (statistically repeated, per-stage) is
  the tool for anything sensitive to noise at this scale — see
  `docs/profiling.md`.
- **RSS scales gently with tick rate, not frame count**: 2/15/60 FPS peak
  RSS is ~49-50 MB across the board (background on) — the daemon doesn't
  appear to leak or balloon at higher frame rates in a 60s window.
- **`xvfb-conky` has the lowest cpu/frame** (3.091 ms) — plausible, since
  Xvfb capture is a memory copy from a shared framebuffer rather than a full
  SVG parse+rasterize+composite pipeline per frame.
- **Startup (`ttff_ms=731.5`)**: ~0.73s from process spawn to the first
  frame actually being sent, dominated by fontdb's system font scan (the
  `fontdb_is_loaded_once_across_multiple_renderers` test already
  demonstrates the first `SvgRenderer::new()` call is the expensive one;
  subsequent renderer constructions are a cheap `Arc::clone`).
- **CPU flamegraphs correctly show only the daemon's own code** —
  `perf record --no-inherit` (added alongside this sweep) keeps
  `nvidia-smi` (forked once per second by `NvidiaProvider`, confirmed
  present and actively invoked on this machine) out of every flamegraph;
  spot-checked `neon-dash-v2/flamegraph.svg` directly for zero nvidia
  references despite ~70 nvidia-smi invocations during that scenario's
  warmup+measurement window.
- **The previously-reported spurious "flamegraph generation failed" on
  `xvfb-conky` did not reproduce** in this run — `status=OK`, a
  correctly-sized `flamegraph.svg` (35930 bytes, in line with the other
  scenarios), and no `!! flamegraph generation failed` in the sweep log.
  Consistent with `--no-inherit` having eliminated the underlying cause
  (perf no longer follows/traces the Xvfb/conky children that were
  previously torn down mid-trace).

## TODO: headless-vs-hardware cross-check

Not done in this sweep — running `scripts/profile.sh --hardware <scenario>`
stops the live `thermalwriter.service` for its duration, which needs
explicit user go/no-go before it's run for real (see `docs/profiling.md`'s
`--hardware` warning). The plan's acceptance criteria call for at least one
scenario cross-checked headless vs. hardware, with deltas noted (expected:
USB send/handshake cost, `try_reconnect` polling if the device is slow to
claim, and now real `frames_sent`/`cpu_per_frame_ms` numbers instead of
`n/a`, since `BulkUsb` gained a frame counter alongside this sweep).

- [ ] Get user go/no-go to run `scripts/profile.sh --hardware neon-dash-v2`
      (or another representative scenario)
- [ ] Compare against the headless `neon-dash-v2` row above: cpu/frame,
      frames sent, RSS, and note anything USB-transport-specific (fatal
      error handling / reconnect polling overhead, `BulkUsb::send_frame`'s
      per-frame chunked-write cost vs. `NullTransport`'s no-op)
- [ ] Append the hardware row + delta notes to this document
