# thermalwriter

Lightweight Rust daemon to drive Thermalright cooler LCD displays, replacing the 400MB Python/Qt `trcc` app.

## Project State

- **v0.1.0 deployed** — running as systemd user service, hardware-verified
- **GitHub**: https://github.com/mgaruccio/thermalwriter
- **Binary**: 14MB, 29MB RSS, 1% CPU at 2 FPS

## Architecture

Rust daemon with:
- **USB bulk transport** (rusb) — sends JPEG frames to cooler LCD, 180° rotation
- **Pluggable renderers** via `FrameSource` trait in `src/render/mod.rs`
  - `SvgRenderer` (primary) — SVG templates + Tera + resvg → Pixmap
  - `TemplateRenderer` (legacy) — custom HTML subset, taffy + fontdue
  - `BlitzRenderer` (experimental) — behind `--features blitz`, alpha quality
- **Sensor providers** (hwmon, sysinfo, nvidia, amdgpu, mangohud, rapl) — system metrics
- **D-Bus IPC** (zbus) — control interface (`com.thermalwriter.Service`)
- **CLI** (clap) — `thermalwriter daemon` / `thermalwriter ctl ...`
- **systemd user service** — auto-starts on login

## Device Details

- **Cooler**: Thermalright Peerless Vision (reports as "GrandVision 360 AIO")
- **USB**: VID `0x87AD`, PID `0x70DB`, vendor-class bulk interface
- **Protocol**: USB bulk transfers, JPEG frames (cmd=2), 480x480
- **Handshake**: 64-byte magic → 64-byte response, `resp[24]=PM` (4), `resp[36]=SUB` (5)
- **Display orientation**: LCD mounted 180° rotated — frames need rotation before sending (configurable)

## Commands

```bash
cargo build                              # build
cargo test                               # run tests (57 tests)
cargo run --example preview_layout <name_or_path>  # render to PNG (no USB)
cargo run --example render_layout <name_or_path> [secs] [--mock]  # push to device
cargo run --example send_test_frame      # solid red hardware test
systemctl --user status thermalwriter    # check daemon status
thermalwriter ctl status                 # query daemon via D-Bus
```

### Layout Development

```bash
# Preview (fast iteration):
cargo run --example preview_layout layouts/svg/neon-dash.svg
# Push to hardware (stop daemon first):
systemctl --user stop thermalwriter
cargo run --example render_layout layouts/svg/neon-dash.svg 15
systemctl --user start thermalwriter
# Use --mock for gaming-load fake data (FPS, high temps):
cargo run --example render_layout fps-hero 15 --mock
```

## Layout Authoring

See `skills/designing-layouts/SKILL.md` for the full design system.

SVG is the primary layout format. HTML layouts still work via the legacy TemplateRenderer.

Key gotchas:
- LCD backlight washes out dim text — use opacity >= 0.7, colors >= #999999, labels >= 14px
- SVG text uses absolute x/y positioning (no flexbox) — 480x480 fixed canvas
- HTML layouts: every text element needs explicit `height` (taffy can't measure text)
- HTML layouts: comments (`<!-- -->`) break the custom parser
- Seeded layouts in ~/.config/thermalwriter/layouts/ don't auto-update — copy manually after changes
- Built-in SVG layouts: svg/neon-dash, svg/arc-gauge, svg/cyber-grid
- Built-in HTML layouts: system-stats, gpu-focus, minimal, neon-dash, dual-gauge, fps-hero

## Config

`~/.config/thermalwriter/config.toml`:
```toml
[display]
tick_rate = 2
default_layout = "svg/neon-dash.svg"
jpeg_quality = 85
rotation = 180  # 0, 90, 180, 270

[sensors]
poll_interval_ms = 1000
```

Layouts in `~/.config/thermalwriter/layouts/` — built-in layouts seeded on first run.

## Key Dependencies

rusb, zbus, tiny-skia, resvg, taffy, tera, fontdue, image, sysinfo, tokio, clap, dirs
