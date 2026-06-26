# thermalwriter

`thermalwriter` is a lightweight Linux daemon for Thermalright cooler LCD displays. It replaces the large vendor Python/Qt app with a Rust service that renders local layouts, polls system sensors, and sends JPEG frames over USB.

Current status: Public Beta (`v0.1.0`).

### Supported Devices

| Status | Device / Host Details |
| --- | --- |
| **Known working** | Thermalright Peerless Vision / GrandVision 360 AIO, USB `87ad:70db` |
| **Experimental** | Other Thermalright LCD coolers using the same protocol |
| **Not supported** | Non-Linux hosts and devices with different USB protocols |

## Features

![Layout Preview](docs/assets/neon-dash-v2-preview.png)

- User-session systemd daemon with D-Bus control commands.
- SVG layout renderer with Tera variables and built-in 480x480 layouts.
- Sensor providers for hwmon, sysinfo, AMDGPU, NVIDIA, MangoHud, and Intel RAPL.
- Global background image support for PNG/JPEG assets.
- Xvfb capture mode for streaming any X11 application onto the LCD, with built-in presets for conky, cava, and btop (session-only; never persisted as a boot default).
- Optional Tauri/Svelte configuration GUI under `gui/`, including a Stream tab with a live preview.
- No hardware required for layout previews and most tests.

## Requirements

- Linux with systemd user services and udev.
- Rust 1.85 or newer.
- A supported Thermalright LCD cooler on USB.
- D-Bus session bus, standard on desktop Linux.
- Optional: Node.js for GUI development.
- Optional: Xvfb for mirror mode.

## Install Daemon From Source

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

## Install GUI Release Artifact

The GUI companion app is available as a pre-compiled Debian package (`.deb`) or standalone executable (`.AppImage`) from the releases page.

*Note: Installing or launching the GUI only installs/runs the GUI itself. It does **not** install the `thermalwriter` daemon, the systemd user service, or the udev rule. You must still install the daemon separately.*

### Installation / Run Commands

For Debian/Ubuntu-like systems (via `.deb`):
```sh
sudo apt install ./thermalwriter-config_*_amd64.deb
```

For general Linux distributions (via `.AppImage`):
```sh
chmod +x ./Thermalwriter*.AppImage
./Thermalwriter*.AppImage
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

Config lives at `~/.config/thermalwriter/config.toml`. Built-in layouts are seeded into `~/.config/thermalwriter/layouts/`, backgrounds into `~/.config/thermalwriter/backgrounds/`, and streaming wrapper configs (conky/cava) into `~/.config/thermalwriter/wrappers/`. Existing user files are not overwritten.

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

## GUI Development

The optional GUI source code is in `gui/`:

```sh
cd gui
npm ci
npm run build
npm run tauri:dev
```

The GUI talks to the daemon over D-Bus and can also render local previews.
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
cd gui && npm ci && npm run build && npm run tauri:build
```

## Documentation

For more information, consult the following documentation files:
- [CHANGELOG.md](CHANGELOG.md) - Version history and changes.
- [Configuration Guide](docs/configuration.md) - Full details on `config.toml` structure, defaults, and ranges.
- [GUI Guide](docs/gui.md) - Detailed guide to the Svelte/Tauri-based configuration app.
- [Release Guide](docs/release.md) - Procedures for publishing and creating release packages.
- [Architecture Guide](docs/architecture.md) - Internal design of the daemon and GUI components.
- [Designing Layouts](skills/designing-layouts/SKILL.md) - Guidelines for creating custom LCD layouts.

## License

MIT. See `LICENSE`.
