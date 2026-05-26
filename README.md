# thermalwriter

`thermalwriter` is a lightweight Linux daemon for Thermalright cooler LCD displays. It replaces the large vendor Python/Qt app with a Rust service that renders local layouts, polls system sensors, and sends JPEG frames over USB.

Current status: `v0.1.0`, hardware-verified on the Thermalright Peerless Vision / GrandVision 360 AIO, USB `87ad:70db`. Other Thermalright LCD coolers may work if they use the same protocol, but they are not confirmed yet.

## Features

- User-session systemd daemon with D-Bus control commands.
- SVG layout renderer with Tera variables and built-in 480x480 layouts.
- Sensor providers for hwmon, sysinfo, AMDGPU, NVIDIA, MangoHud, and Intel RAPL.
- Global background image support for PNG/JPEG assets.
- Xvfb mirror mode for rendering an X11 application onto the LCD.
- Optional Tauri/Svelte configuration GUI under `gui/`.
- No hardware required for layout previews and most tests.

## Requirements

- Linux with systemd user services and udev.
- Rust 1.85 or newer.
- A supported Thermalright LCD cooler on USB.
- D-Bus session bus, standard on desktop Linux.
- Optional: Node.js for GUI development.
- Optional: Xvfb for mirror mode.

## Install From Source

Clone the repository, then run:

```sh
./packaging/install.sh
```

The installer:

1. Builds and installs `thermalwriter` to `~/.cargo/bin`.
2. Installs the systemd user service to `~/.config/systemd/user/thermalwriter.service`.
3. Installs the RAPL udev rule through `thermalwriter setup-udev`, prompting for sudo only for that step.
4. Enables and restarts the user service.

Manual install:

```sh
cargo install --path . --locked
install -Dm0644 systemd/thermalwriter.service ~/.config/systemd/user/thermalwriter.service
systemctl --user daemon-reload
thermalwriter setup-udev
systemctl --user enable --now thermalwriter
```

Uninstall the service and installed binary:

```sh
./packaging/uninstall.sh
```

The uninstall script leaves `~/.config/thermalwriter/` intact.

## Usage

```sh
systemctl --user status thermalwriter
thermalwriter ctl status
thermalwriter ctl layouts
thermalwriter ctl layout svg/neon-dash-v2.svg
thermalwriter ctl sensors
thermalwriter ctl mirror "conky -c ~/.config/conky/lcd.conf"
```

Config lives at `~/.config/thermalwriter/config.toml`. Built-in layouts are seeded into `~/.config/thermalwriter/layouts/`, and backgrounds are seeded into `~/.config/thermalwriter/backgrounds/`. Existing user files are not overwritten.

## Preview Without Hardware

Render a layout preview PNG:

```sh
cargo run --example preview_layout layouts/svg/neon-dash-v2.svg
```

Render to hardware for a short mock-data run:

```sh
cargo run --example render_layout layouts/svg/neon-dash-v2.svg 15 --mock
```

Do not assume hardware is attached when developing. Prefer tests and preview examples before running hardware-facing commands.

## GUI

The optional GUI is in `gui/`:

```sh
cd gui
npm ci
npm run build
npm run tauri:dev
```

The GUI talks to the daemon over D-Bus and can also render local previews. GUI app bundles are not published yet; see `docs/release.md` for the release checklist.

## RAPL Udev Rule

CPU package power comes from `/sys/class/powercap/intel-rapl:*/energy_uj`. Modern Linux kernels keep that file root-only by default after CVE-2020-8694. The included udev rule changes matching `energy_uj` files to `0444` on powercap add/change events so the user-session daemon can read them.

If you skip the rule, the daemon still runs; CPU power appears unavailable and a warning points to `thermalwriter setup-udev`.

## Development

Useful checks:

```sh
cargo fmt --check
cargo test --workspace
cargo test --workspace --no-default-features
cargo clippy --workspace --all-targets -- -D warnings
cd gui && npm ci && npm run build
```

More details:

- `docs/architecture.md` describes the daemon, renderers, sensors, D-Bus service, and GUI split.
- `docs/release.md` covers GitHub, crates.io, and GUI release checks.
- `skills/designing-layouts/` documents LCD layout constraints.

## License

MIT. See `LICENSE`.
