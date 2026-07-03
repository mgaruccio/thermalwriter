# Brainstorm: Comprehensive CPU + Memory Profiling for the Daemon

**Date:** 2026-07-03
**Status:** Decided — ready for planning
**Decision:** Whole-daemon scenario harness + criterion micro-benches for the hottest stages, headless-first via a null transport, with a real-hardware mode.

## Why

Staying lightweight is the project's headline feature — the whole point of thermalwriter is replacing a 400 MB Python/Qt app with a 14 MB binary at 29 MB RSS and ~1% CPU. Today those numbers are folklore: there is no tooling to measure where CPU cycles or memory actually go, no baselines to compare against after a change, and no way to notice a slow regression. The only performance tooling in the repo is the USB throughput `bench` command, which measures the wire, not the daemon.

The deliverable is **repeatable profiling infrastructure checked into the repo** — not a one-time report, and not (yet) CI gates.

## What We're Building

### 1. Null transport — the enabling primitive

A way to run the *full* daemon (tokio runtime, D-Bus service, sensor polling, render pipeline, JPEG encode, tick loop) without the cooler attached. The `Transport` trait (`src/transport/mod.rs:19`) is a single-method seam (`send_frame`), so a null implementation that discards frames (optionally simulating device latency) slots in naturally. This also benefits contributors who don't own the hardware.

### 2. Scenario-driven whole-daemon profiling harness

A checked-in entry point (script or cargo alias) that runs the daemon through a defined scenario matrix for a fixed duration and captures profiles:

- **Scenarios:** each built-in SVG layout (neon-dash-v2, neon-dash, arc-gauge, cyber-grid), a representative HTML layout, xvfb stream mode, background compositing on/off, tick rates 2 / 15 / 60 FPS, plus idle/startup.
- **CPU:** flamegraphs of the live daemon (perf-based), with debug symbols available in the profiled build.
- **Memory:** allocation profiling via a feature-gated dhat build (never in the default build — the profiling machinery must not make the shipped daemon heavier), plus an RSS-over-time sample from `/proc` to catch steady-state growth and leaks that allocation snapshots miss.
- **Output:** artifacts per scenario plus a summary table of baseline numbers (CPU per frame, steady-state RSS, allocations per tick) recorded in `docs/` so future changes have something concrete to diff against.

### 3. Hot-stage criterion micro-benches

Statistical, saved-baseline benches for the stages the flamegraphs are expected to show as dominant:

- SVG render (per built-in layout, resvg + Tera context build)
- JPEG encode (480×480, quality 85, plus a quality sweep)
- Frame rotation and background blit

These give precise before/after diffs (`cargo bench` against a saved baseline) that flamegraphs can't, and are the natural stepping stone if CI regression gates are wanted later.

### 4. Real-hardware mode

The same harness pointed at the live USB device for ground truth — the null-transport numbers are only trustworthy if periodically validated against reality (per project practice: hardware-verify before claiming milestones).

## Approaches Considered

| Approach | Verdict |
|---|---|
| **A+B: Whole-daemon harness + hot-stage benches** | **Chosen.** Captures whole-system truth (tokio, D-Bus, locks, steady-state RSS) *and* precise per-stage regression numbers. Cost: two layers to keep honest. |
| A only: Whole-daemon harness | Fully representative, one layer — but per-stage attribution means reading flamegraphs by eye; no statistical diffs. |
| B only: Per-stage micro-benches | Precise and CI-friendly, but blind to async runtime overhead, lock contention, and steady-state RSS — the things most likely to erode "lightweight" silently. |
| C: Built-in self-instrumentation (per-tick phase timings, alloc counters via GetStatus) | Rejected as the primary approach: no call-stack attribution, and permanent code in the daemon cuts against the lightweight ethos. A thin slice (per-tick phase timing at debug log level) may be worth folding in later — the tick loop already measures elapsed time. |

## Key Decisions

1. **Repeatable harness, not a one-off report** — though the first run of the harness should produce the initial baseline document.
2. **Headless-first, hardware-validated** — null transport makes profiling runnable anywhere; real-device runs keep it honest.
3. **Profiling machinery must not fatten the product** — dhat/debug-symbol builds are feature-gated or separate cargo profiles; the default `cargo build` output is unchanged.
4. **CI regression gates are explicitly deferred** — the criterion baselines make them cheap to add later, but they are not part of this work.

## Success Criteria

- One command profiles any scenario headlessly and emits CPU flamegraph + allocation profile + RSS timeline.
- `cargo bench` produces per-stage numbers diffable against saved baselines.
- A baseline document exists with measured numbers for every scenario (replacing the folklore "29 MB RSS, 1% CPU").
- Default build artifacts (binary size, deps) are unchanged by the profiling additions.
- At least one headless baseline is cross-checked against a real-hardware run.

## Open Questions (for planning)

- Harness form: shell script vs. cargo xtask vs. `thermalwriter profile` subcommand (a subcommand adds code to the shipped binary — likely disqualifying given decision 3).
- Whether the null transport is a CLI flag, config value, or compile-time feature — and whether it should simulate USB latency to keep tick-loop timing realistic.
- dhat vs. heaptrack vs. valgrind massif as the primary memory tool (dhat is the checked-in-friendly option; the others are worth documenting as manual alternatives).
- Whether xvfb-stream scenarios can run in fully headless environments (they need Xvfb installed) or are marked local-only.
- Whether to include binary-size tracking (cargo-bloat) in the harness — cheap to add, aligned with the lightweight goal.
