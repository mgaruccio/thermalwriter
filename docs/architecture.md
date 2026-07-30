# Architecture

thermalwriter is a Rust daemon plus an optional Tauri configuration GUI and a lightweight system-tray controller for Thermalright cooler LCD displays.

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

## System tray

`tray/` is a workspace member that builds `thermalwriter-tray`. It is intentionally **not** part of the headless daemon and **not** the Tauri process:

- Implements freedesktop StatusNotifierItem via `ksni` (pure D-Bus, no GTK/WebKit).
- Inlines a minimal D-Bus proxy for `com.thermalwriter.Display` (does **not** link the full `thermalwriter` library, to avoid pulling render/sensor deps into the tray binary). Keep method signatures aligned with `src/dbus_types.rs`.
- Ships multi-size ARGB `IconPixmap` data and leaves `IconName` empty. Hosts that prefer theme names without falling back to pixmaps (notably Quickshell/Noctalia) otherwise show a missing-icon placeholder.
- Menu items are text-only; no freedesktop `icon-name` entries (same missing-icon issue on some hosts). The active layout is marked with a `✓` prefix.
- Spawns the Config GUI as an external process (left-click and **Open Config…** share that path). Launch order: `hyprctl dispatch exec` → `systemd-run --user` → direct `setsid` spawn. After launch, the tray focuses/moves the window onto the current Hyprland workspace when possible.
- Idle path is event-driven (menu callbacks + D-Bus). No status polling timers. SNI registration retries while the tray host starts.

The tray unit is `systemd/thermalwriter-tray.service` (`WantedBy=graphical-session.target`). Packaging also drops an XDG autostart `.desktop` for desktops that do not start that target.

## Multi-cooler transport

Discovery (`src/transport/discovery.rs`) scans libusb and `scsi_generic`, applies
the configured `auto`, `all`, or `VID:PID` selector, and rejects zero/ambiguous
matches for single-device selectors. `display.device = "all"` opens every
supported display in deterministic order (VID, PID, then physical path) and
mirrors one rendered frame to each output with aspect-preserving letterboxing.
A `TransportConnector` handshakes the selected device(s) and returns
`ConnectedOutputs` via `connect_all()`, or a single transport via `connect()`
for `auto` and `VID:PID`. `DeviceInfo` stores the negotiated
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

Three dimension spaces matter across the daemon and D-Bus surface:

1. **Native / negotiated** — `DeviceInfo` width/height from the handshake
   profile. The D-Bus `resolution` property and status field report this native
   panel size, not the oriented render canvas.
2. **Oriented** — the rotation-aware authoring/render canvas
   (`oriented_dimensions(native_w, native_h, rotation)`) used before wire-angle
   encode.
3. **Wire** — post-encode payload dimensions expected by the transport
   (`wire_dimensions`).
