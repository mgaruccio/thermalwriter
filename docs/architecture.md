# Architecture

thermalwriter is a Rust daemon plus an optional Tauri configuration GUI for Thermalright cooler LCD displays.

## Daemon

The `thermalwriter` binary has three main entry points:

- `thermalwriter daemon` starts the display service.
- `thermalwriter ctl ...` talks to the daemon over D-Bus.
- `thermalwriter setup-udev` installs the RAPL udev rule used for CPU power readings.

Core modules live under `src/`:

- `transport/` discovers supported USB/SCSI devices, resolves negotiated profiles, encodes frames, and implements raw bulk, SCSI, HID Type 2/3, and LY wire protocols.
- `render/` turns responsive or fixed-canvas layouts into RGB frames at the negotiated native resolution.
- `sensor/` polls system metrics from hwmon, sysinfo, AMDGPU, NVIDIA, MangoHud, and Intel RAPL.
- `service/` runs the tick loop, D-Bus service, and Xvfb capture support.
- `config.rs` handles config loading, validation, seeding built-in assets, and atomic updates.

The primary rendering path is SVG templates rendered with Tera variables and `resvg`. Legacy HTML layouts still work through the custom template renderer. The optional `blitz` feature is experimental and exists for evaluating a fuller HTML/CSS renderer.

## Runtime Data

The daemon stores user config in `~/.config/thermalwriter/config.toml`. Built-in layouts are seeded into `~/.config/thermalwriter/layouts/` on first run, and backgrounds are seeded into `~/.config/thermalwriter/backgrounds/`. Existing user files are not overwritten.

The D-Bus interface is `com.thermalwriter.Display` at `/com/thermalwriter/display` on service `com.thermalwriter.Service`.

## GUI

The GUI lives in `gui/` and uses Svelte 5 with Tauri 2. It depends on the daemon crate with default features disabled so it can reuse config, rendering, and preview code without linking daemon-only USB paths.

GUI release packaging is separate from crates.io publication of the daemon crate.

## Multi-cooler transport

Discovery (`src/transport/discovery.rs`) scans libusb and `scsi_generic`, applies
the configured `auto` or `VID:PID` selector, and rejects zero/ambiguous matches.
A `TransportConnector` handshakes one selected device and returns
`Result<(Box<dyn Transport>, DeviceInfo)>`. `DeviceInfo` stores the negotiated
VID, PID, PM, SUB, FBL, wire protocol, and typed `DeviceProfile`. The profile
stores native width and height, `FrameEncoding`, and the `rotate_panel`,
`widescreen`, `encode_baseline`, `encode_base`, and `encode_invert` controls
used during encoding. Panel shape, oriented dimensions, wire dimensions, and
rotation angles are derived from those stored values; there are no `shape` or
rotation-table fields. PM, SUB, and FBL are negotiated identity values on
`DeviceInfo`; profile resolution folds their applicable overrides into the
resulting `DeviceProfile`.

The tick loop treats a reconnect as a generation. It publishes `connected=true`
and the negotiated D-Bus resolution only after the main task has rebuilt a
dimension-correct frame source and returned the matching generation result.
Stale source builds cannot overwrite a newer connection. Supported shapes span
portrait, square, landscape, wide, and ultrawide panels.
