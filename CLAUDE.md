# thermalwriter

Lightweight Rust daemon to drive Thermalright cooler LCD displays, replacing the 400MB Python/Qt `trcc` app.

## Quick Start for New Sessions

To continue implementation, run:
```
/forge:executing-plans
```

Then point it at the plan: `docs/plans/2026-03-23-thermalwriter-impl.md`

## Project State

- **Design doc**: `docs/plans/2026-03-23-thermalwriter-design.md` (committed)
- **Implementation plan**: `docs/plans/2026-03-23-thermalwriter-impl.md` (committed, 24 tasks across 5 phases)
- **Current status**: No implementation started yet. Begin at Task 1.

## Architecture

Rust daemon with:
- **USB bulk transport** (rusb) — sends JPEG frames to cooler LCD
- **HTML/CSS template rendering** (taffy + tiny-skia + tera) — layout engine
- **Sensor providers** (hwmon, sysinfo, amdgpu, mangohud) — system metrics
- **D-Bus IPC** (zbus) — control interface
- **CLI** (clap) — thin D-Bus client
- **systemd user service** — lifecycle management

## Device Details

- **Cooler**: Thermalright Peerless Vision (reports as "GrandVision 360 AIO")
- **USB**: VID `0x87AD`, PID `0x70DB`, vendor-class bulk interface
- **Protocol**: USB bulk transfers, JPEG frames (cmd=2), 480x480
- **Handshake**: 64-byte magic → 1024-byte response, `resp[24]=PM`, `resp[36]=SUB`

## Key Dependencies

rusb, zbus, tiny-skia, taffy, tera, fontdue, image, sysinfo, tokio, clap

## Commands

```bash
cargo build          # build
cargo test           # run tests
cargo run --example send_test_frame  # hardware validation (device must be plugged in)
```

## Reference Implementation

The existing Python trcc is installed at `/usr/lib/python3.14/site-packages/trcc/`. Key files:
- `adapters/device/bulk.py` — USB bulk protocol (the protocol we're reimplementing)
- `adapters/device/_usb_helpers.py` — USB device lifecycle
- `adapters/device/detector.py` — device registry
