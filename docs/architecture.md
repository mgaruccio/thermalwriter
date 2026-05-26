# Architecture

thermalwriter is a Rust daemon plus an optional Tauri configuration GUI for Thermalright cooler LCD displays.

## Daemon

The `thermalwriter` binary has three main entry points:

- `thermalwriter daemon` starts the display service.
- `thermalwriter ctl ...` talks to the daemon over D-Bus.
- `thermalwriter setup-udev` installs the RAPL udev rule used for CPU power readings.

Core modules live under `src/`:

- `transport/` owns the USB bulk protocol and reconnect behavior.
- `render/` turns layouts into 480x480 RGB frames.
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

