# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added
- Tracked the ordered Thermalright Trofeo Vision 6.86-inch IPS 1280×480 panel as capture-pending, with a hardware-free HID Type 2 (`0416:5302`) PM68/FBL192 profile fixture.
- Mirrored multi-display mode via `display.device = "all"`: opens every attached supported LCD, renders once at a deterministic primary resolution, letterboxes per output, encodes for each negotiated profile, and reports `display_count` in status.

## [0.1.4] - 2026-07-28

### Added
- Linux release compatibility: CI builds on `ubuntu-22.04` (glibc 2.35); L0 QA asserts prebuilt daemon/tray/GUI binaries require GLIBC ≤ 2.35; README documents x86_64/glibc baseline and `APPIMAGE_EXTRACT_AND_RUN=1`.
- Vendored GUI fonts (Major Mono Display, IBM Plex Mono/Sans) with local `gui/src/fonts.css` — release GUI makes no external font requests; `THIRD_PARTY_NOTICES.md` covers embedded DejaVu + GUI fonts.
- `docs/troubleshooting.md` and `docs/agent-testing.md` (Tauri MCP dev bridge runbook).
- `thermalwriter --version` CLI output.
- Tray install `INSTALL_TRAY=auto` (default): installs tray only when a Config GUI is discoverable; `INSTALL_TRAY=1` forces, `INSTALL_TRAY=0` skips.

### Changed
- Refined the Config GUI into a quieter display-studio interface with neutral floating surfaces, clearer labels, and streamlined Stream controls.
- GitHub release badge includes prereleases; tag pushes create non-prerelease Latest releases with per-version CHANGELOG notes; `workflow_dispatch` can set tag + prerelease flag.
- Tauri MCP bridge is optional behind the `devtools` feature (localhost bind `127.0.0.1`); release builds do not link the plugin or ship its ACL.
- AppImage docs: clarify WebKitGTK/GTK bundling, FUSE requirement, and non-standalone behavior; Arch L1 QA tries minimal deps before installing host WebKitGTK.
- README: removed public “automatic USB reconnect” marketing claim (reconnect logic unchanged).

### Fixed
- Updated locked Svelte/Vite frontend dependencies to patched releases; `npm audit` reports zero known vulnerabilities.
- Release QA tray assertions: expect systemd **or** XDG autostart, not both; auto tray mode on clean VMs without GUI.
- Release workflow checks out the requested tag (workflow_dispatch), validates semver tags, sets Latest/prerelease explicitly, runs L0 checks pre-publish, and keeps release notes out of SHA256SUMS.
- Tray install skips now remove any previously installed tray service/autostart/binary.
- MCP capabilities use `capabilities_path_pattern` (no source-tree mutation); GUI bundles ship font/OFL notices.
- Agent-testing runbook uses `dbus-run-session` for true D-Bus isolation; MCP grep check is pipefail-safe; GUI bundles include DejaVu license.

## [0.1.3] - 2026-07-27

### Added
- Gallery announce media: animated layout GIFs (neon-dash-v2 lead, neon-dash, arc-gauge, cyber-grid) and a live `cava` stream GIF under `docs/assets/gallery/`. Offline multi-frame helper: `cargo run --release --example render_gallery_frames`.

### Fixed
- Xvfb stream sessions start with `-nocursor` so cooler frames / GUI stream preview / cava captures no longer include a pointer sprite.
- Tray no longer double-registers on desktops that run both systemd user units and XDG autostart (installer prefers one path; binary takes a single-instance flock on `$XDG_RUNTIME_DIR/thermalwriter/tray.lock`).

## [0.1.2] - 2026-07-27

### Added
- `thermalwriter-tray`: lightweight StatusNotifierItem controller (`ksni`) for opening the Config GUI, switching layouts, starting stream presets, reloading config, and stopping the daemon. Separate process from both the headless daemon and the Tauri GUI; idle path is D-Bus event-driven with no polling timers. Optional install via `packaging/install.sh` (`INSTALL_TRAY=0` to skip) plus `systemd/thermalwriter-tray.service` and XDG autostart.
  - Pixmap-only tray icon (empty `IconName`) so Quickshell/Noctalia does not show a missing-icon tile.
  - Text-only context menu (no unresolved freedesktop `icon-name` entries); active layout marked with `✓`.
  - Detached GUI launch (`hyprctl` / `systemd-run`) with focus/move onto the current Hyprland workspace; live compositor instance resolved via lock + `.socket.sock`.
- Release tarball now ships `bin/thermalwriter-tray`, `packaging/thermalwriter-tray.desktop`, and `systemd/thermalwriter-tray.service` alongside the daemon.
- Clean-machine release QA harness under `scripts/release-qa/` (L0 artifact checks, multi-distro L1 install matrix, optional L2 host cable smoke).

### Fixed
- Release tarball stages `docs/comparison-methodology.md` (README relative link was broken in the published `v0.1.1` archive).
- Cooler unplug/replug auto-detect no longer waits on Enter prompts during host cable smoke.

## [0.1.1] - 2026-07-25

### Performance
- Tick loop skips render/encode/USB send when the active layout's resolved inputs are unchanged (`FrameSource::content_fingerprint`). Streaming (Xvfb) sources still render every tick.
- NVIDIA GPU sensors prefer in-process NVML (dedicated worker, hard 500ms recv timeout so a wedged driver cannot pin the tick loop) over forking `nvidia-smi` every poll; smi remains a fallback with the same budget + kill, and both paths back off when unusable. NVML `device_by_index` failures demote to smi (not silent empty). (#80, #91)
- Hybrid AMD+NVIDIA machines register Nvidia before AmdGpu so the discrete GPU owns `gpu_*` / `vram_*` keys.
- SensorHub collision warnings log once per key for the life of the hub (no per-poll journal spam).
- hwmon skips high-latency JEDEC `spd5118` DIMM sensors (~8 ms/poll on typical boards) that no stock layout displays.
- Adaptive hwmon pruning: after a short discovery window, chips that cannot serve the active layout's needed metrics are skipped (EMA cost > 500µs or simply unneeded).
- Sensor polling is limited to keys the active layout can display (frontmatter + template token scan against the catalog), with provider short-circuit once those keys are collected.
- Default `sensors.poll_interval_ms` lowered from `1000` to `2000` (still one sensor refresh per display frame at the default 2 FPS).
- `list_sensors` reports per-key poll cost (`cost_us`); GUI sensor pickers show live-measured cost so setup can avoid expensive keys.
- Steady-state daemon footprint on the comparison machine: **0.41% of one core** and ~81 MB PSS at default 2 FPS (stock neon-dash, NVML, dirty-frame skip, 2000 ms poll). Earlier post-`v0.1.0` remasure was 0.87% / ~72 MB PSS before the 2000 ms poll + layout-needed prune path.

### Fixed
- Clippy `-D warnings` clean again on CI (`type_complexity` on the shared sensor catalog, duplicated `too_many_arguments` allow, plus a couple of follow-on lints).

## [0.1.0] - 2026-07-24

### Added
- Initial daemon implementation for controlling Thermalright LCD coolers.
- Multi-transport device support: raw bulk, SCSI, HID Type 2/3, and LY bulk protocols across nine USB IDs, with automatic discovery and config-overridable `VID:PID` selection. `87ad:70db` (Peerless Vision / GrandVision 360 AIO) is verified on physical hardware; the remaining IDs are implemented against protocol fixtures — testers wanted.
- Responsive layout rendering at negotiated native resolutions (240x240 through ultrawide), with `{# canvas: responsive #}` opt-in reflow and uniform containment for fixed-canvas layouts.
- Support for rendering layout templates using SVG and Tera engine, including built-in layouts.
- Integrated sensor providers for system temperature, load, power, and memory metrics, with slow/wedged hwmon chip quarantine.
- D-Bus control command-line interface (CLI) to interface with the running daemon.
- RAPL udev configuration rule generator to safely allow reading processor power metrics without root.
- Optional Tauri-based graphical user interface (GUI) for visual configuration and streaming, including overlay color suggestions derived from the selected background image.
- Support for setting background images on the cooler LCD.
- Xvfb streaming presets to pipe arbitrary window regions (e.g. conky, cava, btop, nvtop, custom terminal emulators) straight to the cooler.
- Whole-daemon profiling harness with committed performance baselines (`scripts/profile.sh`, `docs/profiling-baselines.md`).


### Changed
- Release tarball installer now uses the bundled daemon binary when present, while source checkouts still build with Cargo.
- RAPL access now uses a dedicated `thermalreader` group with `0440` permissions instead of world-readable counters.
- Xvfb streaming now uses generated Xauthority credentials and private runtime preview frames.

### Fixed
- Stream restarts preserve the original layout tick rate when returning from Xvfb mode.
- Layout switches now wait for renderer startup confirmation before updating D-Bus state.
- SVG text variables are XML-escaped before rendering, so labels containing characters like `&` do not break previews or daemon rendering.
