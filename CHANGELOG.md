# Changelog

All notable changes to this project will be documented in this file.

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
