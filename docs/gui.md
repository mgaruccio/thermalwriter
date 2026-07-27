# Graphical User Interface (GUI)

The `thermalwriter` project includes a Svelte/Tauri-based graphical configuration tool, **Thermalwriter Config**, which provides an intuitive interface for managing your LCD cooler layouts, backgrounds, variables, and screen streaming.

## Installation & Requirements

Before using the GUI, you **must install the daemon first** (using the daemon source installer or package). 

While the GUI can perform several actions offline (without the daemon running), features like **Live Apply** and **Streaming** require a running `thermalwriter` daemon.

---

## Offline Features

When the daemon is offline, the status bar will show `Daemon · Offline`. You can still:
- **Browse and Preview layouts**: Select any layout to render a local 480x480 preview.
- **Save Configuration offline**: Clicking **Apply** saves your layout variables and settings to `~/.config/thermalwriter/config.toml` so they load automatically on the next daemon start.

---

## Layout Variables

The **Variables** tab allows you to configure layout-specific options:
- It dynamically reads and lists the frontmatter variables declared in the currently selected layout.
- You can override values (e.g. toggling charts, changing labels, or modifying ranges).
- Overrides are saved per-layout under the `[layout_vars]` section in the configuration file.

---

## Background Gallery

The **Backgrounds** panel allows you to customize the image displayed behind your layout.
- You can select **None** or choose from imported backgrounds.
- The gallery supports importing custom PNG and JPEG images.
- **Validation**: Imports are limited to a maximum of 8 MB. Bytes are decoded and verified to ensure they are valid images and can be successfully downscaled to the LCD's 480x480 resolution before being copied to `~/.config/thermalwriter/backgrounds/`.

---

## Screen Streaming (Stream Tab)

The **Stream** tab allows you to capture a window or application running inside a virtual frame buffer and pipe it directly to your cooler's LCD.

### Requirements
- **Xvfb** (`xorg-server-xvfb` or `xvfb` package) must be installed on your system. The GUI will check for the `Xvfb` binary on startup.

### Presets
The interface supports several presets for popular monitoring tools and custom sources:
- **Conky**: Renders a system-monitoring overlay. Requires a valid `.conf` file.
- **Cava**: Renders an audio visualizer. Requires a valid config file.
- **btop**: Full-featured terminal system monitor. Run inside a terminal emulator wrapper.
- **nvtop**: Terminal GPU status monitor. Run inside a terminal emulator wrapper.
- **Custom...**: Run an arbitrary command or script as your user inside Xvfb. The executable must be selected as an absolute path; streaming is a same-user session-bus feature, not a privilege boundary.

### Terminal Emulator Fallbacks
For presets that run a text user interface (TUI) and need a terminal window (like `btop` and `nvtop`), the GUI probes the system for installed terminal emulators in this preference order:
1. `alacritty`
2. `kitty`
3. `xterm`

The daemon validates streamed executables as absolute paths before launch. Built-in presets are resolved against the daemon's own `PATH`, while custom commands use the executable path chosen in the GUI. Streamed child processes run as the daemon user with stdout/stderr hidden in the UI; use only commands you trust. The public streaming path uses structured argv rather than `sh -c`.

### Target Framerate (FPS)
- You can adjust the stream frame rate using the FPS slider or number input.
- Allowed FPS range: **`1..=60`**.

---

## System Tray

`thermalwriter-tray` is a separate, lightweight StatusNotifierItem process (not the Tauri GUI). It stays idle on the session bus with no timers and no WebKit, and talks to the daemon through the same D-Bus API as `thermalwriter ctl`.

### Controls
- **Left-click** — open or focus the Config GUI (`thermalwriter-gui`, or `$THERMALWRITER_GUI`)
- **Right-click menu**
  - **Open Config…**
  - **Layouts** — quick-switch any layout the daemon reports (active layout marked with `✓`)
  - **Stream** — start `conky` / `cava` / `btop` presets; **Return to layout** while streaming
  - **Reload config** / **Refresh status** / **Stop daemon** / **Quit tray**

The menu is text-only (no freedesktop theme icon names). Hosts like Noctalia/Quickshell render missing-icon tiles for unresolved `icon-name` values, so the tray intentionally omits them.

### Install

Source installs enable the tray by default:

```sh
./packaging/install.sh          # daemon + tray
INSTALL_TRAY=0 ./packaging/install.sh   # daemon only
cargo install --path tray --locked      # tray binary alone
```

The installer writes `~/.config/systemd/user/thermalwriter-tray.service` (`WantedBy=graphical-session.target`) and an XDG autostart entry as a fallback. Start manually with `thermalwriter-tray`.

Override the GUI launcher if it is not on `PATH`:

```sh
export THERMALWRITER_GUI=/path/to/Thermalwriter.AppImage
# or install the binary somewhere on PATH:
#   install -m0755 target/release/thermalwriter-gui ~/.cargo/bin/
```

### Desktop notes (Hyprland / Noctalia)

- The tray icon is an embedded multi-size **pixmap** with an empty `IconName`. Quickshell prefers `IconName` via `QIcon::fromTheme` and does **not** fall back to `IconPixmap` when the theme name is missing, so a non-empty unresolved name becomes the purple missing-icon tile.
- Left-click launches the GUI detached (`hyprctl dispatch exec`, then `systemd-run --user`, then a direct spawn). After launch the tray moves/focuses the window onto the **current** Hyprland workspace (Tauri often restores on a stale workspace otherwise).
- The tray resolves the live Hyprland instance by lock file + `.socket.sock` under `$XDG_RUNTIME_DIR/hypr/`, because `/proc/Hyprland/environ` can retain a stale `HYPRLAND_INSTANCE_SIGNATURE`.
- On Noctalia, pin `Thermalwriter*` / `thermalwriter*` in the bar Tray widget if you want the icon inline instead of only in the tray drawer.
