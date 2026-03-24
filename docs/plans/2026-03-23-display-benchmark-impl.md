# Display Benchmark Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use forge:executing-plans to implement this plan task-by-task.

**Goal:** Add a `thermalwriter bench [--duration <secs>]` CLI subcommand that measures the maximum frame rate the LCD display hardware will accept over USB.

**Architecture:** Pre-render two solid-color JPEG frames (red/blue), open USB device, blast alternating frames in a tight synchronous loop for the given duration, then report throughput statistics. No async runtime needed for the benchmark itself — USB writes are blocking.

**Tech Stack:** clap (CLI), rusb (USB), image (JPEG encoding), tiny-skia (Pixmap creation), std::time (timing)

**Required Skills:**
- `forge:writing-tests`: Invoke before writing test code — ensures TDD discipline

## Context for Executor

### Key Files
- `src/cli.rs:1-173` — CLI definitions. `Command` enum (line 14), `CtlCommand` enum (line 24), `run_ctl()` (line 59), CLI tests (line 111). Add `Bench` variant and `run_bench()` here.
- `src/main.rs:31-37` — Command dispatch. Add `Command::Bench` match arm here.
- `src/transport/mod.rs:1-27` — `Transport` trait (line 19) with `handshake()`, `send_frame()`, `close()`. `DeviceInfo` struct (line 8).
- `src/transport/bulk_usb.rs:1-206` — `BulkUsb::new()` (line 69), `handshake()` (line 122), `send_frame()` (line 162). This is the real USB path.
- `src/service/tick.rs:67-96` — `encode_jpeg(pixmap, quality, rotation)` function. Handles premultiplied RGBA → straight RGB → JPEG.
- `src/config.rs` — `Config::load(path)`, `Config::default_path()`. `DisplayConfig` has `jpeg_quality: u8` (default 85) and `rotation: u16` (default 180).

### Research Findings
- `send_frame()` is synchronous (blocking USB bulk writes with 5s timeout, 16 KiB chunks)
- The tight loop needs no async runtime — just `BulkUsb::new()`, `handshake()`, then loop `send_frame()`
- `encode_jpeg()` is a public function in `crate::service::tick` — can be called directly
- A solid-color 480x480 JPEG at quality 85 is roughly 3-5 KB (very compressible)
- `tiny_skia::Pixmap::new()` creates a transparent black pixmap; fill with `pixmap.fill(Color::from_rgba8(r, g, b, 255))`
- The `Command` enum uses clap `Subcommand` derive — adding a variant is one line plus doc comment
- CLI tests pattern: `Cli::try_parse_from(["thermalwriter", "bench"]).unwrap()`

### Relevant Patterns
- `src/cli.rs:59-109` — `run_ctl()` is the pattern for a subcommand handler function
- `src/main.rs:32-37` — Command dispatch pattern to follow
- `src/cli.rs:117-173` — CLI parse tests to extend

## Execution Architecture

**Team:** 1 dev, 1 spec reviewer, 1 quality reviewer
**Task dependencies:**
  - Tasks 1 and 4 are independent (CLI wiring vs bench logic)
  - Task 7 (integration) depends on both Tasks 1 and 4
**Phases:**
  - Phase 1: Tasks 1-6 (CLI wiring + benchmark implementation)
  - Phase 2: Task 7 (wire everything together in main.rs)
**Milestones:**
  - After Task 3 (CLI structure complete)
  - After Task 6 (bench logic complete)
  - After Task 9 (final — everything wired and tested)

---

### Task 1: Add `Bench` variant to CLI [DO-CONFIRM]

**Files:**
- Modify: `src/cli.rs:13-22` (Command enum)
- Modify: `src/cli.rs:1` (add imports if needed)

**Implement:** Add a `Bench` variant to the `Command` enum with an optional `--duration` flag. No subcommand nesting needed — this is a simple command with one optional arg.

```rust
/// Benchmark display USB throughput.
Bench {
    /// Duration in seconds (default: 10).
    #[arg(long, default_value_t = 10)]
    duration: u64,
},
```

**Confirm checklist:**
- [ ] `Bench` variant added to `Command` enum in `src/cli.rs`
- [ ] `--duration` has `default_value_t = 10` and type `u64`
- [ ] `cargo check` passes
- [ ] Committed

### Task 2: Review Task 1

**Trigger:** Both reviewers start when Task 1 completes.

**Killer items (blocking):**
- [ ] `Command::Bench { duration }` variant exists in `src/cli.rs` `Command` enum
- [ ] `duration` field has type `u64` with `default_value_t = 10`
- [ ] `#[arg(long)]` attribute present on `duration` field
- [ ] `cargo check` passes with no errors or warnings related to the new variant

**Quality items (non-blocking):**
- [ ] Doc comment on `Bench` variant describes what it does
- [ ] No unnecessary imports added

### Task 3: Milestone — CLI structure complete

**Present to user:**
- `Bench` variant added to `Command` enum with `--duration` flag
- Parsing verified via `cargo check`

**Wait for user response before proceeding to Task 4.**

---

### Task 4: Implement `run_bench()` function [READ-DO]

**Files:**
- Modify: `src/cli.rs` (add `run_bench()` function and required imports)

**Step 1: Add imports at top of `src/cli.rs`**

Add these to the existing imports:

```rust
use std::time::{Duration, Instant};
use tiny_skia::{Color, Pixmap};
use crate::config::Config;
use crate::service::tick::encode_jpeg;
use crate::transport::Transport;
use crate::transport::bulk_usb::BulkUsb;
```

**Step 2: Write the `run_bench()` function**

Add after `run_ctl()` (after line 109):

```rust
/// Run the USB throughput benchmark.
pub fn run_bench(duration_secs: u64) -> Result<()> {
    let config = Config::load(&Config::default_path())?;
    let quality = config.display.jpeg_quality;
    let rotation = config.display.rotation;

    // Pre-render two solid-color frames (red and blue) for visual confirmation
    let mut pixmap_red = Pixmap::new(480, 480).context("Failed to create pixmap")?;
    pixmap_red.fill(Color::from_rgba8(255, 0, 0, 255));
    let jpeg_red = encode_jpeg(&pixmap_red, quality, rotation)?;

    let mut pixmap_blue = Pixmap::new(480, 480).context("Failed to create pixmap")?;
    pixmap_blue.fill(Color::from_rgba8(0, 0, 255, 255));
    let jpeg_blue = encode_jpeg(&pixmap_blue, quality, rotation)?;

    // Open USB device and handshake
    let mut transport = BulkUsb::new()?;
    let info = transport.handshake()?;

    println!("Benchmarking display throughput...");
    println!("  Device: {}x{}", info.width, info.height);
    println!("  Frame size: {} bytes (JPEG q={})", jpeg_red.len(), quality);
    println!("  Duration: {}s", duration_secs);
    println!();

    let duration = Duration::from_secs(duration_secs);
    let mut frame_times: Vec<Duration> = Vec::new();
    let mut use_red = true;

    let bench_start = Instant::now();
    while bench_start.elapsed() < duration {
        let frame_start = Instant::now();
        let jpeg = if use_red { &jpeg_red } else { &jpeg_blue };
        transport.send_frame(jpeg)?;
        frame_times.push(frame_start.elapsed());
        use_red = !use_red;
    }

    transport.close();

    // Report results
    let total_elapsed = bench_start.elapsed();
    let count = frame_times.len();
    if count == 0 {
        println!("No frames sent!");
        return Ok(());
    }

    let avg_fps = count as f64 / total_elapsed.as_secs_f64();
    let min = frame_times.iter().min().unwrap();
    let max = frame_times.iter().max().unwrap();
    let avg = Duration::from_nanos(
        (frame_times.iter().map(|d| d.as_nanos()).sum::<u128>() / count as u128) as u64
    );

    println!("Results:");
    println!("  Duration: {:.1}s", total_elapsed.as_secs_f64());
    println!("  Frames sent: {}", count);
    println!("  Average FPS: {:.1}", avg_fps);
    println!("  Frame time: min={:.1}ms avg={:.1}ms max={:.1}ms",
             min.as_secs_f64() * 1000.0,
             avg.as_secs_f64() * 1000.0,
             max.as_secs_f64() * 1000.0);

    Ok(())
}
```

**Step 3: Verify it compiles**

Run: `cargo check`
Expected: passes (the function exists but isn't called yet)

**Step 4: Commit**

```bash
git add src/cli.rs
git commit -m "feat: add run_bench() for USB throughput measurement"
```

### Task 5: Review Task 4

**Trigger:** Both reviewers start when Task 4 completes.

**Killer items (blocking):**
- [ ] `run_bench()` is `pub fn run_bench(duration_secs: u64) -> Result<()>` — not async (USB is blocking)
- [ ] Two JPEGs pre-rendered before timing loop — `encode_jpeg()` calls happen BEFORE `Instant::now()` bench start
- [ ] Timing loop uses `Instant` for bench start and per-frame timing — no sleeping, no async
- [ ] `transport.close()` called after the loop
- [ ] Frame times collected in `Vec<Duration>`, min/max/avg computed correctly
- [ ] Config loaded for `jpeg_quality` and `rotation` — not hardcoded

**Quality items (non-blocking):**
- [ ] Output format matches design doc (device info, frame size, results)
- [ ] Alternating red/blue frames for visual confirmation

### Task 6: Milestone — Benchmark logic complete

**Present to user:**
- `run_bench()` implemented with pre-rendered alternating frames, tight USB loop, timing stats
- Compiles but not yet wired to CLI dispatch

**Wait for user response before proceeding to Task 7.**

---

### Task 7: Wire bench command in main.rs and add CLI tests [DO-CONFIRM]

**Files:**
- Modify: `src/main.rs:31-37` (add match arm)
- Modify: `src/cli.rs` (add CLI parse tests)

**Implement:**

In `src/main.rs`, add the `Bench` match arm before `Command::Daemon`:

```rust
Command::Bench { duration } => {
    return thermalwriter::cli::run_bench(duration);
}
```

Note: `run_bench()` is NOT async — call it directly, not with `.await`.

In `src/cli.rs` tests, add:

```rust
#[test]
fn cli_parses_bench() {
    let cli = Cli::try_parse_from(["thermalwriter", "bench"]).unwrap();
    assert!(matches!(cli.command, Command::Bench { duration: 10 }));
}

#[test]
fn cli_parses_bench_with_duration() {
    let cli = Cli::try_parse_from(["thermalwriter", "bench", "--duration", "30"]).unwrap();
    assert!(matches!(cli.command, Command::Bench { duration: 30 }));
}
```

**Confirm checklist:**
- [ ] `Command::Bench { duration }` match arm in `src/main.rs` calls `run_bench(duration)` (not `.await`)
- [ ] Match arm returns early (like `Command::Ctl`)
- [ ] Two CLI parse tests added and pass: default duration=10, explicit duration=30
- [ ] `cargo test` passes (all existing + new tests)
- [ ] `cargo check` passes
- [ ] Committed

### Task 8: Review Task 7

**Trigger:** Both reviewers start when Task 7 completes.

**Killer items (blocking):**
- [ ] `main.rs` match arm calls `run_bench(duration)` synchronously — NOT `run_bench(duration).await`
- [ ] Match arm has `return` so daemon startup code is not reached
- [ ] `cli_parses_bench` test asserts default duration is 10
- [ ] `cli_parses_bench_with_duration` test asserts `--duration 30` parses to 30
- [ ] `cargo test` shows all tests passing (run and verify output)

**Quality items (non-blocking):**
- [ ] Match arm order: `Bench` before `Daemon` (like `Ctl`)
- [ ] No unused imports introduced

### Task 9: Milestone — Final: everything wired and tested

**Present to user:**
- Full `thermalwriter bench [--duration N]` subcommand working
- All tests passing
- Ready for hardware testing: `cargo run -- bench`
- Remind user to stop the daemon first: `systemctl --user stop thermalwriter`

**Wait for user response.**
