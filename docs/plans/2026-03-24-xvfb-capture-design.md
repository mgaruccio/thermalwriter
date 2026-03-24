---
date: 2026-03-24
topic: xvfb-capture
---

# Xvfb Capture Source

## What We're Building

A generic xvfb-based frame capture source for the thermalwriter daemon. This enables displaying output from any X11 application (conky, doom, custom tools) on the cooler LCD by capturing a virtual framebuffer. The daemon spawns and owns the xvfb + child app lifecycle, reads the framebuffer via mmap, and ships frames through the existing JPEG encode + USB pipeline.

This also refactors the `FrameSource` trait to return raw RGB data instead of `Pixmap`, eliminating unnecessary pixel format round-trips.

## Why This Approach

The display benchmark showed USB transport handles 750fps at ~0% CPU — the bottleneck is rendering. For applications that render themselves (conky, games, custom tools), we can bypass the SVG/HTML rendering pipeline entirely and capture their output directly.

xvfb with `-fbdir` + mmap was chosen over X11 SHM (XShmGetImage) because:
- Zero dependencies beyond libc (no X11 client libraries)
- Zero-copy reads from the kernel page cache
- Simpler implementation — just mmap a file
- Tearing is negligible at 480x480, even at 60fps

Daemon-owned lifecycle (vs. user-managed) follows the `xvfb-run` pattern used by CI harnesses, ffmpeg wrappers, and headless capture tools.

## Key Decisions

- **FrameSource returns raw RGB, not Pixmap:** All sources must output RGB (3 bytes/pixel). Existing renderers convert internally via a shared `pixmap_to_rgb()` helper. `encode_jpeg` accepts raw RGB instead of Pixmap. This eliminates the premultiply/de-premultiply round-trip for xvfb capture.

- **Generic capture, not conky-specific:** The xvfb source captures any X11 application — conky, doom, a web browser, anything. The child command is user-configurable.

- **Daemon owns xvfb + child process lifecycle:** `XvfbManager` spawns both on mode activation, kills both on mode switch or shutdown. The `XvfbSource` frame source only reads pixels — it doesn't manage processes.

- **mmap capture via `-fbdir`:** Xvfb writes a memory-mapped XWD file. XvfbSource mmaps it once, reads BGRA pixels at a known offset, converts BGRA→RGB per frame.

- **15fps default, 60fps cap for xvfb mode:** USB can handle 750fps but the display panel likely refreshes at ~60Hz. 15fps is smooth for widgets, 60fps cap for games.

## Architecture

### FrameSource Trait (refactored)

```rust
pub struct RawFrame {
    pub data: Vec<u8>,  // RGB, 3 bytes per pixel
    pub width: u32,
    pub height: u32,
}

pub trait FrameSource: Send {
    fn render(&mut self, sensors: &SensorData) -> Result<RawFrame>;
    fn name(&self) -> &str;
    fn set_template(&mut self, _template: &str) {}
}
```

### XvfbSource (`src/render/xvfb.rs`)

- mmaps the XWD fbdir file on construction, parses header for pixel data offset
- `render()`: reads BGRA pixels at offset, converts BGRA→RGB, returns RawFrame
- Ignores sensor data — child app handles its own
- mmap stays open for source lifetime (no per-frame I/O)

### XvfbManager (`src/service/xvfb.rs`)

- Picks unused display number (`:99` and up)
- Creates temp fbdir directory
- Spawns `Xvfb :<display> -screen 0 480x480x24 -fbdir <tmpdir> -ac`
- Waits for fbdir file to appear
- Spawns child app with `DISPLAY=:<n>`
- `Drop` impl kills both processes, cleans up temp dir

### Configuration

```toml
[display]
mode = "svg"  # "svg", "html", or "xvfb"

[xvfb]
command = "conky -c ~/.config/conky/lcd.conf"
```

### Control

- D-Bus: `set_mode(mode, command)` to switch at runtime
- CLI: `thermalwriter ctl mirror "command"` for xvfb mode
- CLI: `thermalwriter ctl layout <name>` switches back to svg/html mode

### Data Flow

```
xvfb mode:  Xvfb fbdir → mmap → BGRA→RGB → encode_jpeg → rotate → USB
svg mode:   Tera+resvg → Pixmap → pixmap_to_rgb → encode_jpeg → rotate → USB
html mode:  Tera+taffy → Pixmap → pixmap_to_rgb → encode_jpeg → rotate → USB
```

## Open Questions

- Child app crash handling: log + idle, restart, or fallback to SVG?
- Sensor polling in xvfb mode: skip to save cycles, or leave running for D-Bus status?

## Next Steps

→ writing-plans skill for implementation details
