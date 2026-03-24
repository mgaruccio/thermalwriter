---
date: 2026-03-23
topic: display-benchmark
---

# Display Benchmark Subcommand

## What We're Building

A `thermalwriter bench` CLI subcommand that measures the maximum frame rate the LCD display hardware will accept over USB. It pre-renders two visually distinct JPEG frames (for visual confirmation), then sends them alternating in a tight loop for a configurable duration, reporting throughput statistics.

This is the prerequisite for video and application streaming features — we need to know the hardware ceiling before designing around it.

## Why This Approach

The bottleneck for video/app streaming isn't CPU rendering — it's USB throughput and what the display controller will actually accept and render. A synthetic benchmark that bypasses the rendering pipeline isolates this constraint directly.

Alternating two distinct frames (red/blue) lets you visually confirm the display is truly refreshing, not just accepting and discarding data.

## Key Decisions

- **New CLI subcommand** (`thermalwriter bench`), not an example binary — this is a first-class tool
- **Pre-baked JPEGs**: Encode two test frames before the timed loop so encoding cost doesn't pollute the measurement
- **Alternating frames**: Red and blue so you can visually verify refresh on the hardware
- **Reuse `BulkUsb` transport**: Same code path the daemon uses — measures the real transfer path
- **Apply configured rotation**: Default 180° so frames display correctly on the hardware
- **Default 10s duration**: `--duration` flag to override

## Output Format

```
Benchmarking display throughput...
  Duration: 10.0s
  Frame size: ~34 KB (JPEG q=85)
  Frames sent: 247
  Average FPS: 24.7
  Frame time: min=38.2ms avg=40.5ms max=52.1ms
```

## Open Questions

- Will the display accept frames faster than its own panel refresh? (This benchmark will answer that)
- Is there a protocol-level acknowledgment or backpressure from the device? (USB bulk transfer semantics should tell us)

## Next Steps

-> writing-plans skill for implementation details
