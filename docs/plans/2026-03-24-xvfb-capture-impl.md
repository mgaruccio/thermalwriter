# Xvfb Capture Source Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use forge:executing-plans to implement this plan task-by-task.

**Goal:** Add a generic xvfb-based frame capture source, enabling any X11 application (conky, doom, etc.) to display on the cooler LCD. Refactor `FrameSource` trait to return raw RGB instead of `Pixmap`.

**Architecture:** Three phases — refactor the rendering pipeline to use raw RGB throughout, implement xvfb capture + process management, then wire configuration/D-Bus/CLI for mode switching. The trait refactor is the foundation; everything else builds on it.

**Tech Stack:** tiny-skia (Pixmap conversion), image (PNG/JPEG encoding), libc (mmap), std::process (xvfb/child management), zbus (D-Bus), clap (CLI)

**Required Skills:**
- `forge:writing-tests`: Invoke before writing test code in Tasks 1, 4, 5, 8

## Context for Executor

### Key Files
- `src/render/mod.rs:1-73` — `FrameSource` trait (line 22), `RawFrame` will go here, `TemplateRenderer` impl (line 50). Trait returns `Pixmap` → will return `RawFrame`.
- `src/render/svg.rs:1-119` — `SvgRenderer` impl of `FrameSource` (line 72). Renders via resvg → Pixmap → will convert to RawFrame.
- `src/render/blitz.rs:1-122` — `BlitzRenderer` impl (line 101). Behind `--features blitz`. Renders via vello_cpu → Pixmap → will convert to RawFrame.
- `src/service/tick.rs:1-187` — `rotate_pixels()` (line 18, hardcoded 4 bytes/pixel), `encode_jpeg()` (line 67, takes Pixmap), `run_tick_loop()` (line 104). All need refactoring.
- `src/service/dbus.rs:1-182` — `ServiceState` (line 13) has `layout_change_tx` (mpsc<String>). Will change to `mode_change_tx` (mpsc<ModeChange>). `DisplayInterface` (line 36) needs `set_mode` method.
- `src/config.rs:1-133` — `DisplayConfig` (line 12) needs `mode: String` field. New `XvfbConfig` struct needed.
- `src/main.rs:1-170` — Command dispatch (line 31), frame source creation (line 100), layout change listener (line 133), tick loop (line 154). Mode switching logic goes here.
- `src/cli.rs:1-174` — `CtlCommand` enum (line 24) needs `Mirror` variant. `run_bench()` (line ~125) uses Pixmap+Color for test frames → will use RawFrame directly.
- `tests/integration_tests.rs:1-142` — `MockFrameSource` (line 24, 110) returns Pixmap. `encode_jpeg` tests (line 35-54) use Pixmap. All need updating.
- `tests/render_tests.rs:76-96` — TemplateRenderer test checks `pixmap.data()` RGBA pixels → will check `frame.data` RGB pixels.
- `tests/component_tests.rs:68-83` — SvgRenderer test calls `renderer.render()` → return type changes.
- `examples/preview_layout.rs:149-153` — `pixmap.save_png()` → needs `image::ImageBuffer` for PNG save.
- `examples/render_layout.rs:188-222` — Same: `pixmap.save_png()` + `encode_jpeg(&pixmap)` → RawFrame path.
- `examples/preview_blitz.rs:91-95` — Same: `pixmap.save_png()` → ImageBuffer.
- `src/lib.rs:1-7` — Module declarations. No `xvfb` module exists yet.

### Research Findings
- **XWD file format** (xvfb `-fbdir` output): Big-endian header. First 4 bytes = header_size (u32 BE). For 24-bit TrueColor depth: `ncolors=0`, pixel data starts at byte `header_size`. Pixels are 32-bit (padded from 24), in host byte order. On x86 (LSBFirst): memory layout is `[B, G, R, X]` per pixel. Key header fields: `pixmap_width` at offset 16, `pixmap_height` at offset 20, `bits_per_pixel` at offset 44, `bytes_per_line` at offset 48.
- **BGRA→RGB conversion**: For each 4-byte pixel `[B, G, R, X]`, extract `[R, G, B]` = bytes `[2, 1, 0]`. At 480×480 = 230K pixels, this is ~1μs.
- **mmap lifetime**: mmap the file once on XvfbSource construction. The kernel page cache ensures reads reflect current framebuffer state. No per-frame open/close needed.
- **Display number detection**: Check `/tmp/.X{N}-lock` file existence, starting at 99, scanning upward. This is the `xvfb-run` pattern.
- **Xvfb spawn command**: `Xvfb :N -screen 0 480x480x24 -fbdir /tmp/thermalwriter-xvfb-XXXXX -ac -nolisten tcp`
- **fbdir file appearance**: Xvfb creates `{fbdir}/Xvfb_screen0` after startup. Poll for existence with timeout.
- **rotate_pixels**: Currently hardcoded to 4 bytes/pixel (RGBA). Needs update to 3 bytes/pixel (RGB) since all frame data is now RGB.

### Relevant Patterns
- `src/render/mod.rs:50-67` — TemplateRenderer's FrameSource impl: the pattern all renderers follow
- `src/cli.rs:59-109` — `run_ctl()`: pattern for CtlCommand handlers
- `src/cli.rs:125-165` — `run_bench()`: pattern for standalone USB commands (pre-render, tight loop, stats)
- `src/service/dbus.rs:38-58` — `set_layout()`: pattern for D-Bus methods with channel send + state update
- `src/main.rs:133-147` — Layout change listener: pattern for mode change listener

## Execution Architecture

**Team:** 2 devs, 1 spec reviewer, 1 quality reviewer
**Task dependencies:**
  - Task 1 (FrameSource refactor) is standalone — no dependencies
  - Tasks 4 and 5 are independent (can run in parallel) — both depend on Task 3 milestone
  - Task 8 depends on Task 7 milestone (needs XvfbSource, XvfbManager, and Phase 1 complete)
**Phases:**
  - Phase 1: Task 1 (FrameSource refactor — sequential, one dev)
  - Phase 2: Tasks 4-5 (XvfbSource + XvfbManager — parallel, two devs)
  - Phase 3: Task 8 (Integration — sequential, one dev)
**Milestones:**
  - After Task 3 (FrameSource refactor complete, all existing tests pass)
  - After Task 7 (xvfb capture + process management complete)
  - After Task 10 (final — full feature wired and tested)

---

### Task 1: Refactor FrameSource pipeline to raw RGB [READ-DO]

**Files:**
- Modify: `src/render/mod.rs:14-73` (trait, RawFrame, TemplateRenderer)
- Modify: `src/render/svg.rs:72-113` (SvgRenderer impl)
- Modify: `src/render/blitz.rs:101-121` (BlitzRenderer impl)
- Modify: `src/service/tick.rs:16-96` (rotate_pixels, encode_jpeg)
- Modify: `src/service/tick.rs:104-187` (run_tick_loop — frame_source.render() call site)
- Modify: `src/main.rs:100-165` (frame_source type, tick loop call)
- Modify: `src/cli.rs:1-8,125-165` (run_bench imports and implementation)
- Modify: `tests/integration_tests.rs:1-142` (MockFrameSource, encode_jpeg tests)
- Modify: `tests/render_tests.rs:76-96` (TemplateRenderer pixel assertions)
- Modify: `tests/component_tests.rs:68-83` (SvgRenderer render call)
- Modify: `examples/preview_layout.rs:149-153` (PNG save)
- Modify: `examples/render_layout.rs:188-222` (PNG save + encode_jpeg calls)
- Modify: `examples/preview_blitz.rs:91-95` (PNG save)

This is one atomic refactor — all changes must compile together. Follow each step sequentially.

**Step 1: Add `RawFrame` struct and `from_pixmap` to `src/render/mod.rs`**

Add after the `SensorData` type alias (line 19), before the `FrameSource` trait:

```rust
/// A rendered frame as raw RGB pixel data (3 bytes per pixel, row-major).
#[derive(Debug, Clone)]
pub struct RawFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl RawFrame {
    /// Convert a tiny_skia Pixmap (premultiplied RGBA) to RawFrame (straight RGB).
    pub fn from_pixmap(pixmap: &Pixmap) -> Self {
        let data = pixmap.data();
        let pixel_count = (pixmap.width() * pixmap.height()) as usize;
        let mut rgb = Vec::with_capacity(pixel_count * 3);
        for chunk in data.chunks(4) {
            let a = chunk[3] as u16;
            if a == 0 {
                rgb.extend_from_slice(&[0, 0, 0]);
            } else {
                let r = ((chunk[0] as u16 * 255) / a).min(255) as u8;
                let g = ((chunk[1] as u16 * 255) / a).min(255) as u8;
                let b = ((chunk[2] as u16 * 255) / a).min(255) as u8;
                rgb.extend_from_slice(&[r, g, b]);
            }
        }
        Self {
            data: rgb,
            width: pixmap.width(),
            height: pixmap.height(),
        }
    }

    /// Save frame as PNG (convenience for examples/debugging).
    pub fn save_png(&self, path: &str) -> anyhow::Result<()> {
        use image::{ImageBuffer, Rgb};
        let img: ImageBuffer<Rgb<u8>, _> = ImageBuffer::from_raw(
            self.width, self.height, self.data.clone()
        ).ok_or_else(|| anyhow::anyhow!("Failed to create image buffer"))?;
        img.save(path)?;
        Ok(())
    }
}
```

**Step 2: Change the `FrameSource` trait return type**

In `src/render/mod.rs`, change the trait (line 22):

```rust
pub trait FrameSource: Send {
    fn render(&mut self, sensors: &SensorData) -> Result<RawFrame>;
    fn name(&self) -> &str;
    fn set_template(&mut self, _template: &str) {}
}
```

**Step 3: Update `TemplateRenderer` impl in `src/render/mod.rs`**

Change `impl FrameSource for TemplateRenderer` (line 50) — the last line of `render()`:

```rust
    fn render(&mut self, sensors: &SensorData) -> Result<RawFrame> {
        // Steps 1-3 unchanged (tera → parse → layout → draw)
        let mut context = tera::Context::new();
        for (key, value) in sensors {
            context.insert(key, value);
        }
        let rendered = tera::Tera::one_off(&self.template, &context, false)?;
        let root = parser::parse_html(&rendered)?;
        let nodes = layout::compute_layout(&root, self.width as f32, self.height as f32)?;
        let pixmap = draw::render_nodes(&nodes, self.width, self.height)?;
        Ok(RawFrame::from_pixmap(&pixmap))
    }
```

**Step 4: Update `SvgRenderer` impl in `src/render/svg.rs`**

Change the return type of `render()` at line 73 and convert at the end:

```rust
    fn render(&mut self, sensors: &SensorData) -> Result<RawFrame> {
        // ... existing steps 1-4 unchanged ...
        resvg::render(&tree, transform, &mut pixmap.as_mut());
        Ok(RawFrame::from_pixmap(&pixmap))
    }
```

Add `use super::RawFrame;` to the imports (or it's already in scope via `use super::{FrameSource, SensorData}`). Add `RawFrame` to that import line.

**Step 5: Update `BlitzRenderer` impl in `src/render/blitz.rs`**

Change return type at line 102:

```rust
    fn render(&mut self, sensors: &SensorData) -> Result<RawFrame> {
        let mut context = tera::Context::new();
        for (key, value) in sensors {
            context.insert(key, value);
        }
        let html = tera::Tera::one_off(&self.template, &context, false)?;
        let pixmap = self.render_html(&html)?;
        Ok(RawFrame::from_pixmap(&pixmap))
    }
```

Add `RawFrame` to the `use super::{FrameSource, SensorData};` import.

**Step 6: Update `rotate_pixels` in `src/service/tick.rs` for 3 bytes/pixel**

Change function signature and all `* 4` → `* 3`, `+ 4` → `+ 3`:

```rust
pub fn rotate_pixels(data: &[u8], width: u32, height: u32, degrees: u16) -> (Vec<u8>, u32, u32) {
    let w = width as usize;
    let h = height as usize;
    let pixel_count = w * h;

    match degrees {
        0 => (data.to_vec(), width, height),
        180 => {
            let mut out = vec![0u8; data.len()];
            for i in 0..pixel_count {
                let src = i * 3;
                let dst = (pixel_count - 1 - i) * 3;
                out[dst..dst + 3].copy_from_slice(&data[src..src + 3]);
            }
            (out, width, height)
        }
        90 => {
            let mut out = vec![0u8; data.len()];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * 3;
                    let dst = (x * h + (h - 1 - y)) * 3;
                    out[dst..dst + 3].copy_from_slice(&data[src..src + 3]);
                }
            }
            (out, height, width)
        }
        270 => {
            let mut out = vec![0u8; data.len()];
            for y in 0..h {
                for x in 0..w {
                    let src = (y * w + x) * 3;
                    let dst = ((w - 1 - x) * h + y) * 3;
                    out[dst..dst + 3].copy_from_slice(&data[src..src + 3]);
                }
            }
            (out, height, width)
        }
        _ => {
            log::warn!("Unsupported rotation {}, using 0", degrees);
            (data.to_vec(), width, height)
        }
    }
}
```

**Step 7: Refactor `encode_jpeg` in `src/service/tick.rs` to accept `RawFrame`**

Replace the entire `encode_jpeg` function (line 67-96):

```rust
use crate::render::RawFrame;

/// Encode a RawFrame (straight RGB) to JPEG bytes, with optional rotation.
pub fn encode_jpeg(frame: &RawFrame, quality: u8, rotation: u16) -> Result<Vec<u8>> {
    let (rotated, out_w, out_h) = rotate_pixels(&frame.data, frame.width, frame.height, rotation);

    let img: ImageBuffer<Rgb<u8>, _> = ImageBuffer::from_raw(out_w, out_h, rotated)
        .ok_or_else(|| anyhow::anyhow!("Failed to create image buffer"))?;

    let mut buf = std::io::Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    image::DynamicImage::ImageRgb8(img).write_with_encoder(encoder)?;

    Ok(buf.into_inner())
}
```

Remove the now-unused `use tiny_skia::Pixmap;` import from tick.rs.

**Step 8: Update `run_tick_loop` in `src/service/tick.rs`**

The render call (line 157-158) changes from:

```rust
Ok(pixmap) => {
    match encode_jpeg(&pixmap, jpeg_quality, rotation) {
```

to:

```rust
Ok(frame) => {
    match encode_jpeg(&frame, jpeg_quality, rotation) {
```

This is just a variable rename — `pixmap` → `frame`.

**Step 9: Update `run_bench()` in `src/cli.rs`**

Replace the Pixmap/Color-based frame creation with direct RGB construction. Change imports at top of file — remove `tiny_skia::{Color, Pixmap}`, add `crate::render::RawFrame`:

```rust
use crate::render::RawFrame;
```

Replace the frame creation section in `run_bench()`:

```rust
    // Pre-render two solid-color frames (red and blue) for visual confirmation
    let frame_red = RawFrame {
        data: vec![255, 0, 0].repeat(480 * 480),
        width: 480,
        height: 480,
    };
    let jpeg_red = encode_jpeg(&frame_red, quality, rotation)?;

    let frame_blue = RawFrame {
        data: vec![0, 0, 255].repeat(480 * 480),
        width: 480,
        height: 480,
    };
    let jpeg_blue = encode_jpeg(&frame_blue, quality, rotation)?;
```

**Step 10: Update `tests/integration_tests.rs`**

Change MockFrameSource (line 24) and TrackingFrameSource (line 110) to return `RawFrame`:

```rust
use thermalwriter::render::{SensorData, FrameSource, RawFrame};
// Remove: use tiny_skia::Pixmap;

struct MockFrameSource {
    last_template: Option<String>,
}
impl FrameSource for MockFrameSource {
    fn render(&mut self, _sensors: &SensorData) -> Result<RawFrame> {
        Ok(RawFrame {
            data: vec![0u8; 480 * 480 * 3],
            width: 480,
            height: 480,
        })
    }
    fn name(&self) -> &str { "mock" }
    fn set_template(&mut self, template: &str) {
        self.last_template = Some(template.to_string());
    }
}
```

Same pattern for `TrackingFrameSource` (line 110).

Update `encode_jpeg` tests (lines 35-54) to use RawFrame:

```rust
#[test]
fn jpeg_encode_produces_valid_output() {
    use thermalwriter::service::tick::encode_jpeg;
    use thermalwriter::render::RawFrame;
    let frame = RawFrame { data: vec![0u8; 480 * 480 * 3], width: 480, height: 480 };
    let jpeg = encode_jpeg(&frame, 85, 0).unwrap();
    assert_eq!(&jpeg[0..2], &[0xFF, 0xD8]);
    assert!(jpeg.len() > 100, "JPEG should be more than 100 bytes");
}

#[test]
fn jpeg_encode_quality_affects_size() {
    use thermalwriter::service::tick::encode_jpeg;
    use thermalwriter::render::RawFrame;
    let frame = RawFrame { data: vec![0u8; 480 * 480 * 3], width: 480, height: 480 };
    let jpeg_high = encode_jpeg(&frame, 95, 0).unwrap();
    let jpeg_low = encode_jpeg(&frame, 10, 0).unwrap();
    assert_eq!(&jpeg_high[0..2], &[0xFF, 0xD8]);
    assert_eq!(&jpeg_low[0..2], &[0xFF, 0xD8]);
}
```

**Step 11: Update `tests/render_tests.rs` (line 76-96)**

Change the `template_renderer_produces_480x480_pixmap` test:

```rust
#[test]
fn template_renderer_produces_480x480_frame() {
    let layout_html = r#"<div style="display: flex; flex-direction: column; padding: 12px; background: #1a1a2e; color: #ffffff; font-size: 24px;">
        <span>CPU {{ cpu_temp }}C</span>
    </div>"#;

    let mut renderer = TemplateRenderer::new(layout_html, 480, 480).unwrap();
    let mut sensors = HashMap::new();
    sensors.insert("cpu_temp".to_string(), "65".to_string());

    let frame = renderer.render(&sensors).unwrap();
    assert_eq!(frame.width, 480);
    assert_eq!(frame.height, 480);
    assert_eq!(frame.data.len(), 480 * 480 * 3);
    // Verify background color is #1a1a2e (RGB)
    let pixel = &frame.data[0..3];
    assert_eq!(pixel[0], 0x1a, "R channel should be 0x1a");
    assert_eq!(pixel[1], 0x1a, "G channel should be 0x1a");
    assert_eq!(pixel[2], 0x2e, "B channel should be 0x2e");
}
```

**Step 12: Update `tests/component_tests.rs` (line 68-83)**

The SvgRenderer test calls `renderer.render(&sensors)` which now returns `RawFrame`. Update:

```rust
    let result = renderer.render(&sensors);
    assert!(result.is_ok(), "Renderer with component function should render: {:?}", result.err());
```

(No change needed — `result` is now `Result<RawFrame>` but the test only checks `.is_ok()`.)

**Step 13: Update `examples/preview_layout.rs` (lines 149-153)**

Replace:
```rust
    let pixmap = renderer.render(&sensors)?;
    let path = format!("/tmp/thermalwriter_{}.png", display_name);
    pixmap.save_png(&path)?;
```

With:
```rust
    let frame = renderer.render(&sensors)?;
    let path = format!("/tmp/thermalwriter_{}.png", display_name);
    frame.save_png(&path)?;
```

Remove the `use tiny_skia::Pixmap;` import if it existed (it doesn't in this file, but verify).

**Step 14: Update `examples/render_layout.rs`**

Replace all `pixmap` usage. Key changes:

Line 188-194:
```rust
    let frame = renderer.render(&initial_sensors)?;
    let png_path = format!("/tmp/thermalwriter_{}.png", display_name);
    frame.save_png(&png_path)?;
    println!("\nSaved preview: {}", png_path);
    let jpeg_data = encode_jpeg(&frame, 85, 180)?;
```

Line 221-222 (inside loop):
```rust
        let frame = renderer.render(&sensors)?;
        let jpeg_data = encode_jpeg(&frame, 85, 180)?;
```

**Step 15: Update `examples/preview_blitz.rs` (lines 91-95)**

Replace:
```rust
    let pixmap = renderer.render(&sensors)?;
    let path = format!("/tmp/thermalwriter_blitz_{}.png", display_name);
    pixmap.save_png(&path)?;
```

With:
```rust
    let frame = renderer.render(&sensors)?;
    let path = format!("/tmp/thermalwriter_blitz_{}.png", display_name);
    frame.save_png(&path)?;
```

**Step 16: Verify everything compiles and tests pass**

Run: `cargo test`
Expected: All tests pass (101 tests). No warnings about unused imports.

Run: `cargo check --features blitz` (if blitz feature is available)
Expected: passes.

**Step 17: Commit**

```bash
git add -A
git commit -m "refactor: change FrameSource to return raw RGB instead of Pixmap

All renderers now return RawFrame (straight RGB, 3 bytes/pixel) instead
of tiny_skia::Pixmap. RawFrame::from_pixmap() handles premultiplied
RGBA → straight RGB conversion. encode_jpeg() accepts RawFrame directly.
This removes the premultiply round-trip and prepares for xvfb capture."
```

### Task 2: Review Task 1

**Trigger:** Both reviewers start simultaneously when Task 1 completes.

**Killer items (blocking):**
- [ ] `RawFrame` struct exists in `src/render/mod.rs` with `data: Vec<u8>`, `width: u32`, `height: u32`
- [ ] `RawFrame::from_pixmap()` correctly de-premultiplies alpha — verify the `a == 0` branch returns `[0,0,0]` and the else branch divides by alpha. Check at `src/render/mod.rs`.
- [ ] `FrameSource::render()` return type is `Result<RawFrame>` in the trait definition
- [ ] All three renderer impls (SvgRenderer, TemplateRenderer, BlitzRenderer) call `RawFrame::from_pixmap(&pixmap)` — not returning Pixmap
- [ ] `rotate_pixels` in `src/service/tick.rs` uses `* 3` and `+ 3` everywhere, not `* 4` — check all four rotation branches (0, 90, 180, 270)
- [ ] `encode_jpeg` in `src/service/tick.rs` accepts `&RawFrame`, not `&Pixmap` — no premultiply conversion code remains in this function
- [ ] `run_bench()` in `src/cli.rs` creates `RawFrame` with `vec![255, 0, 0].repeat(480 * 480)` for red, not using `Pixmap`/`Color`
- [ ] `cargo test` passes all tests — run it and verify count (should be ~101)

**Quality items (non-blocking):**
- [ ] No unused `tiny_skia::Pixmap` imports remain in files that no longer use it (tick.rs, cli.rs, integration_tests.rs)
- [ ] `RawFrame::save_png` helper exists for example convenience
- [ ] Test assertions in `render_tests.rs` check RGB (3 bytes) not RGBA (4 bytes)

### Task 3: Milestone — FrameSource refactor complete

**Present to user:**
- FrameSource trait now returns RawFrame (raw RGB) instead of Pixmap
- All renderers, tick loop, examples, tests, and bench command updated
- All existing tests pass — no behavior change, just cleaner data path
- Foundation ready for xvfb capture (which produces RGB natively)

**Wait for user response before proceeding to Task 4.**

---

### Task 4: Implement XvfbSource [READ-DO]

**Files:**
- Create: `src/render/xvfb.rs`
- Modify: `src/render/mod.rs:1-10` (add `pub mod xvfb;`)

**Step 1: Add the module declaration**

In `src/render/mod.rs`, add after the other module declarations (line 8):

```rust
pub mod xvfb;
```

**Step 2: Create `src/render/xvfb.rs` with XWD header parsing and mmap capture**

```rust
//! Xvfb framebuffer capture: reads pixels from an mmap'd XWD file produced by Xvfb -fbdir.

use std::fs::File;
use std::path::Path;
use anyhow::{Context, Result, bail};

use super::{FrameSource, RawFrame, SensorData};

/// XWD header field offsets (all big-endian u32).
const XWD_HEADER_SIZE: usize = 0;
const XWD_PIXMAP_WIDTH: usize = 16;
const XWD_PIXMAP_HEIGHT: usize = 20;
const XWD_BITS_PER_PIXEL: usize = 44;
const XWD_BYTES_PER_LINE: usize = 48;
const XWD_NCOLORS: usize = 76;
const XWD_COLOR_ENTRY_SIZE: usize = 12;

/// Read a big-endian u32 from a byte slice at the given offset.
fn read_be_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]])
}

/// Parsed XWD header info needed for pixel extraction.
struct XwdHeader {
    pixel_data_offset: usize,
    width: u32,
    height: u32,
    bits_per_pixel: u32,
    bytes_per_line: u32,
}

fn parse_xwd_header(data: &[u8]) -> Result<XwdHeader> {
    if data.len() < 100 {
        bail!("XWD file too small: {} bytes", data.len());
    }

    let header_size = read_be_u32(data, XWD_HEADER_SIZE) as usize;
    let width = read_be_u32(data, XWD_PIXMAP_WIDTH);
    let height = read_be_u32(data, XWD_PIXMAP_HEIGHT);
    let bits_per_pixel = read_be_u32(data, XWD_BITS_PER_PIXEL);
    let bytes_per_line = read_be_u32(data, XWD_BYTES_PER_LINE);
    let ncolors = read_be_u32(data, XWD_NCOLORS) as usize;

    let pixel_data_offset = header_size + ncolors * XWD_COLOR_ENTRY_SIZE;

    if bits_per_pixel != 32 {
        bail!("Unsupported XWD bits_per_pixel: {} (expected 32)", bits_per_pixel);
    }

    Ok(XwdHeader {
        pixel_data_offset,
        width,
        height,
        bits_per_pixel,
        bytes_per_line,
    })
}

/// Convert BGRX pixel data (4 bytes/pixel, x86 byte order) to RGB (3 bytes/pixel).
fn bgrx_to_rgb(src: &[u8], width: u32, height: u32, bytes_per_line: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let bpl = bytes_per_line as usize;
    let mut rgb = Vec::with_capacity(w * h * 3);

    for y in 0..h {
        let row_start = y * bpl;
        for x in 0..w {
            let px = row_start + x * 4;
            // x86 LSBFirst: memory layout is [B, G, R, X]
            rgb.push(src[px + 2]); // R
            rgb.push(src[px + 1]); // G
            rgb.push(src[px]);     // B
        }
    }

    rgb
}

/// Frame source that captures from an Xvfb framebuffer via mmap.
pub struct XvfbSource {
    mmap: memmap2::Mmap,
    header: XwdHeader,
    expected_width: u32,
    expected_height: u32,
}

impl XvfbSource {
    /// Create a new XvfbSource from an Xvfb fbdir screen file.
    ///
    /// The file is typically `{fbdir}/Xvfb_screen0`, created by `Xvfb -fbdir`.
    pub fn new(fbdir_file: &Path, expected_width: u32, expected_height: u32) -> Result<Self> {
        let file = File::open(fbdir_file)
            .with_context(|| format!("Failed to open XWD file: {}", fbdir_file.display()))?;

        // Safety: Xvfb owns the file and writes valid data. We read-only mmap.
        let mmap = unsafe { memmap2::Mmap::map(&file) }
            .context("Failed to mmap XWD file")?;

        let header = parse_xwd_header(&mmap)?;

        if header.width != expected_width || header.height != expected_height {
            bail!(
                "XWD dimensions {}x{} don't match expected {}x{}",
                header.width, header.height, expected_width, expected_height
            );
        }

        Ok(Self {
            mmap,
            header,
            expected_width,
            expected_height,
        })
    }
}

impl FrameSource for XvfbSource {
    fn render(&mut self, _sensors: &SensorData) -> Result<RawFrame> {
        let pixel_data = &self.mmap[self.header.pixel_data_offset..];
        let rgb = bgrx_to_rgb(
            pixel_data,
            self.header.width,
            self.header.height,
            self.header.bytes_per_line,
        );

        Ok(RawFrame {
            data: rgb,
            width: self.expected_width,
            height: self.expected_height,
        })
    }

    fn name(&self) -> &str {
        "xvfb"
    }
}
```

**Step 3: Add `memmap2` dependency**

Run: `cargo add memmap2`

**Step 4: Write tests for XWD parsing and BGRX→RGB conversion**

Add to the bottom of `src/render/xvfb.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal synthetic XWD file for testing.
    fn build_test_xwd(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
        let header_size: u32 = 100 + 1; // 100-byte fixed header + 1-byte NUL window name
        let bits_per_pixel: u32 = 32;
        let bytes_per_line = width * 4;
        let ncolors: u32 = 0;

        let mut buf = vec![0u8; header_size as usize + pixels.len()];

        // Write header fields (big-endian)
        buf[0..4].copy_from_slice(&header_size.to_be_bytes());
        buf[4..8].copy_from_slice(&7u32.to_be_bytes()); // file_version = 7
        buf[8..12].copy_from_slice(&2u32.to_be_bytes()); // pixmap_format = ZPixmap
        buf[12..16].copy_from_slice(&24u32.to_be_bytes()); // pixmap_depth
        buf[16..20].copy_from_slice(&width.to_be_bytes());
        buf[20..24].copy_from_slice(&height.to_be_bytes());
        buf[44..48].copy_from_slice(&bits_per_pixel.to_be_bytes());
        buf[48..52].copy_from_slice(&bytes_per_line.to_be_bytes());
        buf[76..80].copy_from_slice(&ncolors.to_be_bytes());

        // Copy pixel data after header
        buf[header_size as usize..].copy_from_slice(pixels);

        buf
    }

    #[test]
    fn parse_xwd_header_extracts_dimensions() {
        let pixels = vec![0u8; 2 * 2 * 4]; // 2x2, 4 bytes/pixel
        let xwd = build_test_xwd(2, 2, &pixels);
        let header = parse_xwd_header(&xwd).unwrap();
        assert_eq!(header.width, 2);
        assert_eq!(header.height, 2);
        assert_eq!(header.bits_per_pixel, 32);
    }

    #[test]
    fn bgrx_to_rgb_converts_correctly() {
        // One pixel: B=0x11, G=0x22, R=0x33, X=0x00
        let bgrx = vec![0x11, 0x22, 0x33, 0x00];
        let rgb = bgrx_to_rgb(&bgrx, 1, 1, 4);
        assert_eq!(rgb, vec![0x33, 0x22, 0x11]); // R, G, B
    }

    #[test]
    fn bgrx_to_rgb_handles_multiple_pixels() {
        // 2x1: red pixel, blue pixel
        let bgrx = vec![
            0x00, 0x00, 0xFF, 0x00, // B=0, G=0, R=255 → red
            0xFF, 0x00, 0x00, 0x00, // B=255, G=0, R=0 → blue
        ];
        let rgb = bgrx_to_rgb(&bgrx, 2, 1, 8);
        assert_eq!(rgb, vec![0xFF, 0x00, 0x00, 0x00, 0x00, 0xFF]);
    }

    #[test]
    fn xvfb_source_reads_from_synthetic_xwd() {
        use std::io::Write;
        let width = 2u32;
        let height = 2u32;
        // 4 pixels, all green: B=0, G=255, R=0, X=0
        let pixels = vec![0x00, 0xFF, 0x00, 0x00].repeat(4);
        let xwd_data = build_test_xwd(width, height, &pixels);

        // Write to temp file
        let tmp = std::env::temp_dir().join("thermalwriter_test_xvfb.xwd");
        let mut f = std::fs::File::create(&tmp).unwrap();
        f.write_all(&xwd_data).unwrap();
        f.flush().unwrap();

        let mut source = XvfbSource::new(&tmp, width, height).unwrap();
        let frame = source.render(&std::collections::HashMap::new()).unwrap();

        assert_eq!(frame.width, 2);
        assert_eq!(frame.height, 2);
        assert_eq!(frame.data.len(), 2 * 2 * 3);
        // All pixels should be green (R=0, G=255, B=0)
        for chunk in frame.data.chunks(3) {
            assert_eq!(chunk, &[0x00, 0xFF, 0x00]);
        }

        std::fs::remove_file(&tmp).ok();
    }
}
```

**Step 5: Run tests**

Run: `cargo test`
Expected: All existing tests pass + 4 new XvfbSource tests pass.

**Step 6: Commit**

```bash
git add src/render/xvfb.rs src/render/mod.rs Cargo.toml Cargo.lock
git commit -m "feat: add XvfbSource for mmap-based framebuffer capture

Reads pixels from Xvfb's -fbdir XWD file via mmap. Parses XWD header
to find pixel data offset, converts BGRX to RGB per frame. Tests use
synthetic XWD files."
```

### Task 5: Implement XvfbManager [READ-DO]

**Files:**
- Create: `src/service/xvfb.rs`
- Modify: `src/service/mod.rs` (add `pub mod xvfb;`)

**Coordination required:**
Before starting, confirm with the dev implementing Task 4 that `XvfbSource::new()` takes `(path: &Path, width: u32, height: u32)` — the manager needs to pass the fbdir file path to the source constructor.

**Step 1: Add the module declaration**

Check `src/service/mod.rs` exists. If not, check how service modules are declared (might be in `src/lib.rs`). Add `pub mod xvfb;` to the appropriate location.

**Step 2: Create `src/service/xvfb.rs`**

```rust
//! Xvfb process manager: spawns/owns Xvfb and child application processes.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};
use anyhow::{Context, Result, bail};
use log::info;

/// Handle to a running Xvfb instance and its child application.
/// Dropping this handle kills both processes and cleans up the temp directory.
pub struct XvfbHandle {
    xvfb_process: Child,
    child_process: Option<Child>,
    display_num: u32,
    fbdir: PathBuf,
    screen_file: PathBuf,
}

impl XvfbHandle {
    /// Path to the XWD screen file (for XvfbSource to mmap).
    pub fn screen_file(&self) -> &Path {
        &self.screen_file
    }

    /// The display number (e.g., 99 for `:99`).
    pub fn display_num(&self) -> u32 {
        self.display_num
    }
}

impl Drop for XvfbHandle {
    fn drop(&mut self) {
        if let Some(ref mut child) = self.child_process {
            let _ = child.kill();
            let _ = child.wait();
            info!("Killed child application (pid {})", child.id());
        }
        let _ = self.xvfb_process.kill();
        let _ = self.xvfb_process.wait();
        info!("Killed Xvfb (pid {}, display :{})", self.xvfb_process.id(), self.display_num);
        // Clean up temp fbdir
        let _ = std::fs::remove_dir_all(&self.fbdir);
    }
}

/// Find an unused X display number by checking for lock files.
fn find_unused_display() -> Result<u32> {
    for n in 99..200 {
        let lock_file = format!("/tmp/.X{}-lock", n);
        if !Path::new(&lock_file).exists() {
            return Ok(n);
        }
    }
    bail!("No available X display number found (checked :99 through :199)")
}

/// Start Xvfb and a child application, returning a handle that owns both processes.
///
/// `command` is the shell command to run inside the virtual display (e.g., "conky -c foo.conf").
/// `width` and `height` set the virtual screen dimensions.
pub fn start(command: &str, width: u32, height: u32) -> Result<XvfbHandle> {
    let display_num = find_unused_display()?;
    let display = format!(":{}", display_num);

    // Create temp directory for fbdir
    let fbdir = std::env::temp_dir().join(format!("thermalwriter-xvfb-{}", display_num));
    std::fs::create_dir_all(&fbdir)
        .with_context(|| format!("Failed to create fbdir: {}", fbdir.display()))?;

    let screen_spec = format!("{}x{}x24", width, height);

    // Spawn Xvfb
    let xvfb_process = Command::new("Xvfb")
        .arg(&display)
        .args(["-screen", "0", &screen_spec])
        .args(["-fbdir", &fbdir.to_string_lossy()])
        .args(["-ac", "-nolisten", "tcp"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("Failed to spawn Xvfb — is xvfb installed?")?;

    info!("Spawned Xvfb on display {} (pid {})", display, xvfb_process.id());

    // Wait for screen file to appear
    let screen_file = fbdir.join("Xvfb_screen0");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !screen_file.exists() {
        if Instant::now() > deadline {
            // Cleanup on failure
            let mut proc = xvfb_process;
            let _ = proc.kill();
            let _ = std::fs::remove_dir_all(&fbdir);
            bail!("Xvfb screen file did not appear within 5 seconds: {}", screen_file.display());
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    info!("Xvfb screen file ready: {}", screen_file.display());

    // Spawn the child application with DISPLAY set
    let child_process = Command::new("sh")
        .args(["-c", command])
        .env("DISPLAY", &display)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("Failed to spawn child command: {}", command))?;

    info!("Spawned child application: {} (pid {})", command, child_process.id());

    Ok(XvfbHandle {
        xvfb_process,
        child_process: Some(child_process),
        display_num,
        fbdir,
        screen_file,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_unused_display_returns_valid_number() {
        let num = find_unused_display().unwrap();
        assert!(num >= 99 && num < 200);
        // Verify the lock file doesn't exist
        let lock = format!("/tmp/.X{}-lock", num);
        assert!(!Path::new(&lock).exists());
    }
}
```

**Step 3: Add module declaration**

Check how service modules are organized. Look at `src/service/mod.rs` or `src/lib.rs`:

If `src/service/mod.rs` exists, add: `pub mod xvfb;`
If services are declared in `src/lib.rs`, add: `pub mod xvfb;` inside the service module.

**Step 4: Run tests**

Run: `cargo test`
Expected: All tests pass including the new `find_unused_display` test.

**Step 5: Commit**

```bash
git add src/service/xvfb.rs src/service/mod.rs
git commit -m "feat: add XvfbManager for process lifecycle management

Spawns Xvfb with -fbdir and a child application with DISPLAY set.
Finds unused display numbers via lock file detection. Drop impl
cleans up both processes and temp directory."
```

### Task 6: Review Tasks 4-5

**Trigger:** Both reviewers start simultaneously when Tasks 4 and 5 complete.

**Killer items (blocking):**
- [ ] `XvfbSource::new()` in `src/render/xvfb.rs` mmaps the file with `memmap2::Mmap::map()` — read-only, mapped once on construction (not per-frame)
- [ ] `parse_xwd_header()` reads `header_size` from offset 0 as big-endian u32 and `ncolors` from offset 76 — pixel data offset is `header_size + ncolors * 12`
- [ ] `bgrx_to_rgb()` extracts bytes as `[src[px+2], src[px+1], src[px]]` (R,G,B from BGRX) — verify the index order in `src/render/xvfb.rs`
- [ ] `XvfbSource::render()` returns `RawFrame` with `width` and `height` matching constructor params
- [ ] `XvfbHandle::drop()` kills both xvfb and child processes and removes the temp fbdir directory
- [ ] `find_unused_display()` checks `/tmp/.X{N}-lock` file existence starting at 99
- [ ] `start()` waits for `Xvfb_screen0` file with a timeout (not infinite wait)
- [ ] Tests exist for XWD parsing, BGRX→RGB conversion, and synthetic XWD capture
- [ ] `cargo test` passes all tests

**Quality items (non-blocking):**
- [ ] `start()` handles Xvfb spawn failure gracefully (cleans up fbdir on error)
- [ ] Child process spawned with `DISPLAY=:N` environment variable
- [ ] Xvfb spawned with `-ac -nolisten tcp` flags for security
- [ ] No panic paths — all errors use `Result`/`bail!`

### Task 7: Milestone — Xvfb capture and process management complete

**Present to user:**
- `XvfbSource`: mmap-based framebuffer capture with XWD header parsing, BGRX→RGB conversion
- `XvfbManager`: spawns/owns Xvfb + child app, cleanup on drop
- All tests passing
- Not yet wired to config/D-Bus/CLI — that's Phase 3

**Wait for user response before proceeding to Task 8.**

---

### Task 8: Wire configuration, D-Bus, CLI, and daemon mode switching [READ-DO]

**Files:**
- Modify: `src/config.rs:10-68` (add mode field, XvfbConfig struct)
- Modify: `src/service/dbus.rs:12-23,36-58` (ServiceState, set_mode method)
- Modify: `src/cli.rs:24-41,59-109` (CtlCommand::Mirror, run_ctl handler, D-Bus proxy trait)
- Modify: `src/main.rs:1-170` (mode switching logic in layout/mode change listener)

**Step 1: Add config fields in `src/config.rs`**

Add `XvfbConfig` after `SensorsConfig` (after line 52):

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct XvfbConfig {
    /// Shell command to run inside the virtual display.
    pub command: String,
    /// Frame rate for xvfb capture mode (1-60 FPS).
    pub tick_rate: u32,
}

impl Default for XvfbConfig {
    fn default() -> Self {
        Self {
            command: String::new(),
            tick_rate: 15,
        }
    }
}
```

Add `mode: String` to `DisplayConfig` (line 12):

```rust
pub struct DisplayConfig {
    pub tick_rate: u32,
    pub default_layout: String,
    pub jpeg_quality: u8,
    pub rotation: u16,
    /// Display mode: "svg", "html", or "xvfb".
    pub mode: String,
}
```

Update `DisplayConfig::default()` to include `mode: "svg".to_string()`.

Add `xvfb: XvfbConfig` to the `Config` struct (line 64):

```rust
pub struct Config {
    pub display: DisplayConfig,
    pub sensors: SensorsConfig,
    pub theme: ThemeConfig,
    pub xvfb: XvfbConfig,
}
```

**Step 2: Add `ModeChange` enum for channel communication**

Add to `src/service/dbus.rs` (or a shared location — `src/service/mod.rs`):

```rust
/// Message sent through the mode change channel to switch display modes.
#[derive(Debug, Clone)]
pub enum ModeChange {
    /// Switch to an SVG or HTML layout by name.
    Layout(String),
    /// Switch to xvfb capture mode with the given shell command.
    Xvfb { command: String },
}
```

**Step 3: Update `ServiceState` in `src/service/dbus.rs`**

Rename `layout_change_tx` → `mode_change_tx` and change its type:

```rust
pub struct ServiceState {
    pub active_layout: String,
    pub mode: String,
    pub connected: bool,
    pub resolution: (u32, u32),
    pub tick_rate: u32,
    pub jpeg_quality: u8,
    pub shutdown_tx: watch::Sender<bool>,
    pub layout_dir: std::path::PathBuf,
    pub mode_change_tx: tokio::sync::mpsc::Sender<ModeChange>,
}
```

**Step 4: Update `set_layout` in `src/service/dbus.rs`**

Change from `layout_change_tx.send(name)` to `mode_change_tx.send(ModeChange::Layout(name))`.

**Step 5: Add `set_mode` D-Bus method in `src/service/dbus.rs`**

Add after `set_layout`:

```rust
    /// Switch display mode. mode="xvfb" starts capture with the given command.
    /// mode="svg" or mode="html" with command as layout name switches back to layout mode.
    async fn set_mode(&self, mode: String, command: String) -> zbus::fdo::Result<String> {
        let mut state = self.state.lock().await;
        let change = match mode.as_str() {
            "xvfb" => {
                if command.is_empty() {
                    return Err(zbus::fdo::Error::InvalidArgs(
                        "xvfb mode requires a command".to_string()
                    ));
                }
                ModeChange::Xvfb { command: command.clone() }
            }
            "svg" | "html" => {
                let layout_path = state.layout_dir.join(&command);
                if !layout_path.exists() {
                    return Err(zbus::fdo::Error::InvalidArgs(
                        format!("Layout not found: {}", command)
                    ));
                }
                ModeChange::Layout(command.clone())
            }
            _ => return Err(zbus::fdo::Error::InvalidArgs(
                format!("Unknown mode: {} (expected svg, html, or xvfb)", mode)
            )),
        };

        state.mode_change_tx.send(change).await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        state.mode = mode.clone();

        Ok(format!("Mode set to: {} ({})", mode, command))
    }
```

Update `get_status` to include mode.

**Step 6: Add `Mirror` variant to `CtlCommand` in `src/cli.rs`**

```rust
    /// Start xvfb mirror mode — capture any X11 application on the LCD.
    Mirror {
        /// Shell command to run inside the virtual display.
        command: String,
    },
```

**Step 7: Add D-Bus proxy method and handler in `src/cli.rs`**

Add to the `Display` proxy trait:

```rust
    async fn set_mode(&self, mode: &str, command: &str) -> zbus::Result<String>;
```

Add handler in `run_ctl()`:

```rust
        CtlCommand::Mirror { command } => {
            let result = proxy.set_mode("xvfb", &command).await
                .context("Failed to set mirror mode")?;
            println!("{}", result);
        }
```

**Step 8: Wire mode switching in `src/main.rs`**

This is the big integration point. The existing layout change listener (lines 133-147) becomes a mode change listener. Key changes:

1. Change the channel type from `mpsc<String>` to `mpsc<ModeChange>`
2. Import `XvfbSource` and `xvfb::start` (the xvfb manager)
3. Handle `ModeChange::Layout` (existing behavior) and `ModeChange::Xvfb` (new)
4. For xvfb mode: spawn xvfb, create XvfbSource, swap the frame source, update tick rate
5. For layout mode: drop any running xvfb handle, create SVG/HTML renderer, restore tick rate
6. The frame source and tick rate need to be swappable — use watch channels or Arc<Mutex>

The frame source swap requires the tick loop to check for a new source. Add a `watch::channel` for frame source swaps, similar to the existing template_rx pattern. Or use a more direct approach — send the new `Box<dyn FrameSource>` through a channel.

Since `Box<dyn FrameSource>` is `Send`, use `mpsc::channel::<Box<dyn FrameSource>>`:

```rust
let (source_tx, mut source_rx) = mpsc::channel::<Box<dyn FrameSource>>(1);
```

In the mode change listener:
```rust
while let Some(change) = mode_rx.recv().await {
    match change {
        ModeChange::Layout(name) => {
            // Drop existing xvfb handle if any
            xvfb_handle = None;
            // Load layout file, create renderer
            let path = layout_dir.join(&name);
            match std::fs::read_to_string(&path) {
                Ok(template) => {
                    let is_svg = name.ends_with(".svg");
                    let new_source: Box<dyn FrameSource> = if is_svg {
                        // ... create SvgRenderer ...
                        Box::new(renderer)
                    } else {
                        Box::new(TemplateRenderer::new(&template, 480, 480).unwrap())
                    };
                    let _ = source_tx.send(new_source).await;
                    info!("Switched to layout: {}", name);
                }
                Err(e) => log::warn!("Failed to read layout {}: {}", name, e),
            }
        }
        ModeChange::Xvfb { command } => {
            // Drop previous xvfb
            xvfb_handle = None;
            match crate::service::xvfb::start(&command, 480, 480) {
                Ok(handle) => {
                    match XvfbSource::new(handle.screen_file(), 480, 480) {
                        Ok(source) => {
                            let _ = source_tx.send(Box::new(source)).await;
                            xvfb_handle = Some(handle);
                            info!("Switched to xvfb mode: {}", command);
                        }
                        Err(e) => log::warn!("Failed to create XvfbSource: {}", e),
                    }
                }
                Err(e) => log::warn!("Failed to start xvfb: {}", e),
            }
        }
    }
}
```

In the tick loop, check for a new frame source each tick:

```rust
// At the top of each tick, check for a new frame source
if let Ok(new_source) = source_rx.try_recv() {
    *frame_source = new_source;
    info!("Frame source swapped");
}
```

This requires `frame_source` to be a `Box<dyn FrameSource>` passed mutably, which it already is.

Wait — the tick loop takes `frame_source: &mut dyn FrameSource`, not an owned Box. To support swapping, either:
a) Pass a `&mut Box<dyn FrameSource>` and dereference
b) Add a source_rx parameter to run_tick_loop and handle swap internally
c) Use the existing template_rx pattern (send template string, let tick loop rebuild)

Option (b) is cleanest — add source_rx to run_tick_loop's parameters. When a new source arrives, replace the current one.

Update `run_tick_loop` signature to take an owned Box + a receiver:

```rust
pub async fn run_tick_loop(
    transport: &mut dyn Transport,
    mut frame_source: Box<dyn FrameSource>,
    source_rx: &mut tokio::sync::mpsc::Receiver<Box<dyn FrameSource>>,
    // ... rest of params
) -> Result<()> {
```

And at the top of each loop iteration:

```rust
// Check for frame source swap
if let Ok(new_source) = source_rx.try_recv() {
    frame_source = new_source;
    info!("Frame source swapped to: {}", frame_source.name());
}
```

Also add a tick_rate_rx watch channel so the tick rate can change when switching to/from xvfb mode (15fps vs configured rate).

**Step 9: Handle xvfb mode on startup**

In `main.rs`, after loading config, check `config.display.mode`:

```rust
if config.display.mode == "xvfb" {
    if config.xvfb.command.is_empty() {
        anyhow::bail!("xvfb mode requires [xvfb] command in config");
    }
    let handle = crate::service::xvfb::start(&config.xvfb.command, 480, 480)?;
    let source = XvfbSource::new(handle.screen_file(), 480, 480)?;
    // ... use source as initial frame_source, store handle
}
```

**Step 10: Add CLI parse tests**

```rust
#[test]
fn cli_parses_ctl_mirror() {
    let cli = Cli::try_parse_from(["thermalwriter", "ctl", "mirror", "conky -c foo.conf"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Ctl { subcommand: CtlCommand::Mirror { ref command } } if command == "conky -c foo.conf"
    ));
}
```

**Step 11: Update `main.rs` ServiceState initialization**

Replace `layout_change_tx: layout_tx` with `mode_change_tx: mode_tx` in the ServiceState constructor.

**Step 12: Enforce 60fps cap for xvfb tick rate**

In config loading or xvfb start, clamp: `let xvfb_tick_rate = config.xvfb.tick_rate.min(60).max(1);`

**Step 13: Run full test suite**

Run: `cargo test`
Expected: All tests pass including new CLI parse test.

**Step 14: Commit**

```bash
git add -A
git commit -m "feat: wire xvfb capture mode with config, D-Bus, and CLI

Add display.mode config field (svg/html/xvfb) and [xvfb] config section.
D-Bus set_mode() switches between layout and xvfb modes at runtime.
CLI 'thermalwriter ctl mirror <command>' for xvfb mode activation.
Daemon mode change listener swaps frame source dynamically.
Xvfb tick rate defaults to 15fps, capped at 60fps."
```

### Task 9: Review Task 8

**Trigger:** Both reviewers start simultaneously when Task 8 completes.

**Killer items (blocking):**
- [ ] `XvfbConfig` in `src/config.rs` has `command: String` and `tick_rate: u32` with default 15
- [ ] `DisplayConfig` has `mode: String` with default `"svg"`
- [ ] `ModeChange` enum has `Layout(String)` and `Xvfb { command: String }` variants
- [ ] `ServiceState.mode_change_tx` is `mpsc::Sender<ModeChange>` — not `mpsc::Sender<String>`
- [ ] `set_mode` D-Bus method validates mode is "svg", "html", or "xvfb" — rejects unknown modes
- [ ] `CtlCommand::Mirror { command: String }` exists and `run_ctl` sends `set_mode("xvfb", &command)`
- [ ] Mode change listener handles both `ModeChange::Layout` and `ModeChange::Xvfb` — drops old xvfb handle before starting new one
- [ ] Xvfb tick rate capped at 60 — `config.xvfb.tick_rate.min(60)` or equivalent
- [ ] `cargo test` passes all tests

**Quality items (non-blocking):**
- [ ] `get_status` D-Bus method includes mode in output
- [ ] D-Bus proxy trait in cli.rs includes `set_mode` method
- [ ] Frame source swap in tick loop is non-blocking (`try_recv`, not `recv`)
- [ ] Error paths in mode switching log warnings (don't crash daemon)

### Task 10: Milestone — Final: full xvfb capture feature complete

**Present to user:**
- Full `thermalwriter ctl mirror "command"` support
- Config-driven mode selection (`display.mode = "xvfb"`)
- D-Bus `set_mode` for runtime switching
- Xvfb + child process lifecycle managed by daemon
- 15fps default, 60fps cap for xvfb mode
- All tests passing
- Ready for hardware testing:

```bash
# Config approach:
# Add to ~/.config/thermalwriter/config.toml:
# [display]
# mode = "xvfb"
# [xvfb]
# command = "conky -c ~/.config/conky/lcd.conf"
systemctl --user restart thermalwriter

# CLI approach:
thermalwriter ctl mirror "conky -c ~/.config/conky/lcd.conf"

# Switch back:
thermalwriter ctl layout svg/neon-dash-v2.svg
```

**Wait for user response.**
