# Repository Guidelines

## Project Structure & Module Organization

`thermalwriter` is a Rust 2024 daemon for Thermalright LCD coolers. Core source lives in `src/`: `main.rs` and `cli.rs` wire the binary, `config.rs` handles user config, `transport/` owns USB bulk transfer, `render/` contains SVG/HTML/Xvfb frame sources, `sensor/` contains metric providers, and `service/` contains daemon tick, D-Bus, and Xvfb code. Integration tests live in `tests/`. Development utilities live in `examples/`. Built-in layouts are under `layouts/`, with fonts and assets in `assets/`. Deployment files are in `packaging/` and `systemd/`. Plans are in `docs/plans/`; layout guidance is in `skills/designing-layouts/`.

## Build, Test, and Development Commands

- `cargo build` builds the crate with default features.
- `cargo test` runs the full test suite.
- `cargo run -- bench` runs the USB throughput benchmark.
- `cargo run --example preview_layout layouts/svg/neon-dash-v2.svg` renders a layout preview PNG without USB hardware.
- `cargo run --example render_layout layouts/svg/neon-dash-v2.svg 15 --mock` renders to the device for 15 seconds with mock sensor data.
- `cargo run --features blitz --example preview_blitz layouts/blitz-glass.html` exercises the experimental Blitz renderer.
- `./packaging/install.sh` installs the binary, user systemd service, and udev rule; it may prompt for sudo.

## Coding Style & Naming Conventions

Use standard Rust formatting (`cargo fmt`) and keep modules aligned with existing domain boundaries. Prefer explicit error context with `anyhow` at application edges and typed errors with `thiserror` in reusable modules. Use `snake_case` for files, modules, functions, and tests; `PascalCase` for types and traits. Renderers should return straight RGB `RawFrame`s through `FrameSource`.

## Testing Guidelines

Tests are integration-focused and named by behavior area, for example `tests/render_tests.rs`, `tests/config_tests.rs`, and `tests/sensor_history_tests.rs`. Add tests near the affected subsystem and prefer deterministic mock data over hardware access. Run `cargo test` before submitting; run relevant examples when touching rendering, layouts, USB transport, or device-facing code.

## Commit & Pull Request Guidelines

Recent history uses Conventional Commit prefixes such as `feat:`, `fix:`, and `docs:`. Keep subjects imperative and scoped to one change. Pull requests should include a summary, testing performed, hardware impact, and screenshots or rendered previews for layout/UI changes. Mention setup changes, especially systemd, udev, D-Bus, or config behavior.

## Agent-Specific Instructions

Do not assume hardware is attached. Prefer preview examples and tests before commands that talk to USB devices or modify user services. When editing layouts, consult `skills/designing-layouts/SKILL.md` and remember the target canvas is a 480x480 LCD with washed-out low-contrast colors.
