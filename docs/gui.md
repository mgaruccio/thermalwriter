# Graphical User Interface (GUI)

The `thermalwriter` project includes a Svelte/Tauri-based graphical configuration tool, **Thermalwriter Config**, which provides an intuitive interface for managing your LCD cooler layouts, backgrounds, variables, and screen streaming.

## Installation & Requirements

Before using the GUI, you **must install the daemon first** (using the daemon source installer or package). 

While the GUI can perform several actions offline (without the daemon running), the typed composer can preview, diagnose, and save drafts locally. Activating a typed layout and **Streaming** require a running `thermalwriter` daemon.

---

## Offline Features

When the daemon is offline, the status bar will show `Daemon · Offline`. You can still:
- **Browse and Preview layouts**: Select any layout for a local preview. Typed `.layout.toml` compositions preview at the selected native profile (square, portrait, wide, or curved); legacy SVG/HTML layouts still preview at 480×480.
- **Save Configuration offline**: Clicking **Apply** saves your layout variables and settings to `~/.config/thermalwriter/config.toml` so they load automatically on the next daemon start.

---

## Compose a layout

The **Compose** tab is the owner workflow for new typed layouts. It starts from the shipped **Neon Composer** preset; no renderer source editing is needed.

1. **Choose a preset**
   - Open **Compose**, enter a name in **Name your layout**, select **Neon Composer**, and click **Use preset**.
   - To continue later, choose a file under **Reopen a saved layout** and click **Reopen**.
2. **Edit modules**
   - In **Ordered modules**, add **Metric**, **Sparkline**, **Text**, or **Media**.
   - Select **Configure** for a module, choose its sensor/history/media binding and supported presentation options in the inspector, then use **Move up**, **Move down**, or **Remove module**. The list order is solve order.
   - Media sources are relative files under the approved layout/media directory. Do not use arbitrary paths.
3. **Preview the target surface**
   - In **Preview profile**, choose **Square** (`480 × 480`), **Portrait** (`480 × 1280`), **Wide** (`1280 × 480`), or **Curved** (`2400 × 1080`).
   - The preview uses native pixels; the window only scales its presentation. Curved preview draws illustrative **left/right readable zones** and a **protected bridge**. This is a conservative topology guide, not calibrated optical warp.
   - On the curved profile, the topology is full-height `left-readable` `x=0..960`, protected `center-bridge` `x=960..1440`, and `right-readable` `x=1440..2400` (40% / 20% / 40%). Metric, Sparkline, and Text stay in readable zones. Media can cross the bridge only when the profile allows it and **Allow bridge span** is enabled for that Media module.
4. **Fix validation**
   - When the draft has problems, **Layout diagnostics** shows the stable code, severity, profile, module, property, reason, and fix. Correct the indicated field and wait for the preview to refresh; an invalid target returns diagnostics instead of a frame.
5. **Save and Apply**
   - **Save layout** writes the named `.layout.toml` document to the configured layout directory. It is safe to reopen later; a fingerprint conflict is reported instead of overwriting an external edit.
   - **Save & activate** saves the document and asks a running daemon over D-Bus to select it and persist it as the default. The composer reports whether it is **Active** or saved but not activated.

### Online and offline

- Preview, diagnostics, and saving a typed draft are local GUI operations and work while the status bar says **Daemon · Offline**. No LCD hardware is required for the native profile preview.
- Activation is the online step. If the daemon or session bus is unavailable, **Save & activate** still saves the document and reports that activation was not completed. Start/restart the user daemon, then reopen the saved document and click **Save & activate**:

  ```sh
  systemctl --user restart thermalwriter
  thermalwriter ctl status
  ```
- A terminal-only one-session switch after the daemon is back is:

  ```sh
  thermalwriter ctl layout my-neon-layout.layout.toml
  ```
  Use **Save & activate** in the GUI when you also want the selected layout persisted as the default.

> **Breaking transition:** Layout Studio supports typed `.layout.toml` documents only. Old SVG/Tera and HTML source layouts are unsupported by the composer and are left untouched; keep using their existing Variables/preview/Apply path instead. There is no silent import or conversion.

## Ask an agent for help

- **Recommended: Codex or Claude Code** — They can inspect the repository, run the real validation/preview commands, open every generated PNG, and iterate. Start with the [`.layout.toml` authoring reference](../skills/designing-layouts/references/layout-toml.md).
- **Copy Error** — In **Layout diagnostics**, select one issue and click **Copy Error**. This copies that issue's code, location, profile/module/property, reason, and suggested fix.
- **Copy Preview Image** — In the composition preview, click **Copy preview image** (the button uses a lowercase `preview`). It copies the exact visible native PNG, including the curved topology overlay; if image clipboard access is unavailable, the GUI downloads the PNG instead.
- **Copy Design Context** — With a draft open, click **Copy Design Context**. It copies Markdown describing the selected profile, ordered modules, bindings/options, solved geometry, and diagnostics without embedding runtime media bytes.

These are separate handoff artifacts: **Copy Error** diagnoses one issue, **Copy Preview Image** supplies the visual result, and **Copy Design Context** explains the document and solve. Paste any or all three into a disconnected chatbot; it does not need repository or tool access. The [layout-design skill](../skills/designing-layouts/SKILL.md) and [copyable bootstrap prompt](../skills/designing-layouts/references/bootstrap-prompt.md) describe the same generate → validate → preview → inspect → iterate loop.

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

Source installs use **`INSTALL_TRAY=auto`** (default): the tray is installed only when a Config GUI is discoverable. Force or skip:

```sh
./packaging/install.sh                   # daemon; tray if GUI found
INSTALL_TRAY=1 ./packaging/install.sh    # force tray (GUI-less menu OK)
INSTALL_TRAY=0 ./packaging/install.sh    # daemon only
cargo install --path tray --locked       # tray binary alone
```

The installer enables **either** `~/.config/systemd/user/thermalwriter-tray.service` (`WantedBy=graphical-session.target`) **or** an XDG autostart `.desktop` entry when systemd enable fails — never both.

Override the GUI launcher if it is not on `PATH`:

```sh
export THERMALWRITER_GUI=/path/to/Thermalwriter.AppImage
# or install the binary somewhere on PATH:
#   install -m0755 target/release/thermalwriter-gui ~/.cargo/bin/
```

### Desktop notes

StatusNotifierItem hosts validated for registration:

| Host | Notes |
| --- | --- |
| **Hyprland + Quickshell/Noctalia** | Dev host. Pixmap-only icon (empty `IconName`) — Quickshell does not fall back from a missing theme name to `IconPixmap`. |
| **GNOME (Ubuntu) + AppIndicator extension** | Needs `gnome-shell-extension-appindicator` (shipped/enabled on Ubuntu desktop). Extension provides `org.kde.StatusNotifierWatcher`. |
| **KDE Plasma** | Native SNI via `kded` StatusNotifierWatcher. |

Hyprland-specific launch behavior:

- Left-click launches the GUI detached (`hyprctl dispatch exec`, then `systemd-run --user`, then a direct spawn). After launch the tray moves/focuses the window onto the **current** Hyprland workspace (Tauri often restores on a stale workspace otherwise).
- The tray resolves the live Hyprland instance by lock file + `.socket.sock` under `$XDG_RUNTIME_DIR/hypr/`, because `/proc/Hyprland/environ` can retain a stale `HYPRLAND_INSTANCE_SIGNATURE`.
- On Noctalia, pin `Thermalwriter*` / `thermalwriter*` in the bar Tray widget if you want the icon inline instead of only in the tray drawer.

Menu items are text-only on every host (no freedesktop `icon-name` entries) so hosts that render missing-icon tiles stay clean.
