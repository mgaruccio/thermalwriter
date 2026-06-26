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
- The gallery supports importing custom images. It handles PNG, JPEG, and GIF-capable image formats under the hood.
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
- **Custom...**: Run an arbitrary command or script (requires entering a path to the executable).

### Terminal Emulator Fallbacks
For presets that run a text user interface (TUI) and need a terminal window (like `btop` and `nvtop`), the GUI probes the system for installed terminal emulators in this preference order:
1. `alacritty`
2. `kitty`
3. `xterm`

### Target Framerate (FPS)
- You can adjust the stream frame rate using the FPS slider or number input.
- Allowed FPS range: **`1..=60`**.
