# Changelog

All notable changes to this project will be documented in this file.

## [0.1.0] - 2026-06-26

### Added
- Initial daemon implementation for controlling Thermalright LCD coolers.
- Support for rendering layout templates using SVG and Tera engine, including built-in 480x480 layouts.
- Integrated sensor providers for system temperature, load, power, and memory metrics.
- D-Bus control command-line interface (CLI) to interface with the running daemon.
- RAPL udev configuration rule generator to safely allow reading processor power metrics without root.
- Optional Tauri-based graphical user interface (GUI) for visual configuration and streaming.
- Support for setting background images on the cooler LCD.
- Xvfb streaming presets to pipe arbitrary window regions (e.g. conky, cava, btop, nvtop, custom terminal emulators) straight to the cooler.
- Hardware verification and support, currently limited to Thermalright Peerless Vision / GrandVision 360 AIO with USB ID `87ad:70db`.
