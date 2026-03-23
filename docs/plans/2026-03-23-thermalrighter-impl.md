# thermalwriter Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use forge:executing-plans to implement this plan task-by-task.

**Goal:** Build a lightweight Rust daemon that drives Thermalright cooler LCD displays via USB, with D-Bus IPC, HTML/CSS template rendering, and extensible sensor providers.

**Architecture:** systemd user service with a tick loop that polls sensors, renders an HTML/CSS layout template into a 480x480 pixmap, encodes to JPEG, and sends via USB bulk transfer. D-Bus exposes control methods. CLI is a thin D-Bus client.

**Tech Stack:** Rust, tokio, rusb, zbus, tiny-skia, taffy, tera, fontdue, image, sysinfo, clap, toml, notify (file watcher)

**Required Skills:**
- `forge:writing-tests`: Invoke before every implementation task — covers TDD approach
- `forge:verification-before-completion`: Invoke before claiming any task complete

## Context for Executor

### Key Files (reference — existing trcc Python implementation)
- `/usr/lib/python3.14/site-packages/trcc/adapters/device/bulk.py` — USB bulk protocol implementation (handshake, frame send). This is the protocol we're reimplementing in Rust.
- `/usr/lib/python3.14/site-packages/trcc/adapters/device/_usb_helpers.py` — USB device lifecycle (find, detach kernel driver, claim interface, endpoint discovery)
- `/usr/lib/python3.14/site-packages/trcc/adapters/device/detector.py:94-101` — Device registry: VID `0x87AD`, PID `0x70DB`, protocol "bulk"
- `/usr/lib/python3.14/site-packages/trcc/core/encoding.py` — RGB565 encoding (not needed for our JPEG device, but reference)

### Research Findings

**rusb API (verified via Context7):**
- `rusb::open_device_with_vid_pid(vid, pid)` returns `Option<DeviceHandle<GlobalContext>>`
- `handle.set_auto_detach_kernel_driver(true)` handles kernel driver detachment on Linux
- `handle.claim_interface(n)` claims the USB interface
- `handle.write_bulk(endpoint, data, timeout)` for bulk OUT transfers
- `handle.read_bulk(endpoint, buf, timeout)` for bulk IN transfers
- Endpoints are discovered by iterating the active configuration's interfaces

**zbus API (verified via Context7):**
- `#[interface(name = "com.thermalwriter.Display")]` derive macro on impl block
- Methods are just `async fn` or `fn` on the impl
- Properties use `#[zbus(property)]` on getter/setter methods
- Signals use `#[zbus(signal)]` on method signatures
- `Builder::session()?.name("com.thermalwriter.Service")?.serve_at(path, obj)?.build().await?`
- Client proxy uses `#[proxy(...)]` trait derive macro

**taffy API (verified via Context7):**
- `TaffyTree::<()>::new()` creates a layout tree
- `taffy.new_leaf(style)` creates leaf nodes
- `taffy.new_with_children(style, &[children])` creates container nodes
- `taffy.compute_layout(root, Size::MAX_CONTENT)` computes the layout
- `taffy.layout(node_id)` retrieves computed position/size after layout
- CSS properties map to `Style` struct fields: `display`, `flex_direction`, `justify_content`, `align_items`, `gap`, `padding`, `margin`, `size`
- Length values use `length(px)`, `percent(pct)`, `auto()`

**tiny-skia API (verified via Context7):**
- `Pixmap::new(w, h)` creates an RGBA pixmap
- `pixmap.fill_rect(rect, &paint, transform, mask)` fills a rectangle
- `Paint::default()` + `paint.set_color_rgba8(r, g, b, a)` for solid colors
- `Rect::from_xywh(x, y, w, h)` creates rectangles
- `pixmap.data()` returns raw RGBA bytes
- No built-in text rendering — use `fontdue` to rasterize glyphs, then blit onto pixmap

**tera API (verified via Context7):**
- `Tera::one_off(template_str, context, autoescape)` for one-off rendering
- `Context::new()` + `context.insert("key", &value)` for template variables
- `{{ variable_name }}` syntax in templates

**Bulk USB Protocol (from trcc source):**
- Handshake payload: 64 bytes, `[0x12, 0x34, 0x56, 0x78, ...zeros..., byte[56]=0x01, ...zeros...]`
- Handshake response: 1024 bytes, `resp[24]` = PM (product model), `resp[36]` = SUB
- Frame header: 64 bytes:
  - `[0..4]`: magic `0x12345678` (LE)
  - `[4..8]`: cmd (2=JPEG, 3=RGB565) (LE u32)
  - `[8..12]`: width (LE u32)
  - `[12..16]`: height (LE u32)
  - `[16..56]`: zeros (padding)
  - `[56..60]`: mode = 2 (LE u32)
  - `[60..64]`: payload length (LE u32)
- Payload sent in 16KB chunks via bulk OUT
- ZLP (zero-length packet) sent after payload if total frame (header + payload) is 512-aligned
- Vendor-specific interface: `bInterfaceClass=255`
- Target device: VID=0x87AD, PID=0x70DB

### Relevant Patterns
- The existing Python `BulkDevice.send_frame()` at `/usr/lib/python3.14/site-packages/trcc/adapters/device/bulk.py:132-186` is the exact protocol we're reimplementing

### Project Structure
```
thermalwriter/
├── Cargo.toml
├── src/
│   ├── main.rs              # Entry point: daemon or CLI dispatch
│   ├── lib.rs               # Re-exports all modules
│   ├── config.rs            # TOML config parsing
│   ├── transport/
│   │   ├── mod.rs           # Transport trait
│   │   └── bulk_usb.rs      # USB bulk implementation
│   ├── sensor/
│   │   ├── mod.rs           # SensorProvider trait + SensorHub
│   │   ├── hwmon.rs         # /sys/class/hwmon reader
│   │   ├── amdgpu.rs        # /sys/class/drm AMDGPU reader
│   │   ├── sysinfo_provider.rs  # RAM via sysinfo crate
│   │   └── mangohud.rs      # MangoHud CSV tailer
│   ├── render/
│   │   ├── mod.rs           # FrameSource trait + TemplateRenderer
│   │   ├── parser.rs        # HTML subset parser → element tree
│   │   ├── layout.rs        # Element tree → taffy layout → positioned elements
│   │   └── draw.rs          # Positioned elements → tiny-skia pixmap
│   ├── service/
│   │   ├── mod.rs           # Service state + tick loop
│   │   ├── dbus.rs          # zbus interface definition
│   │   └── tick.rs          # Tick loop implementation
│   └── cli.rs               # clap subcommands + D-Bus proxy client
├── layouts/
│   ├── system-stats.html    # Default: CPU/GPU temps, power, RAM, FPS
│   ├── gpu-focus.html       # GPU-centric layout
│   └── minimal.html         # Clock + CPU temp only
├── systemd/
│   └── thermalwriter.service  # systemd user unit
└── tests/
    ├── transport_tests.rs
    ├── render_tests.rs
    ├── sensor_tests.rs
    └── integration_tests.rs
```

## Execution Architecture

**Team:** 3 devs, 1 spec reviewer, 1 quality reviewer
**Task dependencies:**
  - Tasks 1-3 (transport) are sequential
  - Tasks 6-8 (rendering) are sequential
  - Tasks 11-13 (sensors) are sequential
  - Phase 1, Phase 2, and Phase 3 are independent of each other (can run in parallel across devs)
  - Phase 4 (Tasks 16-17) depends on ALL of Phases 1-3
  - Phase 5 (Tasks 20-22) depends on Phase 4
**Phases:**
  - Phase 1: Tasks 1-5 (Project setup + USB transport)
  - Phase 2: Tasks 6-10 (Rendering pipeline)
  - Phase 3: Tasks 11-15 (Sensor system)
  - Phase 4: Tasks 16-19 (Service integration: tick loop + D-Bus)
  - Phase 5: Tasks 20-24 (CLI + config + systemd + polish)
**Milestones:**
  - After Phase 1 (Task 5): Can send a test image to the cooler LCD
  - After Phase 2 (Task 10): Can render an HTML/CSS template to a JPEG file
  - After Phase 3 (Task 15): Can poll and display live sensor readings
  - After Phase 4 (Task 19): Working daemon with D-Bus control
  - After Phase 5 (Task 24): Complete, installable system

---

## Phase 1: Project Setup + USB Transport

### Task 1: Project scaffolding and Cargo.toml [DO-CONFIRM]

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Create: `src/lib.rs`
- Create: `src/transport/mod.rs`
- Create: `src/sensor/mod.rs`
- Create: `src/render/mod.rs`
- Create: `src/service/mod.rs`

**Implement:** Initialize the Cargo project with all dependencies and module stubs. The project should compile with `cargo build` after this task.

`Cargo.toml` dependencies:
```toml
[package]
name = "thermalwriter"
version = "0.1.0"
edition = "2024"

[dependencies]
rusb = "0.9"
zbus = { version = "5", default-features = false, features = ["tokio"] }
tiny-skia = "0.11"
taffy = "0.7"
tera = "1"
fontdue = "0.9"
image = { version = "0.25", default-features = false, features = ["jpeg"] }
sysinfo = "0.33"
tokio = { version = "1", features = ["full"] }
clap = { version = "4", features = ["derive"] }
toml = "0.8"
serde = { version = "1", features = ["derive"] }
notify = "7"
log = "0.4"
env_logger = "0.11"
anyhow = "1"
thiserror = "2"

[dev-dependencies]
tempfile = "3"
```

Each module's `mod.rs` should have a comment describing its purpose and be empty otherwise. `src/main.rs` should just call `println!("thermalwriter")` and exit. `src/lib.rs` should declare the modules:
```rust
pub mod transport;
pub mod sensor;
pub mod render;
pub mod service;
```

**Confirm checklist:**
- [ ] `cargo build` succeeds with no errors
- [ ] `cargo test` runs (even with zero tests)
- [ ] All module files exist and are declared in `lib.rs`
- [ ] No unused dependency warnings (each dep will be used; warnings are OK for now since modules are stubs)
- [ ] `cargo run` prints "thermalwriter" and exits cleanly
- [ ] Committed with message "feat: project scaffolding with dependencies"

---

### Task 2: Transport trait + BulkUSB device connection [READ-DO]

**Files:**
- Create: `src/transport/bulk_usb.rs`
- Modify: `src/transport/mod.rs`
- Create: `tests/transport_tests.rs`

**Step 1: Define the Transport trait and DeviceInfo struct in `src/transport/mod.rs`**

```rust
pub mod bulk_usb;

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub vid: u16,
    pub pid: u16,
    pub width: u32,
    pub height: u32,
    pub pm: u8,
    pub sub: u8,
    pub use_jpeg: bool,
}

pub trait Transport: Send {
    /// Perform device handshake and return device info.
    fn handshake(&mut self) -> Result<DeviceInfo>;
    /// Send a frame (JPEG or RGB565 bytes depending on device).
    fn send_frame(&mut self, data: &[u8]) -> Result<()>;
    /// Release the USB device.
    fn close(&mut self);
}
```

**Step 2: Write a test for BulkUsb handshake header construction**

In `tests/transport_tests.rs`:
```rust
use thermalwriter::transport::bulk_usb;

#[test]
fn handshake_payload_is_64_bytes() {
    let payload = bulk_usb::handshake_payload();
    assert_eq!(payload.len(), 64);
    assert_eq!(payload[0], 0x12);
    assert_eq!(payload[1], 0x34);
    assert_eq!(payload[2], 0x56);
    assert_eq!(payload[3], 0x78);
    assert_eq!(payload[56], 0x01);
    // All other bytes are zero
    for i in 4..56 {
        assert_eq!(payload[i], 0x00, "byte {} should be 0x00", i);
    }
}

#[test]
fn frame_header_is_64_bytes_with_correct_fields() {
    let header = bulk_usb::build_frame_header(2, 480, 480, 12345);
    assert_eq!(header.len(), 64);
    // Magic
    assert_eq!(&header[0..4], &[0x12, 0x34, 0x56, 0x78]);
    // cmd = 2 (JPEG), little-endian u32
    assert_eq!(&header[4..8], &2u32.to_le_bytes());
    // width = 480
    assert_eq!(&header[8..12], &480u32.to_le_bytes());
    // height = 480
    assert_eq!(&header[12..16], &480u32.to_le_bytes());
    // mode = 2 at offset 56
    assert_eq!(&header[56..60], &2u32.to_le_bytes());
    // payload length = 12345 at offset 60
    assert_eq!(&header[60..64], &12345u32.to_le_bytes());
}
```

**Step 3: Run tests to verify they fail**

Run: `cargo test --test transport_tests`
Expected: FAIL — `bulk_usb::handshake_payload` and `bulk_usb::build_frame_header` don't exist yet.

**Step 4: Implement `handshake_payload()` and `build_frame_header()` in `src/transport/bulk_usb.rs`**

```rust
use std::time::Duration;
use anyhow::{Context, Result, bail};
use log::{debug, info, warn};
use rusb::{DeviceHandle, GlobalContext, UsbContext};

use super::{DeviceInfo, Transport};

const VID: u16 = 0x87AD;
const PID: u16 = 0x70DB;
const HANDSHAKE_READ_SIZE: usize = 1024;
const TIMEOUT: Duration = Duration::from_secs(1);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const CHUNK_SIZE: usize = 16 * 1024; // 16 KiB per USB bulk write

/// The 64-byte handshake payload from USBLCDNew protocol.
pub fn handshake_payload() -> [u8; 64] {
    let mut payload = [0u8; 64];
    payload[0] = 0x12;
    payload[1] = 0x34;
    payload[2] = 0x56;
    payload[3] = 0x78;
    payload[56] = 0x01;
    payload
}

/// Build the 64-byte frame header for a bulk frame send.
///
/// Layout:
///   [0..4]:   magic 0x12345678 (LE)
///   [4..8]:   cmd (2=JPEG, 3=RGB565) (LE u32)
///   [8..12]:  width (LE u32)
///   [12..16]: height (LE u32)
///   [16..56]: zeros
///   [56..60]: mode = 2 (LE u32)
///   [60..64]: payload length (LE u32)
pub fn build_frame_header(cmd: u32, width: u32, height: u32, payload_len: u32) -> [u8; 64] {
    let mut header = [0u8; 64];
    header[0..4].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);
    header[4..8].copy_from_slice(&cmd.to_le_bytes());
    header[8..12].copy_from_slice(&width.to_le_bytes());
    header[12..16].copy_from_slice(&height.to_le_bytes());
    header[56..60].copy_from_slice(&2u32.to_le_bytes());
    header[60..64].copy_from_slice(&payload_len.to_le_bytes());
    header
}

/// Resolve PM byte to (width, height). Defaults to 480x480 for unknown PMs.
fn pm_to_resolution(pm: u8) -> (u32, u32) {
    match pm {
        5 => (240, 240),
        7 | 9 => (320, 320),
        10 | 11 | 12 | 13 | 14 | 15 | 16 | 17 => (320, 240),
        32 => (480, 480),
        50 => (240, 320),
        64 | 65 | 66 => (320, 320),
        68 | 69 => (480, 480),
        _ => (480, 480), // Default for unknown PMs (including PM=4)
    }
}

pub struct BulkUsb {
    handle: Option<DeviceHandle<GlobalContext>>,
    ep_out: u8,
    ep_in: u8,
    info: Option<DeviceInfo>,
}

impl BulkUsb {
    pub fn new() -> Result<Self> {
        let handle = rusb::open_device_with_vid_pid(VID, PID)
            .context("USB device 87AD:70DB not found")?;

        handle.set_auto_detach_kernel_driver(true)
            .context("Failed to set auto-detach kernel driver")?;

        handle.claim_interface(0)
            .context("Failed to claim USB interface 0")?;

        // Discover bulk endpoints
        let device = handle.device();
        let config = device.active_config_descriptor()
            .context("Failed to get active config descriptor")?;

        let mut ep_out = 0u8;
        let mut ep_in = 0u8;

        for iface in config.interfaces() {
            for desc in iface.descriptors() {
                // Prefer vendor-specific interface (class 255)
                if desc.class_code() == 255 || desc.class_code() == 0 {
                    for ep in desc.endpoint_descriptors() {
                        if ep.transfer_type() == rusb::TransferType::Bulk {
                            if ep.direction() == rusb::Direction::Out {
                                ep_out = ep.address();
                            } else {
                                ep_in = ep.address();
                            }
                        }
                    }
                }
            }
        }

        if ep_out == 0 || ep_in == 0 {
            bail!("Could not find bulk IN/OUT endpoints");
        }

        info!("Opened BulkUSB device {:04x}:{:04x} (EP OUT=0x{:02x}, EP IN=0x{:02x})",
              VID, PID, ep_out, ep_in);

        Ok(Self {
            handle: Some(handle),
            ep_out,
            ep_in,
            info: None,
        })
    }
}

impl Transport for BulkUsb {
    fn handshake(&mut self) -> Result<DeviceInfo> {
        let handle = self.handle.as_ref().context("Device not open")?;

        // Write handshake
        let payload = handshake_payload();
        handle.write_bulk(self.ep_out, &payload, TIMEOUT)
            .context("Handshake write failed")?;
        debug!("Handshake sent ({} bytes)", payload.len());

        // Read response
        let mut resp = [0u8; HANDSHAKE_READ_SIZE];
        let n = handle.read_bulk(self.ep_in, &mut resp, TIMEOUT)
            .context("Handshake read failed")?;
        info!("Handshake response: {} bytes", n);

        if n < 41 || resp[24] == 0 {
            bail!("Handshake failed: resp[24]={} (expected non-zero)", resp[24]);
        }

        let pm = resp[24];
        let sub = resp[36];
        let (width, height) = pm_to_resolution(pm);
        let use_jpeg = pm != 32;

        info!("Handshake OK: PM={}, SUB={}, resolution={}x{}, jpeg={}",
              pm, sub, width, height, use_jpeg);

        let info = DeviceInfo {
            vid: VID,
            pid: PID,
            width,
            height,
            pm,
            sub,
            use_jpeg,
        };
        self.info = Some(info.clone());
        Ok(info)
    }

    fn send_frame(&mut self, data: &[u8]) -> Result<()> {
        let handle = self.handle.as_ref().context("Device not open")?;
        let info = self.info.as_ref().context("Handshake not performed")?;

        let cmd: u32 = if info.use_jpeg { 2 } else { 3 };
        let header = build_frame_header(cmd, info.width, info.height, data.len() as u32);

        // Concatenate header + payload
        let mut frame = Vec::with_capacity(64 + data.len());
        frame.extend_from_slice(&header);
        frame.extend_from_slice(data);

        // Send in 16KB chunks
        for chunk in frame.chunks(CHUNK_SIZE) {
            handle.write_bulk(self.ep_out, chunk, WRITE_TIMEOUT)
                .context("Bulk write failed")?;
        }

        // ZLP if total is 512-aligned
        if frame.len() % 512 == 0 {
            handle.write_bulk(self.ep_out, &[], WRITE_TIMEOUT)
                .context("ZLP write failed")?;
        }

        debug!("Frame sent: {}x{}, cmd={}, {} bytes",
               info.width, info.height, cmd, data.len());
        Ok(())
    }

    fn close(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.release_interface(0);
            info!("BulkUSB device closed");
        }
        self.info = None;
    }
}

impl Drop for BulkUsb {
    fn drop(&mut self) {
        self.close();
    }
}
```

**Step 5: Run tests to verify they pass**

Run: `cargo test --test transport_tests`
Expected: PASS (both tests)

**Step 6: Commit**

```bash
git add src/transport/ tests/transport_tests.rs
git commit -m "feat: transport trait and BulkUSB protocol implementation"
```

---

### Task 3: End-to-end device test — send a solid color frame [READ-DO]

**Files:**
- Create: `examples/send_test_frame.rs`

This task validates that the USB transport actually works with the real hardware. It's a manual test — you need the device plugged in.

**Step 1: Write an example binary that sends a red JPEG frame**

In `examples/send_test_frame.rs`:
```rust
//! Manual test: sends a solid red 480x480 JPEG frame to the cooler LCD.
//! Run with: cargo run --example send_test_frame
//! Requires the device to be plugged in and accessible.

use anyhow::Result;
use image::{ImageBuffer, Rgb};
use std::io::Cursor;
use thermalwriter::transport::{Transport, bulk_usb::BulkUsb};

fn main() -> Result<()> {
    env_logger::init();

    println!("Opening device...");
    let mut transport = BulkUsb::new()?;

    println!("Performing handshake...");
    let info = transport.handshake()?;
    println!("Device: {}x{}, PM={}, JPEG={}", info.width, info.height, info.pm, info.use_jpeg);

    // Create a solid red image
    let img = ImageBuffer::from_fn(info.width, info.height, |_x, _y| {
        Rgb([255u8, 0u8, 0u8])
    });

    // Encode to JPEG
    let mut jpeg_buf = Cursor::new(Vec::new());
    img.write_to(&mut jpeg_buf, image::ImageFormat::Jpeg)?;
    let jpeg_data = jpeg_buf.into_inner();
    println!("JPEG encoded: {} bytes", jpeg_data.len());

    // Send frame
    println!("Sending frame...");
    transport.send_frame(&jpeg_data)?;
    println!("Done! The display should now show solid red.");

    transport.close();
    Ok(())
}
```

**Step 2: Run the example with the device plugged in**

Run: `cargo run --example send_test_frame`
Expected: The cooler LCD displays a solid red screen. Console output shows handshake succeeded and frame was sent.

If the device is not plugged in, the example should fail with "USB device 87AD:70DB not found".

**Step 3: Commit**

```bash
git add examples/send_test_frame.rs
git commit -m "feat: add send_test_frame example for hardware validation"
```

---

### Task 4: Review Tasks 1-3

**Trigger:** Both reviewers start simultaneously when Tasks 1-3 complete.

**Killer items (blocking):**
- [ ] Handshake payload bytes in `bulk_usb::handshake_payload()` exactly match the Python reference: bytes 0-3 are `[0x12, 0x34, 0x56, 0x78]`, byte 56 is `0x01`, all others are `0x00`
- [ ] Frame header layout in `build_frame_header()` matches Python `BulkDevice.send_frame()` at `/usr/lib/python3.14/site-packages/trcc/adapters/device/bulk.py:139-163` — verify offsets 0, 4, 8, 12, 56, 60 with `struct.pack_into` calls
- [ ] Frame chunking uses 16KB chunks (`CHUNK_SIZE = 16 * 1024`), matching Python `_WRITE_CHUNK_SIZE`
- [ ] ZLP is sent only when `frame.len() % 512 == 0`, matching Python `if len(frame) % 512 == 0`
- [ ] `BulkUsb::new()` calls `set_auto_detach_kernel_driver(true)` before `claim_interface` — failing to do this causes EBUSY on Linux when kernel drivers are attached
- [ ] `Transport` trait is `Send` — required because the tick loop and D-Bus handler may be on different tokio tasks
- [ ] `Drop` implementation calls `close()` — USB device handle must be released on panic/early return
- [ ] Tests in `transport_tests.rs` assert exact byte values, not just lengths

**Quality items (non-blocking):**
- [ ] Error messages include device VID:PID for debuggability
- [ ] `pm_to_resolution` covers all PMs from the Python `_BULK_KNOWN_PMS` set
- [ ] Logging follows consistent pattern (info for lifecycle events, debug for per-frame)

**Validation Data:**
- Compare `handshake_payload()` output byte-by-byte against `_HANDSHAKE_PAYLOAD` in `/usr/lib/python3.14/site-packages/trcc/adapters/device/bulk.py:30-38`
- Compare `build_frame_header(2, 480, 480, N)` against Python `header` construction in `send_frame()` at bulk.py:158-163

**Resolution:** Killer item findings block merge; quality item findings queue for dev. All killer items must resolve before next milestone.

---

### Task 5: Milestone — USB transport works

**Present to user:**
- Transport trait defined with `handshake()`, `send_frame()`, `close()`
- BulkUSB implementation handles full protocol: device discovery, kernel driver detach, endpoint detection, handshake, chunked frame send with ZLP
- Unit tests verify handshake payload and frame header byte layouts
- Example binary (`send_test_frame`) validates real hardware communication
- Review findings and resolutions

**Wait for user response before proceeding to Task 6.**

---

## Phase 2: Rendering Pipeline

### Task 6: HTML/CSS subset parser [READ-DO]

**Files:**
- Create: `src/render/parser.rs`
- Modify: `src/render/mod.rs`
- Create: `tests/render_tests.rs`

**Step 1: Define the element tree data structures in `src/render/parser.rs`**

```rust
use std::collections::HashMap;

/// A parsed CSS color.
#[derive(Debug, Clone, PartialEq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub fn from_hex(hex: &str) -> Option<Self> {
        let hex = hex.trim_start_matches('#');
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(Self { r, g, b, a: 255 })
        } else {
            None
        }
    }

    pub fn white() -> Self { Self { r: 255, g: 255, b: 255, a: 255 } }
    pub fn black() -> Self { Self { r: 0, g: 0, b: 0, a: 255 } }
    pub fn transparent() -> Self { Self { r: 0, g: 0, b: 0, a: 0 } }
}

/// Parsed inline styles relevant to our subset.
#[derive(Debug, Clone, Default)]
pub struct ElementStyle {
    pub display: Option<String>,          // "flex", "block"
    pub flex_direction: Option<String>,    // "row", "column"
    pub justify_content: Option<String>,   // "center", "space-between", etc.
    pub align_items: Option<String>,       // "center", "flex-start", etc.
    pub gap: Option<f32>,                  // px
    pub padding: Option<f32>,             // px (uniform for now)
    pub margin: Option<f32>,              // px (uniform for now)
    pub font_size: Option<f32>,           // px
    pub font_family: Option<String>,
    pub color: Option<Color>,
    pub background: Option<Color>,
    pub text_align: Option<String>,       // "left", "center", "right"
    pub border_radius: Option<f32>,       // px
    pub width: Option<f32>,               // px
    pub height: Option<f32>,              // px
}

/// A node in the parsed element tree.
#[derive(Debug, Clone)]
pub struct Element {
    pub tag: String,
    pub style: ElementStyle,
    pub text: Option<String>,
    pub children: Vec<Element>,
}

/// Parse an HTML string (our subset) into an element tree.
pub fn parse_html(html: &str) -> anyhow::Result<Element> {
    // Implementation in next step
    todo!()
}

/// Parse a CSS inline style string into an ElementStyle.
pub fn parse_style(style_str: &str) -> ElementStyle {
    // Implementation in next step
    todo!()
}
```

**Step 2: Write tests for the parser**

In `tests/render_tests.rs`:
```rust
use thermalwriter::render::parser::*;

#[test]
fn parse_style_extracts_flex_properties() {
    let style = parse_style("display: flex; flex-direction: column; gap: 8px;");
    assert_eq!(style.display.as_deref(), Some("flex"));
    assert_eq!(style.flex_direction.as_deref(), Some("column"));
    assert_eq!(style.gap, Some(8.0));
}

#[test]
fn parse_style_extracts_colors() {
    let style = parse_style("color: #ff0000; background: #1a1a2e;");
    let color = style.color.unwrap();
    assert_eq!((color.r, color.g, color.b), (255, 0, 0));
    let bg = style.background.unwrap();
    assert_eq!((bg.r, bg.g, bg.b), (0x1a, 0x1a, 0x2e));
}

#[test]
fn parse_style_extracts_font_size() {
    let style = parse_style("font-size: 24px; font-family: monospace;");
    assert_eq!(style.font_size, Some(24.0));
    assert_eq!(style.font_family.as_deref(), Some("monospace"));
}

#[test]
fn parse_html_single_div_with_text() {
    let el = parse_html(r#"<div style="color: #fff;">Hello</div>"#).unwrap();
    assert_eq!(el.tag, "div");
    assert_eq!(el.text.as_deref(), Some("Hello"));
    assert_eq!(el.style.color.as_ref().unwrap().r, 255);
}

#[test]
fn parse_html_nested_elements() {
    let html = r#"<div style="display: flex;">
        <span>CPU 65C</span>
        <span>GPU 72C</span>
    </div>"#;
    let el = parse_html(html).unwrap();
    assert_eq!(el.tag, "div");
    assert_eq!(el.children.len(), 2);
    assert_eq!(el.children[0].text.as_deref(), Some("CPU 65C"));
    assert_eq!(el.children[1].text.as_deref(), Some("GPU 72C"));
}
```

**Step 3: Run tests to verify they fail**

Run: `cargo test --test render_tests`
Expected: FAIL — `parse_html` and `parse_style` are `todo!()`.

**Step 4: Implement `parse_style()`**

Replace the `todo!()` in `parse_style` with a simple CSS property parser:
```rust
pub fn parse_style(style_str: &str) -> ElementStyle {
    let mut style = ElementStyle::default();
    for decl in style_str.split(';') {
        let decl = decl.trim();
        if decl.is_empty() { continue; }
        let mut parts = decl.splitn(2, ':');
        let prop = parts.next().unwrap_or("").trim();
        let val = parts.next().unwrap_or("").trim();
        match prop {
            "display" => style.display = Some(val.to_string()),
            "flex-direction" => style.flex_direction = Some(val.to_string()),
            "justify-content" => style.justify_content = Some(val.to_string()),
            "align-items" => style.align_items = Some(val.to_string()),
            "text-align" => style.text_align = Some(val.to_string()),
            "font-family" => style.font_family = Some(val.to_string()),
            "gap" => style.gap = parse_px(val),
            "padding" => style.padding = parse_px(val),
            "margin" => style.margin = parse_px(val),
            "font-size" => style.font_size = parse_px(val),
            "border-radius" => style.border_radius = parse_px(val),
            "width" => style.width = parse_px(val),
            "height" => style.height = parse_px(val),
            "color" => style.color = Color::from_hex(val),
            "background" => style.background = Color::from_hex(val),
            _ => {} // Ignore unknown properties
        }
    }
    style
}

fn parse_px(val: &str) -> Option<f32> {
    val.trim_end_matches("px").trim().parse().ok()
}
```

**Step 5: Implement `parse_html()`**

A simple recursive descent parser for our HTML subset. No need for a full HTML parser — we only handle `<div>`, `<span>`, self-closing isn't needed, and attributes are limited to `style=""`.

```rust
pub fn parse_html(html: &str) -> anyhow::Result<Element> {
    let html = html.trim();
    let mut parser = HtmlParser::new(html);
    parser.parse_element()
}

struct HtmlParser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> HtmlParser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn remaining(&self) -> &'a str {
        &self.input[self.pos..]
    }

    fn skip_whitespace(&mut self) {
        while self.pos < self.input.len()
            && self.input.as_bytes()[self.pos].is_ascii_whitespace()
        {
            self.pos += 1;
        }
    }

    fn starts_with(&self, s: &str) -> bool {
        self.remaining().starts_with(s)
    }

    fn parse_element(&mut self) -> anyhow::Result<Element> {
        self.skip_whitespace();
        anyhow::ensure!(self.starts_with("<"), "Expected '<', got {:?}", &self.remaining()[..20.min(self.remaining().len())]);

        // Parse opening tag
        self.pos += 1; // skip '<'
        let tag = self.parse_tag_name();
        let style = self.parse_attributes();
        self.skip_whitespace();

        // Skip '>'
        anyhow::ensure!(self.starts_with(">"), "Expected '>'");
        self.pos += 1;

        // Parse children and text
        let mut children = Vec::new();
        let mut text_parts = Vec::new();

        loop {
            self.skip_whitespace();
            if self.starts_with(&format!("</{}", tag)) {
                // Closing tag
                self.pos += 2 + tag.len(); // skip '</' + tag
                self.skip_whitespace();
                if self.starts_with(">") { self.pos += 1; }
                break;
            } else if self.starts_with("<") {
                // Child element
                children.push(self.parse_element()?);
            } else {
                // Text content
                let start = self.pos;
                while self.pos < self.input.len() && !self.starts_with("<") {
                    self.pos += 1;
                }
                let t = self.input[start..self.pos].trim();
                if !t.is_empty() {
                    text_parts.push(t.to_string());
                }
            }
        }

        let text = if text_parts.is_empty() { None } else { Some(text_parts.join(" ")) };

        Ok(Element { tag, style, text, children })
    }

    fn parse_tag_name(&mut self) -> String {
        let start = self.pos;
        while self.pos < self.input.len() {
            let ch = self.input.as_bytes()[self.pos];
            if ch.is_ascii_alphanumeric() || ch == b'-' {
                self.pos += 1;
            } else {
                break;
            }
        }
        self.input[start..self.pos].to_string()
    }

    fn parse_attributes(&mut self) -> ElementStyle {
        self.skip_whitespace();
        let mut style = ElementStyle::default();

        while self.pos < self.input.len() && !self.starts_with(">") {
            self.skip_whitespace();
            if self.starts_with(">") { break; }

            let attr_name = self.parse_tag_name();
            self.skip_whitespace();
            if self.starts_with("=") {
                self.pos += 1; // skip '='
                self.skip_whitespace();
                let value = self.parse_attr_value();
                if attr_name == "style" {
                    style = parse_style(&value);
                }
            }
        }

        style
    }

    fn parse_attr_value(&mut self) -> String {
        self.skip_whitespace();
        let quote = self.input.as_bytes()[self.pos];
        if quote == b'"' || quote == b'\'' {
            self.pos += 1;
            let start = self.pos;
            while self.pos < self.input.len() && self.input.as_bytes()[self.pos] != quote {
                self.pos += 1;
            }
            let val = self.input[start..self.pos].to_string();
            if self.pos < self.input.len() { self.pos += 1; } // skip closing quote
            val
        } else {
            let start = self.pos;
            while self.pos < self.input.len()
                && !self.input.as_bytes()[self.pos].is_ascii_whitespace()
                && self.input.as_bytes()[self.pos] != b'>'
            {
                self.pos += 1;
            }
            self.input[start..self.pos].to_string()
        }
    }
}
```

**Step 6: Update `src/render/mod.rs`**

```rust
pub mod parser;
pub mod layout;
pub mod draw;
```

**Step 7: Run tests**

Run: `cargo test --test render_tests`
Expected: ALL PASS

**Step 8: Commit**

```bash
git add src/render/ tests/render_tests.rs
git commit -m "feat: HTML/CSS subset parser for layout templates"
```

---

### Task 7: Layout computation with taffy [READ-DO]

**Files:**
- Create: `src/render/layout.rs`
- Modify: `tests/render_tests.rs`

**Step 1: Write tests for layout computation**

Add to `tests/render_tests.rs`:
```rust
use thermalwriter::render::layout::*;
use thermalwriter::render::parser::*;

#[test]
fn layout_single_element_fills_container() {
    let el = parse_html(r#"<div style="width: 480px; height: 480px;">Hello</div>"#).unwrap();
    let nodes = compute_layout(&el, 480.0, 480.0).unwrap();
    assert_eq!(nodes.len(), 1);
    assert!((nodes[0].x - 0.0).abs() < 1.0);
    assert!((nodes[0].y - 0.0).abs() < 1.0);
    assert!((nodes[0].width - 480.0).abs() < 1.0);
    assert!((nodes[0].height - 480.0).abs() < 1.0);
}

#[test]
fn layout_flex_column_stacks_children() {
    let html = r#"<div style="display: flex; flex-direction: column; width: 480px; height: 480px;">
        <div style="height: 100px;">Top</div>
        <div style="height: 100px;">Bottom</div>
    </div>"#;
    let el = parse_html(html).unwrap();
    let nodes = compute_layout(&el, 480.0, 480.0).unwrap();
    // Find children by text
    let top = nodes.iter().find(|n| n.text.as_deref() == Some("Top")).unwrap();
    let bottom = nodes.iter().find(|n| n.text.as_deref() == Some("Bottom")).unwrap();
    assert!(bottom.y > top.y, "Bottom should be below Top");
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --test render_tests layout`
Expected: FAIL — `layout` module doesn't exist yet.

**Step 3: Implement layout computation in `src/render/layout.rs`**

```rust
use anyhow::Result;
use taffy::prelude::*;

use super::parser::{Color, Element, ElementStyle};

/// A positioned, renderable node (output of layout computation).
#[derive(Debug, Clone)]
pub struct LayoutNode {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub text: Option<String>,
    pub style: ElementStyle,
}

/// Convert our ElementStyle to a taffy Style.
fn to_taffy_style(es: &ElementStyle) -> Style {
    let mut style = Style::default();

    match es.display.as_deref() {
        Some("flex") => style.display = Display::Flex,
        _ => style.display = Display::Flex, // Default to flex
    }

    match es.flex_direction.as_deref() {
        Some("column") => style.flex_direction = FlexDirection::Column,
        Some("row-reverse") => style.flex_direction = FlexDirection::RowReverse,
        Some("column-reverse") => style.flex_direction = FlexDirection::ColumnReverse,
        _ => style.flex_direction = FlexDirection::Row,
    }

    match es.justify_content.as_deref() {
        Some("center") => style.justify_content = Some(JustifyContent::Center),
        Some("space-between") => style.justify_content = Some(JustifyContent::SpaceBetween),
        Some("space-around") => style.justify_content = Some(JustifyContent::SpaceAround),
        Some("flex-end") => style.justify_content = Some(JustifyContent::FlexEnd),
        _ => {}
    }

    match es.align_items.as_deref() {
        Some("center") => style.align_items = Some(AlignItems::Center),
        Some("flex-start") => style.align_items = Some(AlignItems::FlexStart),
        Some("flex-end") => style.align_items = Some(AlignItems::FlexEnd),
        Some("stretch") => style.align_items = Some(AlignItems::Stretch),
        _ => {}
    }

    if let Some(gap) = es.gap {
        style.gap = Size { width: length(gap), height: length(gap) };
    }

    if let Some(p) = es.padding {
        let lp = LengthPercentage::Length(p);
        style.padding = Rect { left: lp, right: lp, top: lp, bottom: lp };
    }

    if let Some(m) = es.margin {
        let lpa = LengthPercentageAuto::Length(m);
        style.margin = Rect { left: lpa, right: lpa, top: lpa, bottom: lpa };
    }

    if let Some(w) = es.width {
        style.size.width = length(w);
    }
    if let Some(h) = es.height {
        style.size.height = length(h);
    }

    style
}

/// Recursively build taffy nodes from our element tree.
fn build_taffy_tree(
    taffy: &mut TaffyTree<usize>,
    element: &Element,
    nodes_out: &mut Vec<(NodeId, Element)>,
) -> Result<NodeId> {
    let taffy_style = to_taffy_style(&element.style);

    if element.children.is_empty() {
        // Leaf node
        let node = taffy.new_leaf(taffy_style)?;
        nodes_out.push((node, element.clone()));
        Ok(node)
    } else {
        // Container with children
        let mut child_ids = Vec::new();
        for child in &element.children {
            let child_id = build_taffy_tree(taffy, child, nodes_out)?;
            child_ids.push(child_id);
        }
        let node = taffy.new_with_children(taffy_style, &child_ids)?;
        nodes_out.push((node, element.clone()));
        Ok(node)
    }
}

/// Compute layout for an element tree. Returns flat list of positioned nodes.
pub fn compute_layout(root: &Element, container_w: f32, container_h: f32) -> Result<Vec<LayoutNode>> {
    let mut taffy: TaffyTree<usize> = TaffyTree::new();
    let mut node_map: Vec<(NodeId, Element)> = Vec::new();

    let root_id = build_taffy_tree(&mut taffy, root, &mut node_map)?;

    taffy.compute_layout(root_id, Size {
        width: AvailableSpace::Definite(container_w),
        height: AvailableSpace::Definite(container_h),
    })?;

    // Collect layout results with absolute positions
    let mut result = Vec::new();
    collect_layout_nodes(&taffy, root_id, 0.0, 0.0, &node_map, &mut result);
    Ok(result)
}

fn collect_layout_nodes(
    taffy: &TaffyTree<usize>,
    node_id: NodeId,
    parent_x: f32,
    parent_y: f32,
    node_map: &[(NodeId, Element)],
    out: &mut Vec<LayoutNode>,
) {
    let layout = taffy.layout(node_id).unwrap();
    let abs_x = parent_x + layout.location.x;
    let abs_y = parent_y + layout.location.y;

    if let Some((_, element)) = node_map.iter().find(|(id, _)| *id == node_id) {
        out.push(LayoutNode {
            x: abs_x,
            y: abs_y,
            width: layout.size.width,
            height: layout.size.height,
            text: element.text.clone(),
            style: element.style.clone(),
        });
    }

    for &child_id in taffy.children(node_id).unwrap().iter() {
        collect_layout_nodes(taffy, child_id, abs_x, abs_y, node_map, out);
    }
}
```

**Step 4: Run tests**

Run: `cargo test --test render_tests layout`
Expected: PASS

**Step 5: Commit**

```bash
git add src/render/layout.rs tests/render_tests.rs
git commit -m "feat: taffy-based layout computation from element tree"
```

---

### Task 8: Rendering with tiny-skia + fontdue [READ-DO]

**Files:**
- Create: `src/render/draw.rs`
- Modify: `src/render/mod.rs` — add `FrameSource` trait and `TemplateRenderer`
- Modify: `tests/render_tests.rs`

**Step 1: Write a test for rendering a simple layout to pixels**

Add to `tests/render_tests.rs`:
```rust
use thermalwriter::render::{FrameSource, TemplateRenderer};
use std::collections::HashMap;

#[test]
fn template_renderer_produces_480x480_pixmap() {
    let layout_html = r#"<div style="display: flex; flex-direction: column; padding: 12px; background: #1a1a2e; color: #ffffff; font-size: 24px;">
        <span>CPU {{ cpu_temp }}C</span>
    </div>"#;

    let mut renderer = TemplateRenderer::new(layout_html, 480, 480).unwrap();
    let mut sensors = HashMap::new();
    sensors.insert("cpu_temp".to_string(), "65".to_string());

    let pixmap = renderer.render(&sensors).unwrap();
    assert_eq!(pixmap.width(), 480);
    assert_eq!(pixmap.height(), 480);
    // Verify the background isn't all black (it should be #1a1a2e)
    let pixel = &pixmap.data()[0..4]; // first pixel RGBA
    assert!(pixel[0] > 0 || pixel[1] > 0 || pixel[2] > 0, "Background should not be black");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test render_tests template_renderer`
Expected: FAIL

**Step 3: Implement the drawing module in `src/render/draw.rs`**

```rust
use tiny_skia::*;
use fontdue::{Font, FontSettings};
use once_cell::sync::Lazy;

use super::layout::LayoutNode;
use super::parser::Color as ElementColor;

// Embed a default font at compile time
const DEFAULT_FONT_BYTES: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");

static DEFAULT_FONT: Lazy<Font> = Lazy::new(|| {
    Font::from_bytes(DEFAULT_FONT_BYTES, FontSettings::default())
        .expect("Failed to load embedded font")
});

/// Render a list of positioned layout nodes into a tiny-skia Pixmap.
pub fn render_nodes(nodes: &[LayoutNode], width: u32, height: u32) -> Pixmap {
    let mut pixmap = Pixmap::new(width, height).unwrap();

    for node in nodes {
        // Draw background
        if let Some(ref bg) = node.style.background {
            let mut paint = Paint::default();
            paint.set_color_rgba8(bg.r, bg.g, bg.b, bg.a);
            if let Some(rect) = Rect::from_xywh(node.x, node.y, node.width, node.height) {
                pixmap.fill_rect(rect, &paint, Transform::identity(), None);
            }
        }

        // Draw text
        if let Some(ref text) = node.text {
            let font_size = node.style.font_size.unwrap_or(16.0);
            let color = node.style.color.as_ref().cloned().unwrap_or(ElementColor::white());
            let text_align = node.style.text_align.as_deref().unwrap_or("left");

            draw_text(
                &mut pixmap,
                text,
                node.x,
                node.y,
                node.width,
                node.height,
                font_size,
                &color,
                text_align,
            );
        }
    }

    pixmap
}

fn draw_text(
    pixmap: &mut Pixmap,
    text: &str,
    x: f32,
    y: f32,
    container_w: f32,
    container_h: f32,
    font_size: f32,
    color: &ElementColor,
    text_align: &str,
) {
    let font = &*DEFAULT_FONT;

    // Rasterize each glyph and compute total text width
    let mut glyphs: Vec<(fontdue::Metrics, Vec<u8>)> = Vec::new();
    let mut total_width = 0.0f32;

    for ch in text.chars() {
        let (metrics, bitmap) = font.rasterize(ch, font_size);
        total_width += metrics.advance_width;
        glyphs.push((metrics, bitmap));
    }

    // Compute starting X based on text-align
    let start_x = match text_align {
        "center" => x + (container_w - total_width) / 2.0,
        "right" => x + container_w - total_width,
        _ => x, // "left" or default
    };

    // Vertically center the text in the container
    let line_height = font_size;
    let start_y = y + (container_h - line_height) / 2.0;

    let mut cursor_x = start_x;

    for (metrics, bitmap) in &glyphs {
        let glyph_x = cursor_x + metrics.xmin as f32;
        let glyph_y = start_y + (font_size - metrics.height as f32 - metrics.ymin as f32);

        // Blit glyph bitmap onto pixmap
        for row in 0..metrics.height {
            for col in 0..metrics.width {
                let alpha = bitmap[row * metrics.width + col];
                if alpha == 0 { continue; }

                let px = (glyph_x + col as f32) as i32;
                let py = (glyph_y + row as f32) as i32;

                if px < 0 || py < 0 || px >= pixmap.width() as i32 || py >= pixmap.height() as i32 {
                    continue;
                }

                let idx = (py as u32 * pixmap.width() + px as u32) as usize * 4;
                let data = pixmap.data_mut();

                // Alpha blend the glyph
                let a = alpha as u16;
                let inv_a = 255 - a;
                data[idx] = ((color.r as u16 * a + data[idx] as u16 * inv_a) / 255) as u8;
                data[idx + 1] = ((color.g as u16 * a + data[idx + 1] as u16 * inv_a) / 255) as u8;
                data[idx + 2] = ((color.b as u16 * a + data[idx + 2] as u16 * inv_a) / 255) as u8;
                data[idx + 3] = 255;
            }
        }

        cursor_x += metrics.advance_width;
    }
}
```

**Step 4: Add the FrameSource trait and TemplateRenderer to `src/render/mod.rs`**

```rust
pub mod parser;
pub mod layout;
pub mod draw;

use std::collections::HashMap;
use anyhow::Result;
use tiny_skia::Pixmap;

/// Sensor data: flat map of key → string value.
pub type SensorData = HashMap<String, String>;

/// A source that produces frames for the display.
pub trait FrameSource: Send {
    fn render(&mut self, sensors: &SensorData) -> Result<Pixmap>;
    fn name(&self) -> &str;
}

/// Renders HTML/CSS templates with sensor data substitution.
pub struct TemplateRenderer {
    template: String,
    width: u32,
    height: u32,
}

impl TemplateRenderer {
    pub fn new(template: &str, width: u32, height: u32) -> Result<Self> {
        Ok(Self {
            template: template.to_string(),
            width,
            height,
        })
    }

    pub fn set_template(&mut self, template: &str) {
        self.template = template.to_string();
    }
}

impl FrameSource for TemplateRenderer {
    fn render(&mut self, sensors: &SensorData) -> Result<Pixmap> {
        // Step 1: Template substitution
        let mut context = tera::Context::new();
        for (key, value) in sensors {
            context.insert(key, value);
        }
        let rendered = tera::Tera::one_off(&self.template, &context, false)?;

        // Step 2: Parse HTML
        let root = parser::parse_html(&rendered)?;

        // Step 3: Compute layout
        let nodes = layout::compute_layout(&root, self.width as f32, self.height as f32)?;

        // Step 4: Render to pixmap
        let pixmap = draw::render_nodes(&nodes, self.width, self.height);

        Ok(pixmap)
    }

    fn name(&self) -> &str {
        "template"
    }
}
```

**Step 5: Download a font for embedding**

Run: `mkdir -p assets/fonts && curl -L -o assets/fonts/JetBrainsMono-Regular.ttf "https://github.com/JetBrains/JetBrainsMono/raw/master/fonts/ttf/JetBrainsMono-Regular.ttf"`

If the download fails, use any monospace TTF font available on the system. The font is embedded at compile time via `include_bytes!`.

**Step 6: Run tests**

Run: `cargo test --test render_tests`
Expected: ALL PASS

**Step 7: Commit**

```bash
git add src/render/ tests/render_tests.rs assets/
git commit -m "feat: tiny-skia rendering with fontdue text and TemplateRenderer"
```

---

### Task 9: Review Tasks 6-8

**Trigger:** Both reviewers start simultaneously when Tasks 6-8 complete.

**Killer items (blocking):**
- [ ] `parse_style()` correctly parses all CSS properties listed in the design doc: `display`, `flex-direction`, `justify-content`, `align-items`, `gap`, `padding`, `margin`, `font-size`, `font-family`, `color`, `background`, `text-align`, `border-radius`
- [ ] `parse_html()` handles nested elements (div containing div containing span) — test with 3 levels of nesting
- [ ] `compute_layout()` produces absolute coordinates, not relative — verify by checking a deeply nested child's `x`/`y` includes all parent offsets
- [ ] `TemplateRenderer::render()` calls `tera::Tera::one_off` with `autoescape: false` — autoescaping would mangle `<div>` tags in the template output
- [ ] Text rendering handles empty strings without panic — test `render_nodes` with `text: Some("".to_string())`
- [ ] Font file is embedded via `include_bytes!` and exists at `assets/fonts/JetBrainsMono-Regular.ttf` — missing file causes compile error
- [ ] `FrameSource` trait is `Send` — required for use across tokio tasks

**Quality items (non-blocking):**
- [ ] `parse_html` provides useful error messages (not just "Expected '<'") when given malformed input
- [ ] `ElementStyle` implements `Default` — needed for elements without inline styles
- [ ] No `unwrap()` calls in rendering hot path (use `?` or handle gracefully)

**Validation Data:**
- Render the example layout from the design doc with dummy sensor values and verify the output pixmap is 480x480 with non-zero pixel data
- Parse `style="display: flex; flex-direction: column; gap: 8px; padding: 12px; background: #1a1a2e; color: #fff;"` and verify all 6 properties are extracted

**Resolution:** Killer item findings block merge; quality item findings queue for dev.

---

### Task 10: Milestone — rendering pipeline works

**Present to user:**
- HTML/CSS subset parser handles div/span with inline styles
- taffy computes flexbox layout positions from parsed elements
- tiny-skia renders backgrounds and text with fontdue glyph rasterization
- TemplateRenderer integrates the full pipeline: tera substitution → parse → layout → render
- Review findings and resolutions

**Wait for user response before proceeding to Task 11.**

---

## Phase 3: Sensor System

### Task 11: SensorProvider trait + HwmonProvider [READ-DO]

**Files:**
- Create: `src/sensor/hwmon.rs`
- Modify: `src/sensor/mod.rs`
- Create: `tests/sensor_tests.rs`

**Step 1: Define the SensorProvider trait and SensorHub in `src/sensor/mod.rs`**

```rust
pub mod hwmon;
pub mod amdgpu;
pub mod sysinfo_provider;
pub mod mangohud;

use std::collections::HashMap;
use anyhow::Result;

#[derive(Debug, Clone)]
pub struct SensorReading {
    pub key: String,
    pub value: String,
    pub unit: String,
}

#[derive(Debug, Clone)]
pub struct SensorDescriptor {
    pub key: String,
    pub name: String,
    pub unit: String,
}

pub trait SensorProvider: Send {
    fn name(&self) -> &str;
    fn poll(&mut self) -> Result<Vec<SensorReading>>;
    fn available_sensors(&self) -> Vec<SensorDescriptor>;
}

/// Aggregates all sensor providers and exposes a flat key→value map.
pub struct SensorHub {
    providers: Vec<Box<dyn SensorProvider>>,
}

impl SensorHub {
    pub fn new() -> Self {
        Self { providers: Vec::new() }
    }

    pub fn add_provider(&mut self, provider: Box<dyn SensorProvider>) {
        self.providers.push(provider);
    }

    /// Poll all providers and return aggregated sensor data.
    pub fn poll(&mut self) -> HashMap<String, String> {
        let mut data = HashMap::new();
        for provider in &mut self.providers {
            match provider.poll() {
                Ok(readings) => {
                    for reading in readings {
                        data.insert(reading.key, reading.value);
                    }
                }
                Err(e) => {
                    log::warn!("Sensor provider '{}' failed: {}", provider.name(), e);
                }
            }
        }
        data
    }

    pub fn available_sensors(&self) -> Vec<SensorDescriptor> {
        self.providers.iter().flat_map(|p| p.available_sensors()).collect()
    }
}
```

**Step 2: Write tests for HwmonProvider**

In `tests/sensor_tests.rs`:
```rust
use thermalwriter::sensor::hwmon::HwmonProvider;
use thermalwriter::sensor::SensorProvider;
use std::fs;
use tempfile::TempDir;

#[test]
fn hwmon_reads_temperature_from_sysfs() {
    let tmp = TempDir::new().unwrap();
    let hwmon_dir = tmp.path().join("hwmon0");
    fs::create_dir_all(&hwmon_dir).unwrap();
    fs::write(hwmon_dir.join("name"), "coretemp\n").unwrap();
    fs::write(hwmon_dir.join("temp1_input"), "65000\n").unwrap(); // 65°C in millidegrees
    fs::write(hwmon_dir.join("temp1_label"), "Package id 0\n").unwrap();

    let mut provider = HwmonProvider::with_base_path(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    let cpu_temp = readings.iter().find(|r| r.key.contains("temp")).unwrap();
    assert_eq!(cpu_temp.value, "65");
    assert_eq!(cpu_temp.unit, "°C");
}

#[test]
fn hwmon_reads_fan_speed_from_sysfs() {
    let tmp = TempDir::new().unwrap();
    let hwmon_dir = tmp.path().join("hwmon1");
    fs::create_dir_all(&hwmon_dir).unwrap();
    fs::write(hwmon_dir.join("name"), "nct6798\n").unwrap();
    fs::write(hwmon_dir.join("fan1_input"), "1200\n").unwrap(); // RPM
    fs::write(hwmon_dir.join("fan1_label"), "CPU Fan\n").unwrap();

    let mut provider = HwmonProvider::with_base_path(tmp.path().to_path_buf());
    let readings = provider.poll().unwrap();

    let fan = readings.iter().find(|r| r.key.contains("fan")).unwrap();
    assert_eq!(fan.value, "1200");
    assert_eq!(fan.unit, "RPM");
}
```

**Step 3: Run tests to verify they fail**

Run: `cargo test --test sensor_tests`
Expected: FAIL

**Step 4: Implement HwmonProvider in `src/sensor/hwmon.rs`**

```rust
use std::fs;
use std::path::PathBuf;
use anyhow::Result;

use super::{SensorDescriptor, SensorProvider, SensorReading};

const DEFAULT_HWMON_PATH: &str = "/sys/class/hwmon";

pub struct HwmonProvider {
    base_path: PathBuf,
}

impl HwmonProvider {
    pub fn new() -> Self {
        Self { base_path: PathBuf::from(DEFAULT_HWMON_PATH) }
    }

    /// For testing with a fake sysfs tree.
    pub fn with_base_path(base: PathBuf) -> Self {
        Self { base_path: base }
    }

    fn read_file_trimmed(path: &std::path::Path) -> Option<String> {
        fs::read_to_string(path).ok().map(|s| s.trim().to_string())
    }
}

impl SensorProvider for HwmonProvider {
    fn name(&self) -> &str {
        "hwmon"
    }

    fn poll(&mut self) -> Result<Vec<SensorReading>> {
        let mut readings = Vec::new();
        let entries = fs::read_dir(&self.base_path)?;

        for entry in entries.flatten() {
            let hwmon_dir = entry.path();
            let chip_name = Self::read_file_trimmed(&hwmon_dir.join("name"))
                .unwrap_or_else(|| "unknown".to_string());

            // Read temperatures (temp*_input files, millidegrees C)
            for i in 1..=16 {
                let input = hwmon_dir.join(format!("temp{}_input", i));
                if let Some(val_str) = Self::read_file_trimmed(&input) {
                    if let Ok(millideg) = val_str.parse::<i64>() {
                        let label = Self::read_file_trimmed(&hwmon_dir.join(format!("temp{}_label", i)))
                            .unwrap_or_else(|| format!("temp{}", i));
                        let key = format!("{}_{}_temp{}", chip_name, label.to_lowercase().replace(' ', "_"), i);
                        readings.push(SensorReading {
                            key,
                            value: (millideg / 1000).to_string(),
                            unit: "°C".to_string(),
                        });
                    }
                }
            }

            // Read fan speeds (fan*_input files, RPM)
            for i in 1..=8 {
                let input = hwmon_dir.join(format!("fan{}_input", i));
                if let Some(val_str) = Self::read_file_trimmed(&input) {
                    if let Ok(rpm) = val_str.parse::<u64>() {
                        let label = Self::read_file_trimmed(&hwmon_dir.join(format!("fan{}_label", i)))
                            .unwrap_or_else(|| format!("fan{}", i));
                        let key = format!("{}_{}_fan{}", chip_name, label.to_lowercase().replace(' ', "_"), i);
                        readings.push(SensorReading {
                            key,
                            value: rpm.to_string(),
                            unit: "RPM".to_string(),
                        });
                    }
                }
            }
        }

        Ok(readings)
    }

    fn available_sensors(&self) -> Vec<SensorDescriptor> {
        // Discover by polling once
        match self.poll() {
            Ok(readings) => readings.iter().map(|r| SensorDescriptor {
                key: r.key.clone(),
                name: r.key.clone(),
                unit: r.unit.clone(),
            }).collect(),
            Err(_) => Vec::new(),
        }
    }
}
```

**Step 5: Run tests**

Run: `cargo test --test sensor_tests`
Expected: PASS

**Step 6: Commit**

```bash
git add src/sensor/ tests/sensor_tests.rs
git commit -m "feat: SensorProvider trait, SensorHub, and HwmonProvider"
```

---

### Task 12: AmdGpu + Sysinfo providers [DO-CONFIRM]

**Files:**
- Create: `src/sensor/amdgpu.rs`
- Create: `src/sensor/sysinfo_provider.rs`
- Modify: `tests/sensor_tests.rs`

**Implement:** Two sensor providers following the HwmonProvider pattern from Task 11.

**AmdGpuProvider** reads from `/sys/class/drm/card*/device/`:
- `gpu_busy_percent` → GPU utilization
- `hwmon/hwmon*/power1_average` → GPU power draw (microwatts → watts)
- `hwmon/hwmon*/temp1_input` → GPU temperature (millidegrees → degrees)
- `mem_info_vram_used` → VRAM used (bytes → GB)
- `mem_info_vram_total` → VRAM total (bytes → GB)

Keys: `gpu_util`, `gpu_power`, `gpu_temp`, `vram_used`, `vram_total`

**SysinfoProvider** uses the `sysinfo` crate:
- `System::total_memory()` and `System::used_memory()` → RAM in GB
- `System::cpus()` → CPU utilization per-core and average

Keys: `ram_used`, `ram_total`, `cpu_util`

Both providers need `with_base_path()` constructors for testing with fake sysfs trees (AmdGpu) or should use the real `sysinfo` crate (Sysinfo — test that it returns non-empty readings on the current machine).

**Confirm checklist:**
- [ ] Failing tests written FIRST for both providers
- [ ] AmdGpuProvider reads VRAM in bytes and converts to GB with 1 decimal (e.g. "4.2")
- [ ] AmdGpuProvider reads power in microwatts and converts to watts with 0 decimals
- [ ] SysinfoProvider returns `ram_used` and `ram_total` as GB with 1 decimal
- [ ] Both providers implement `SensorProvider` trait (name, poll, available_sensors)
- [ ] AmdGpuProvider tests use `tempfile::TempDir` with fake sysfs, not real sysfs
- [ ] No panics on missing sysfs files — providers return empty readings, not errors
- [ ] Committed with clear message

---

### Task 13: MangoHud CSV provider [DO-CONFIRM]

**Files:**
- Create: `src/sensor/mangohud.rs`
- Modify: `tests/sensor_tests.rs`

**Implement:** A sensor provider that reads MangoHud's CSV log output.

MangoHud writes CSV files to `~/.local/share/MangoHud/` (or `$MANGOHUD_LOG_DIR`). Each file is a CSV with headers like `fps,frametime,cpu_load,gpu_load,...`. The provider should:

1. Find the most recently modified `.csv` file in the MangoHud log directory
2. Read the last line (most recent data point)
3. Parse the CSV header + last line into sensor readings

Keys: `fps`, `frametime`, `gpu_load`, `cpu_load`

Use `with_log_dir()` constructor for testing. Tests should create a fake CSV file in a tempdir.

**Confirm checklist:**
- [ ] Failing test written FIRST
- [ ] Provider finds the most recently modified CSV by checking file metadata, not by name pattern
- [ ] Reads only the header line and last data line — does NOT load the entire file into memory (these can be hundreds of MB)
- [ ] Handles case where no CSV files exist (returns empty readings, not error)
- [ ] Handles case where CSV file has headers but no data rows
- [ ] `fps` value is rounded to integer, `frametime` to 1 decimal
- [ ] Committed with clear message

---

### Task 14: Review Tasks 11-13

**Trigger:** Both reviewers start simultaneously when Tasks 11-13 complete.

**Killer items (blocking):**
- [ ] HwmonProvider converts millidegrees to degrees (divides by 1000) in `poll()` — verify with test: `temp1_input = "65500"` → `value = "65"` (integer division)
- [ ] AmdGpuProvider converts microwatts to watts (divides by 1,000,000) — verify: `power1_average = "120000000"` → `gpu_power = "120"`
- [ ] AmdGpuProvider converts VRAM bytes to GB (divides by 1,073,741,824) — verify: `mem_info_vram_used = "4294967296"` → `vram_used = "4.0"`
- [ ] MangoHudProvider reads only header + last line, NOT the entire file — check implementation for `BufReader::lines().last()` or equivalent efficient approach
- [ ] SensorHub does not panic if a provider fails — verify it logs a warning and continues with other providers
- [ ] All providers return `Ok(vec![])` when their data source doesn't exist, not `Err`
- [ ] All provider trait objects are `Send` — required for SensorHub to be used from async context

**Quality items (non-blocking):**
- [ ] Sensor keys follow consistent naming convention (`snake_case`, no spaces)
- [ ] AmdGpuProvider scans all `/sys/class/drm/card*` directories, not just `card0`
- [ ] MangoHudProvider respects `$MANGOHUD_LOG_DIR` environment variable if set

**Resolution:** Killer item findings block merge; quality item findings queue for dev.

---

### Task 15: Milestone — sensor system works

**Present to user:**
- SensorProvider trait and SensorHub aggregator
- HwmonProvider reads temps and fan speeds from sysfs
- AmdGpuProvider reads VRAM, GPU power, GPU temp from DRM sysfs
- SysinfoProvider reads RAM usage via sysinfo crate
- MangoHudProvider tails CSV logs for FPS/frametime
- All providers tested with fake sysfs/filesystem fixtures
- Review findings and resolutions

**Wait for user response before proceeding to Task 16.**

---

## Phase 4: Service Integration

### Task 16: Tick loop wiring everything together [READ-DO]

**Files:**
- Create: `src/service/tick.rs`
- Modify: `src/service/mod.rs`

**Coordination required:**
Before starting, confirm with devs who implemented Tasks 2-3, 6-8, and 11-13 that:
- `Transport::send_frame()` accepts JPEG bytes (not raw pixmap)
- `FrameSource::render()` returns a `Pixmap` (which must be JPEG-encoded before sending)
- `SensorHub::poll()` returns `HashMap<String, String>` matching tera template variable format

**Step 1: Write a test for the tick loop logic (without real USB)**

Add to `tests/integration_tests.rs`:
```rust
use thermalwriter::render::{SensorData, FrameSource};
use thermalwriter::transport::{DeviceInfo, Transport};
use anyhow::Result;
use tiny_skia::Pixmap;
use std::sync::atomic::{AtomicU32, Ordering};

struct MockTransport {
    frames_sent: AtomicU32,
}
impl Transport for MockTransport {
    fn handshake(&mut self) -> Result<DeviceInfo> {
        Ok(DeviceInfo { vid: 0, pid: 0, width: 480, height: 480, pm: 4, sub: 0, use_jpeg: true })
    }
    fn send_frame(&mut self, _data: &[u8]) -> Result<()> {
        self.frames_sent.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn close(&mut self) {}
}

struct MockFrameSource;
impl FrameSource for MockFrameSource {
    fn render(&mut self, _sensors: &SensorData) -> Result<Pixmap> {
        Ok(Pixmap::new(480, 480).unwrap())
    }
    fn name(&self) -> &str { "mock" }
}

#[test]
fn jpeg_encode_produces_valid_output() {
    use thermalwriter::service::tick::encode_jpeg;
    let pixmap = Pixmap::new(480, 480).unwrap();
    let jpeg = encode_jpeg(&pixmap, 85).unwrap();
    // JPEG files start with FF D8
    assert_eq!(&jpeg[0..2], &[0xFF, 0xD8]);
    assert!(jpeg.len() > 100, "JPEG should be more than 100 bytes");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test integration_tests`
Expected: FAIL — `service::tick::encode_jpeg` doesn't exist yet.

**Step 3: Implement the tick loop in `src/service/tick.rs`**

```rust
use std::time::{Duration, Instant};
use anyhow::Result;
use image::{ImageBuffer, Rgba};
use log::{debug, info, warn};
use tiny_skia::Pixmap;

use crate::render::FrameSource;
use crate::sensor::SensorHub;
use crate::transport::Transport;

/// Encode a tiny-skia Pixmap to JPEG bytes.
pub fn encode_jpeg(pixmap: &Pixmap, quality: u8) -> Result<Vec<u8>> {
    let width = pixmap.width();
    let height = pixmap.height();
    let data = pixmap.data(); // premultiplied RGBA

    // Convert premultiplied RGBA to straight RGBA for image crate
    let mut rgba = Vec::with_capacity(data.len());
    for chunk in data.chunks(4) {
        let a = chunk[3] as u16;
        if a == 0 {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            let r = ((chunk[0] as u16 * 255) / a).min(255) as u8;
            let g = ((chunk[1] as u16 * 255) / a).min(255) as u8;
            let b = ((chunk[2] as u16 * 255) / a).min(255) as u8;
            rgba.extend_from_slice(&[r, g, b, chunk[3]]);
        }
    }

    let img: ImageBuffer<Rgba<u8>, _> = ImageBuffer::from_raw(width, height, rgba)
        .ok_or_else(|| anyhow::anyhow!("Failed to create image buffer"))?;

    let mut buf = std::io::Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    image::DynamicImage::ImageRgba8(img).write_with_encoder(encoder)?;

    Ok(buf.into_inner())
}

/// Run the tick loop. Blocks until `shutdown` is signaled.
pub async fn run_tick_loop(
    transport: &mut dyn Transport,
    frame_source: &mut dyn FrameSource,
    sensor_hub: &mut SensorHub,
    tick_rate_fps: u32,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let tick_duration = Duration::from_secs_f64(1.0 / tick_rate_fps as f64);
    info!("Tick loop started: {} FPS ({:?} per tick)", tick_rate_fps, tick_duration);

    loop {
        let tick_start = Instant::now();

        // Check shutdown
        if *shutdown.borrow() {
            info!("Tick loop shutdown requested");
            break;
        }

        // Poll sensors
        let sensors = sensor_hub.poll();

        // Render frame
        match frame_source.render(&sensors) {
            Ok(pixmap) => {
                // Encode to JPEG
                match encode_jpeg(&pixmap, 85) {
                    Ok(jpeg) => {
                        debug!("Frame rendered: {} bytes JPEG", jpeg.len());
                        if let Err(e) = transport.send_frame(&jpeg) {
                            warn!("Failed to send frame: {}", e);
                        }
                    }
                    Err(e) => warn!("JPEG encode failed: {}", e),
                }
            }
            Err(e) => warn!("Render failed: {}", e),
        }

        // Sleep until next tick
        let elapsed = tick_start.elapsed();
        if elapsed < tick_duration {
            tokio::time::sleep(tick_duration - elapsed).await;
        }

        // Check shutdown again after sleep
        if shutdown.has_changed().unwrap_or(false) && *shutdown.borrow() {
            break;
        }
    }

    Ok(())
}
```

**Step 4: Update `src/service/mod.rs`**

```rust
pub mod dbus;
pub mod tick;
```

**Step 5: Run tests**

Run: `cargo test --test integration_tests`
Expected: PASS

**Step 6: Commit**

```bash
git add src/service/ tests/integration_tests.rs
git commit -m "feat: tick loop with JPEG encoding and shutdown signal"
```

---

### Task 17: D-Bus interface with zbus [READ-DO]

**Files:**
- Create: `src/service/dbus.rs`
- Modify: `src/main.rs`

**Step 1: Implement the D-Bus interface in `src/service/dbus.rs`**

```rust
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, watch};
use zbus::{interface, SignalEmitter};
use log::info;

use crate::render::{FrameSource, TemplateRenderer};

/// Shared state between the D-Bus interface and the tick loop.
pub struct ServiceState {
    pub active_layout: String,
    pub connected: bool,
    pub resolution: (u32, u32),
    pub tick_rate: u32,
    pub shutdown_tx: watch::Sender<bool>,
    pub layout_dir: std::path::PathBuf,
    // Sender to notify the tick loop to reload the frame source
    pub layout_change_tx: tokio::sync::mpsc::Sender<String>,
}

pub struct DisplayInterface {
    state: Arc<Mutex<ServiceState>>,
}

impl DisplayInterface {
    pub fn new(state: Arc<Mutex<ServiceState>>) -> Self {
        Self { state }
    }
}

#[interface(name = "com.thermalwriter.Display")]
impl DisplayInterface {
    async fn set_layout(&self, name: String,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<String> {
        let state = self.state.lock().await;
        let layout_path = state.layout_dir.join(&name);
        if !layout_path.exists() {
            return Err(zbus::fdo::Error::InvalidArgs(
                format!("Layout not found: {}", name)
            ));
        }
        state.layout_change_tx.send(name.clone()).await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        drop(state);

        // Update state
        let mut state = self.state.lock().await;
        state.active_layout = name.clone();

        Self::layout_changed(&emitter, &name).await?;
        Ok(format!("Layout set to: {}", name))
    }

    async fn get_status(&self) -> HashMap<String, String> {
        let state = self.state.lock().await;
        let mut status = HashMap::new();
        status.insert("active_layout".to_string(), state.active_layout.clone());
        status.insert("connected".to_string(), state.connected.to_string());
        status.insert("resolution".to_string(), format!("{}x{}", state.resolution.0, state.resolution.1));
        status.insert("tick_rate".to_string(), state.tick_rate.to_string());
        status
    }

    async fn list_layouts(&self) -> Vec<String> {
        let state = self.state.lock().await;
        let mut layouts = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&state.layout_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |ext| ext == "html") {
                    if let Some(name) = path.file_name() {
                        layouts.push(name.to_string_lossy().to_string());
                    }
                }
            }
        }
        layouts.sort();
        layouts
    }

    async fn list_sensors(&self) -> Vec<String> {
        // Sensor listing will be wired up when SensorHub is integrated
        Vec::new()
    }

    async fn stop(&self) {
        let state = self.state.lock().await;
        let _ = state.shutdown_tx.send(true);
        info!("Shutdown requested via D-Bus");
    }

    async fn reload(&self) {
        info!("Reload requested via D-Bus");
        // Trigger re-read of config and reconnect
    }

    // Properties
    #[zbus(property)]
    async fn active_layout(&self) -> String {
        self.state.lock().await.active_layout.clone()
    }

    #[zbus(property)]
    async fn connected(&self) -> bool {
        self.state.lock().await.connected
    }

    #[zbus(property)]
    async fn resolution(&self) -> (u32, u32) {
        self.state.lock().await.resolution
    }

    #[zbus(property)]
    async fn tick_rate(&self) -> u32 {
        self.state.lock().await.tick_rate
    }

    #[zbus(property)]
    async fn set_tick_rate(&mut self, rate: u32) -> zbus::fdo::Result<()> {
        if rate == 0 || rate > 30 {
            return Err(zbus::fdo::Error::InvalidArgs("Tick rate must be 1-30".to_string()));
        }
        self.state.lock().await.tick_rate = rate;
        Ok(())
    }

    // Signals
    #[zbus(signal)]
    async fn layout_changed(emitter: &SignalEmitter<'_>, name: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn device_connected(emitter: &SignalEmitter<'_>, info: HashMap<String, String>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn device_disconnected(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn error(emitter: &SignalEmitter<'_>, message: &str) -> zbus::Result<()>;
}

/// Start the D-Bus service on the session bus.
pub async fn serve(state: Arc<Mutex<ServiceState>>) -> anyhow::Result<zbus::Connection> {
    let iface = DisplayInterface::new(state);
    let connection = zbus::connection::Builder::session()?
        .name("com.thermalwriter.Service")?
        .serve_at("/com/thermalwriter/display", iface)?
        .build()
        .await?;

    info!("D-Bus service registered: com.thermalwriter.Service");
    Ok(connection)
}
```

**Step 2: Wire up `main.rs` as the daemon entry point**

```rust
use std::path::PathBuf;
use std::sync::Arc;
use anyhow::Result;
use log::info;
use tokio::sync::{Mutex, watch, mpsc};

mod cli;

use thermalwriter::sensor::SensorHub;
use thermalwriter::sensor::hwmon::HwmonProvider;
use thermalwriter::sensor::sysinfo_provider::SysinfoProvider;
use thermalwriter::sensor::amdgpu::AmdGpuProvider;
use thermalwriter::sensor::mangohud::MangoHudProvider;
use thermalwriter::render::TemplateRenderer;
use thermalwriter::service::dbus::{self, ServiceState};
use thermalwriter::service::tick;
use thermalwriter::transport::Transport;
use thermalwriter::transport::bulk_usb::BulkUsb;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    // TODO: CLI dispatch (daemon vs ctl subcommands) — Task 20
    // For now, just run the daemon directly

    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("thermalwriter");
    let layout_dir = config_dir.join("layouts");
    std::fs::create_dir_all(&layout_dir)?;

    // Load default layout
    let default_layout = layout_dir.join("system-stats.html");
    let template = if default_layout.exists() {
        std::fs::read_to_string(&default_layout)?
    } else {
        include_str!("../layouts/system-stats.html").to_string()
    };

    // Setup transport
    let mut transport = BulkUsb::new()?;
    let device_info = transport.handshake()?;
    info!("Device: {}x{}, PM={}", device_info.width, device_info.height, device_info.pm);

    // Setup sensors
    let mut sensor_hub = SensorHub::new();
    sensor_hub.add_provider(Box::new(HwmonProvider::new()));
    sensor_hub.add_provider(Box::new(SysinfoProvider::new()));
    sensor_hub.add_provider(Box::new(AmdGpuProvider::new()));
    sensor_hub.add_provider(Box::new(MangoHudProvider::new()));

    // Setup renderer
    let mut frame_source = TemplateRenderer::new(&template, device_info.width, device_info.height)?;

    // Shared state
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (layout_tx, mut layout_rx) = mpsc::channel::<String>(4);

    let state = Arc::new(Mutex::new(ServiceState {
        active_layout: "system-stats.html".to_string(),
        connected: true,
        resolution: (device_info.width, device_info.height),
        tick_rate: 2,
        shutdown_tx,
        layout_dir: layout_dir.clone(),
        layout_change_tx: layout_tx,
    }));

    // Start D-Bus service
    let _connection = dbus::serve(state.clone()).await?;

    // Layout change listener
    let layout_dir_clone = layout_dir.clone();
    tokio::spawn(async move {
        while let Some(name) = layout_rx.recv().await {
            let path = layout_dir_clone.join(&name);
            if let Ok(html) = std::fs::read_to_string(&path) {
                // We'd need a way to update frame_source — handled via shared Arc<Mutex<>>
                info!("Layout changed to: {}", name);
            }
        }
    });

    // Run tick loop (blocks until shutdown)
    tick::run_tick_loop(
        &mut transport,
        &mut frame_source,
        &mut sensor_hub,
        2, // Default 2 FPS
        shutdown_rx,
    ).await?;

    transport.close();
    info!("thermalwriter shutdown complete");
    Ok(())
}
```

**Step 3: Add `dirs` dependency to `Cargo.toml`**

Add to `[dependencies]`: `dirs = "6"`

**Step 4: Verify it compiles**

Run: `cargo build`
Expected: Compiles successfully

**Step 5: Commit**

```bash
git add src/service/dbus.rs src/main.rs Cargo.toml
git commit -m "feat: D-Bus interface and daemon main entry point"
```

---

### Task 18: Review Tasks 16-17

**Trigger:** Both reviewers start simultaneously when Tasks 16-17 complete.

**Killer items (blocking):**
- [ ] `encode_jpeg()` handles premultiplied→straight alpha conversion correctly — tiny-skia uses premultiplied RGBA, `image` crate expects straight RGBA. Verify: a pixel with (128, 0, 0, 128) premultiplied should become (255, 0, 0, 128) straight
- [ ] JPEG output starts with `0xFF 0xD8` magic bytes — verified by the test in integration_tests.rs
- [ ] Tick loop respects shutdown signal — `watch::Receiver` is checked both before render and after sleep
- [ ] D-Bus `set_tick_rate` validates range (1-30) — prevents 0 FPS (infinite loop) or unreasonable rates
- [ ] D-Bus `set_layout` checks that the layout file exists before accepting — prevents silent failures
- [ ] `ServiceState` uses `Arc<Mutex<>>` for shared access between D-Bus and tick loop — not `Rc` (would panic in async context)
- [ ] D-Bus service name is `com.thermalwriter.Service` and object path is `/com/thermalwriter/display` — matching the design doc

**Quality items (non-blocking):**
- [ ] Tick loop logs frame render time for performance monitoring
- [ ] D-Bus interface has doc comments on methods
- [ ] main.rs creates config directory if it doesn't exist

**Resolution:** Killer item findings block merge; quality item findings queue for dev.

---

### Task 19: Milestone — working daemon

**Present to user:**
- Tick loop wires sensors → template rendering → JPEG encoding → USB transport
- D-Bus interface exposes SetLayout, GetStatus, ListLayouts, Stop, and properties
- Daemon starts, connects to device, and pushes live-rendered frames
- Test by running the daemon and verifying the cooler LCD shows sensor data
- Verify D-Bus works: `busctl call com.thermalwriter.Service /com/thermalwriter/display com.thermalwriter.Display GetStatus`
- Review findings and resolutions

**Wait for user response before proceeding to Task 20.**

---

## Phase 5: CLI + Config + Packaging

### Task 20: CLI subcommands [DO-CONFIRM]

**Files:**
- Create: `src/cli.rs`
- Modify: `src/main.rs` — dispatch between `daemon` and `ctl` subcommands

**Implement:** CLI using clap with two top-level subcommands:
- `thermalwriter daemon` — start the background service (current main.rs logic)
- `thermalwriter ctl <subcommand>` — D-Bus client commands

The `ctl` subcommand uses zbus proxy (the `#[proxy(...)]` derive macro) to call D-Bus methods:
- `ctl status` → calls `GetStatus()`, prints key-value pairs
- `ctl layout <name>` → calls `SetLayout(name)`
- `ctl layouts` → calls `ListLayouts()`, prints one per line
- `ctl sensors` → calls `ListSensors()`
- `ctl stop` → calls `Stop()`
- `ctl reload` → calls `Reload()`

Follow the zbus proxy pattern from Context7 research:
```rust
#[zbus::proxy(
    interface = "com.thermalwriter.Display",
    default_service = "com.thermalwriter.Service",
    default_path = "/com/thermalwriter/display"
)]
trait Display {
    async fn get_status(&self) -> zbus::Result<HashMap<String, String>>;
    async fn set_layout(&self, name: &str) -> zbus::Result<String>;
    // ... etc
}
```

**Confirm checklist:**
- [ ] Failing test written FIRST (at minimum: test that CLI parses known subcommands without error)
- [ ] `thermalwriter` with no args prints help and exits
- [ ] `thermalwriter daemon` runs the service (moves existing main logic into a function)
- [ ] `thermalwriter ctl status` connects to D-Bus and prints output, or prints clear error if service isn't running
- [ ] Proxy trait matches the D-Bus interface in `dbus.rs` exactly (same method signatures)
- [ ] No `unwrap()` on D-Bus proxy calls — user-facing errors should be readable
- [ ] Committed with clear message

---

### Task 21: Config file + built-in layouts [DO-CONFIRM]

**Files:**
- Create: `src/config.rs`
- Create: `layouts/system-stats.html`
- Create: `layouts/gpu-focus.html`
- Create: `layouts/minimal.html`
- Modify: `src/main.rs` — load config on startup

**Implement:** TOML config parsing and 3 built-in HTML layouts.

Config file at `~/.config/thermalwriter/config.toml`:
```toml
[display]
tick_rate = 2
default_layout = "system-stats.html"
jpeg_quality = 85

[sensors]
poll_interval_ms = 1000
mangohud_log_dir = "" # empty = auto-detect
```

Parse with `serde` + `toml` crate. Use defaults when config file doesn't exist.

Three built-in layouts embedded via `include_str!`:
- `system-stats.html`: CPU temp, GPU temp, CPU power, GPU power, RAM, VRAM, FPS, frametime
- `gpu-focus.html`: GPU-centric with larger GPU metrics, smaller CPU
- `minimal.html`: Just a clock and CPU temp

On first run, copy built-in layouts to `~/.config/thermalwriter/layouts/` so users can edit them.

**Confirm checklist:**
- [ ] Failing test written FIRST (config parsing with known TOML input)
- [ ] Config struct derives `Deserialize` and `Default`
- [ ] Missing config file uses defaults (no error, no crash)
- [ ] Invalid TOML prints a clear error with the file path
- [ ] Built-in layouts use template variables that match the sensor keys from Phase 3
- [ ] Layouts are valid HTML parseable by `parse_html()` — test each layout through the parser
- [ ] Layouts are copied to config dir only if they don't already exist (don't overwrite user edits)
- [ ] Committed with clear message

---

### Task 22: systemd user service unit [DO-CONFIRM]

**Files:**
- Create: `systemd/thermalwriter.service`

**Implement:** A systemd user service unit file.

```ini
[Unit]
Description=Thermalright Cooler LCD Display Service
Documentation=https://github.com/mgaruccio/thermalwriter
After=default.target

[Service]
Type=simple
ExecStart=/usr/bin/thermalwriter daemon
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=info

[Install]
WantedBy=default.target
```

**Confirm checklist:**
- [ ] Service type is `simple` (not `forking` — tokio runs in the foreground)
- [ ] `Restart=on-failure` handles USB disconnect crashes gracefully
- [ ] `RestartSec=5` prevents tight restart loops
- [ ] `WantedBy=default.target` (user service, not `multi-user.target`)
- [ ] `ExecStart` path matches where `cargo install` places the binary
- [ ] No `User=` or `Group=` directives (user service, not system service)
- [ ] Committed with clear message

---

### Task 23: Review Tasks 20-22

**Trigger:** Both reviewers start simultaneously when Tasks 20-22 complete.

**Killer items (blocking):**
- [ ] CLI proxy trait in `cli.rs` matches D-Bus interface in `dbus.rs` — same method names, same parameter types, same return types
- [ ] `thermalwriter ctl status` exits with non-zero code when D-Bus service is not running (e.g., exit code 1 with "Service not running" message)
- [ ] Config `default_layout` is used on startup — verify in `main.rs` that it reads the config before loading the layout
- [ ] Built-in layouts parse successfully through `parse_html()` without errors — run each through the parser in a test
- [ ] systemd service has `After=default.target` — without this, the service may start before the user's D-Bus session is ready
- [ ] Built-in layout template variables (`{{ cpu_temp }}`, `{{ gpu_power }}`, etc.) exactly match the sensor keys produced by the providers in Phase 3

**Quality items (non-blocking):**
- [ ] CLI outputs structured text, not raw debug format
- [ ] Config file has comments explaining each option
- [ ] systemd unit has a `Documentation=` URL

**Resolution:** Killer item findings block merge; quality item findings queue for dev.

---

### Task 24: Final milestone

**Present to user:**
- Complete working system: daemon + CLI + config + systemd service
- Demo: `systemctl --user start thermalwriter`, verify LCD shows live data
- Demo: `thermalwriter ctl status`, `thermalwriter ctl layouts`, `thermalwriter ctl layout gpu-focus.html`
- Full test suite passes
- List of all sensor keys available
- Binary size and memory footprint compared to trcc
- Any remaining open questions from the design doc (hot-plug, multi-device, boot animation)
- Summary of all review findings and resolutions

**Wait for user response. This is the final milestone — project is complete.**
