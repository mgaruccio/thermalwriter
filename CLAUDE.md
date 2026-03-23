# thermalwriter

Lightweight Rust daemon to drive Thermalright cooler LCD displays, replacing the 400MB Python/Qt `trcc` app.

## Project State

- **v0.1.0 deployed** — running as systemd user service, hardware-verified
- **GitHub**: https://github.com/mgaruccio/thermalwriter
- **Binary**: 14MB, 29MB RSS, 1% CPU at 2 FPS

## Architecture

Rust daemon with:
- **USB bulk transport** (rusb) — sends JPEG frames to cooler LCD, 180° rotation
- **HTML/CSS template rendering** (taffy + tiny-skia + tera + fontdue) — layout engine
- **Sensor providers** (hwmon, sysinfo, nvidia, amdgpu, mangohud) — system metrics
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
cargo run --example preview_layout       # render layout to PNG (no USB)
cargo run --example render_layout        # render + push to device (60s)
cargo run --example send_test_frame      # solid red hardware test
systemctl --user status thermalwriter    # check daemon status
thermalwriter ctl status                 # query daemon via D-Bus
```

## Config

`~/.config/thermalwriter/config.toml`:
```toml
[display]
tick_rate = 2
default_layout = "system-stats.html"
jpeg_quality = 85
rotation = 180  # 0, 90, 180, 270

[sensors]
poll_interval_ms = 1000
```

Layouts in `~/.config/thermalwriter/layouts/` — built-in layouts seeded on first run.

## Key Dependencies

rusb, zbus, tiny-skia, taffy, tera, fontdue, image, sysinfo, tokio, clap, dirs
