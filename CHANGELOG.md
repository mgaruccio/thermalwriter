# Changelog

All notable changes to this project will be documented in this file.

## [0.1.0] - 2026-07-24

### Performance
- Tick loop skips render/encode/USB send when the active layout's resolved inputs are unchanged (`FrameSource::content_fingerprint`). Streaming (Xvfb) sources still render every tick.
- NVIDIA GPU sensors prefer in-process NVML over forking `nvidia-smi` every poll; smi remains a fallback, with backoff when neither path is available (#80, #91).
- Hybrid AMD+NVIDIA machines register Nvidia before AmdGpu so the discrete GPU owns `gpu_*` / `vram_*` keys.
- SensorHub collision warnings log once per key for the life of the hub (no per-poll journal spam).
- Steady-state daemon CPU on the comparison machine dropped from 2.52% → **0.86%** of one core at default 2 fps (below TRCC-Linux's 1.06% default).

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