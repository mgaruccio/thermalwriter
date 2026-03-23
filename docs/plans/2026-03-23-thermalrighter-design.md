---
date: 2026-03-23
topic: thermalwriter-service
---

# thermalwriter — Lightweight Cooler Display Service

## What We're Building

A lightweight Rust daemon that owns and drives the LCD display on Thermalright coolers. It runs as a systemd user service, continuously compositing and pushing frames to the display. External tools (CLI, future Tauri GUI) interact with it over D-Bus.

The core insight: the cooler LCD is a dumb framebuffer — it shows whatever frame you last sent. All rendering, sensor polling, and update scheduling lives in the service. The existing `trcc` Python project buries this behind 147 files, a Qt dependency, and a 400MB runtime. We're replacing that with a single binary.

## Architecture

```
┌──────────────────────────────────────────────────┐
│                  thermalwriter                   │
│                  (daemon)                         │
│                                                   │
│  ┌─────────────┐  ┌──────────────┐               │
│  │ SensorHub   │  │ FrameSource  │ (trait)        │
│  │             │  │  ├─ TemplateRenderer          │
│  │  ├─ Hwmon   │  │  ├─ StaticImage              │
│  │  ├─ Sysinfo │  │  └─ (future: mirror, gpu)    │
│  │  └─ MangoHud│  └──────┬───────┘               │
│  └──────┬──────┘         │                        │
│         │         ┌──────▼───────┐               │
│         └────────►│  Tick Loop   │               │
│                   │  (compose +  │               │
│                   │   encode +   │               │
│                   │   send)      │               │
│                   └──────┬───────┘               │
│                          │                        │
│                   ┌──────▼───────┐               │
│                   │  Transport   │ (trait)        │
│                   │  └─ BulkUSB  │               │
│                   └──────────────┘               │
│                                                   │
│  ┌──────────────┐                                │
│  │  D-Bus API   │◄──── CLI / Tauri GUI           │
│  └──────────────┘                                │
└──────────────────────────────────────────────────┘
```

## Why This Approach

**Considered and rejected:**

- **Keep Python, just refactor** — doesn't solve the weight problem. PySide6 alone is 200MB+ and you need it even headless because the renderer depends on QImage/QPainter.
- **Go instead of Rust** — viable for the service, but Tauri is the GUI target and Tauri is Rust-native. Two language boundaries (Go service <-> Tauri GUI) vs one codebase.
- **Zig** — would work for the service, but the crate ecosystem for D-Bus/USB/rendering is much more mature in Rust, and Tauri GUI would be a separate project with no shared code.
- **GPU rendering** — overkill for 480x480 at 2 FPS. GPU path is a future FrameSource implementation, not a rewrite.
- **Raw unix socket** — simpler, but D-Bus gives us signals/discoverability/desktop integration that matter for a long-running Linux desktop daemon.
- **TOML for layouts** — awkward for spatial layout definition. HTML/CSS is universally understood and the same language the Tauri GUI will use for its layout editor.

## Key Decisions

- **Rust**: single binary, Tauri-compatible, mature crate ecosystem for USB/D-Bus/rendering
- **D-Bus IPC**: linux-native, signals for state broadcast, discoverability via busctl/gdbus, desktop integration for panel widgets
- **HTML/CSS layout definition**: taffy (CSS flexbox/grid engine) + tera templates + tiny-skia rendering. Users define layouts in a familiar language; future Tauri GUI edits the same files
- **JPEG frame encoding**: device requires it (PM=4 bulk protocol). JPEG at 480x480 is fast and small (~30-50KB per frame)
- **Trait-based frame sources**: TemplateRenderer is primary, but screen mirroring/GPU effects/static images are future FrameSource implementations with no architectural changes
- **Trait-based sensor providers**: extensible without modifying core. Ship with hwmon, sysinfo, MangoHud CSV, AMDGPU
- **systemd user service**: auto-starts on login, config in ~/.config/thermalwriter/

## Transport Layer

Target device: Peerless Vision (VID 87AD, PID 70DB), 480x480, USB bulk protocol.

Protocol (reverse-engineered from USBLCDNew.exe):

1. **Handshake**: write 64-byte magic (12 34 56 78...), read 1024 bytes. resp[24] = PM (product model), resp[36] = SUB. Determines resolution.
2. **Frame send**: 64-byte header + JPEG payload in 16KB chunks + ZLP delimiter
3. **Header format**: magic(4) + cmd(4) + width(4) + height(4) + padding(40) + mode(4) + payload_length(4)
4. **cmd=2 for JPEG** (all PMs except 32), cmd=3 for raw RGB565 (PM=32 only)

Transport trait for future protocol support (SCSI, HID):

```rust
trait Transport {
    fn handshake(&mut self) -> Result<DeviceInfo>;
    fn send_frame(&mut self, data: &[u8]) -> Result<()>;
    fn close(&mut self);
}
```

Rust crate: `rusb` (libusb bindings).

## Rendering Pipeline

Per-tick pipeline:

1. `tera`: substitute {{ sensor_name }} with live values in layout HTML
2. Lightweight HTML parser: extract elements + inline styles
3. `taffy`: compute layout positions/sizes from CSS flexbox/grid properties
4. `tiny-skia`: draw text, rects, backgrounds into 480x480 pixmap
5. `turbojpeg` or `image`: encode to JPEG
6. Transport: send to device

Layout files are a subset of HTML/CSS:

```html
<div style="display: flex; flex-direction: column; gap: 8px; padding: 12px;
            background: #1a1a2e; color: #fff; font-family: monospace;">
  <div style="font-size: 28px; text-align: center;">
    CPU {{ cpu_temp }}C  ·  GPU {{ gpu_temp }}C
  </div>
  <div style="display: flex; justify-content: space-between; font-size: 20px;">
    <span>CPU {{ cpu_power }}W</span>
    <span>GPU {{ gpu_power }}W</span>
  </div>
  <div style="display: flex; justify-content: space-between; font-size: 20px;">
    <span>RAM {{ ram_used }}GB</span>
    <span>VRAM {{ vram_used }}GB</span>
  </div>
  <div style="font-size: 18px; color: #88ff88; text-align: center;">
    {{ fps }} FPS  ·  {{ frametime }}ms
  </div>
</div>
```

Supported CSS subset: display (flex, grid), flex-direction, justify-content, align-items, gap, padding, margin, font-size, font-family, color, background, text-align, border-radius.

Frame source trait for modularity:

```rust
trait FrameSource {
    fn render(&mut self, sensors: &SensorData) -> Result<Pixmap>;
    fn name(&self) -> &str;
}
```

## Sensor System

Extensible provider model:

```rust
trait SensorProvider {
    fn name(&self) -> &str;
    fn poll(&mut self) -> Result<Vec<SensorReading>>;
    fn available_sensors(&self) -> Vec<SensorDescriptor>;
}
```

Initial providers:

| Provider | Source | Metrics |
|----------|--------|---------|
| HwmonProvider | /sys/class/hwmon/*/ | CPU/GPU temps, fan speeds (RPM) |
| SysinfoProvider | sysinfo crate | RAM used/total, CPU/GPU power draw |
| MangoHudProvider | MangoHud CSV log files | FPS, frametime, GPU load |
| AmdGpuProvider | /sys/class/drm/card*/device/ | VRAM used, GPU power, GPU temp |

SensorHub aggregates all providers, polls on configurable interval (default 1s), exposes flat SensorData map. Sensor keys are template variable names (cpu_temp, gpu_power, fps, etc.).

MangoHud provider watches CSV output directory, tails latest file, parses most recent row.

## D-Bus Interface

Service name: `com.thermalwriter.Service`
Object path: `/com/thermalwriter/display`

```
Interface: com.thermalwriter.Display

Methods:
  SetLayout(path: String)          - Load a layout file and switch to it
  SetStaticImage(path: String)     - Push a static image to the display
  SetBrightness(level: u8)         - Set display brightness
  GetStatus() -> Dict              - Current state (layout, fps, device info)
  ListLayouts() -> Vec<String>     - List available layout files
  ListSensors() -> Vec<String>     - List available sensor keys + current values
  Reload()                         - Re-read config, reconnect device if needed
  Stop()                           - Gracefully shut down the service

Signals:
  DeviceConnected(info: Dict)      - Emitted when device is detected
  DeviceDisconnected()             - Emitted when device is lost
  LayoutChanged(name: String)      - Emitted when active layout changes
  Error(message: String)           - Emitted on recoverable errors

Properties:
  ActiveLayout: String (read)
  Connected: bool (read)
  Resolution: (u32, u32) (read)
  TickRate: u32 (read/write)       - Frames per second target
```

Implemented via `zbus` derive macros.

## CLI

Subcommands of the main binary:

```
thermalwriter ctl status          # show current state
thermalwriter ctl layout <name>   # switch layout
thermalwriter ctl image <path>    # push static image
thermalwriter ctl brightness <n>  # set brightness
thermalwriter ctl layouts         # list available layouts
thermalwriter ctl sensors         # list sensor readings
thermalwriter ctl reload          # reload config
```

## Service Lifecycle

- systemd user service: systemctl --user start/enable thermalwriter
- Config directory: ~/.config/thermalwriter/
  - config.toml: tick rate, default layout, sensor poll interval, device settings
  - layouts/: HTML/CSS layout files
- Ships with 2-3 built-in layouts (system stats, GPU focus, minimal)
- Watches layout files for changes (hot-reload)

## Tick Loop

```
loop {
    let sensors = sensor_hub.poll();
    let pixmap = frame_source.render(&sensors);
    let jpeg = encode_jpeg(&pixmap);
    transport.send_frame(&jpeg)?;
    sleep_until(next_tick);
}
```

Default tick rate: 2 FPS. Configurable up to ~10 FPS. Device accepts frames as fast as sent; bottleneck is USB transfer (~30-50KB JPEG per frame).

## Crate Dependencies

| Crate | Purpose |
|-------|---------|
| rusb | USB bulk transfers (libusb bindings) |
| zbus | D-Bus interface (async, derive macros) |
| tiny-skia | 2D software rendering |
| taffy | CSS flexbox/grid layout engine |
| tera | Template engine ({{ var }} substitution) |
| fontdue | Font rasterization |
| turbojpeg or image | JPEG encoding |
| sysinfo | System metrics (RAM, CPU) |
| tokio | Async runtime (D-Bus + tick loop) |
| clap | CLI argument parsing |

## Open Questions

- **Device hot-plug**: should the service handle USB disconnect/reconnect gracefully (retry loop), or exit and let systemd restart it?
- **Multi-device**: trcc supports multiple devices. Single-device fine for now?
- **Boot animation**: bulk protocol supports writing compressed frames to device flash. Include in v1 or defer?
