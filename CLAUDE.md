# thermalwriter

Lightweight Rust daemon to drive Thermalright cooler LCD displays, replacing the 400MB Python/Qt `trcc` app.

## Project State

- **v0.1.0 deployed** — running as systemd user service, hardware-verified
- **GitHub**: https://github.com/mgaruccio/thermalwriter
- **Binary**: 14MB, 29MB RSS, 1% CPU at 2 FPS

## Architecture

Rust daemon with:
- **USB bulk transport** (rusb) — sends JPEG frames to cooler LCD, 180° rotation
- **Pluggable renderers** via `FrameSource` trait in `src/render/mod.rs` (returns `RawFrame` — straight RGB)
  - `SvgRenderer` (primary) — SVG templates + Tera + resvg → Pixmap → RawFrame
  - `XvfbSource` — mmap-based capture from Xvfb virtual framebuffer (any X11 app)
  - `TemplateRenderer` (legacy) — custom HTML subset, taffy + fontdue
  - `BlitzRenderer` (experimental) — behind `--features blitz`, alpha quality
- **Sensor providers** (hwmon, sysinfo, nvidia, amdgpu, mangohud, rapl) — system metrics. `nvidia-smi` polls use a 500 ms `wait_timeout` so a hung GPU driver (D3/TDR) doesn't freeze the tick loop — kill+wait reaps the child and the tick continues with other sensors. `SensorHistory` prunes every `record()` unconditionally — sensor dropout (key absent from the data map) or non-numeric value (e.g. nvidia-smi `"N/A"`) decays buffers within `max_duration` instead of leaving stale ghost samples on history graphs.
- **D-Bus IPC** (zbus) — control interface (`com.thermalwriter.Service`); methods include `SetLayout`/`SetLayoutVars`/`SetDefaultLayout`/`SetBackground`/`ClearBackground`/`ListBackgrounds`/`SetMode`. `TickRate` is a writable property — change at runtime via `busctl set-property`. Heavy work (image decode, file writes) runs outside the state lock so concurrent calls don't block each other. **Concurrency hardening:** writes to `config.toml` are serialized via a process-global mutex + per-write atomic temp-file suffix (no lost updates under concurrent D-Bus calls); `SetBackground`/`ClearBackground` hold a `bg_change_lock` end-to-end (decode → disk → channel → state mirror) so disk, tick channel, and in-memory background never diverge under concurrent invocations.
- **Global background images** — daemon-level bg compositing under any layout. PNG/JPEG files in `~/.config/thermalwriter/backgrounds/` (decoded once, cached as 480×480 premultiplied Pixmap, blitted under each rendered frame)
- **CLI** (clap) — `thermalwriter daemon` / `thermalwriter ctl ...`
- **systemd user service** — auto-starts on login. SIGTERM produces clean shutdown (drains tick loop, closes USB transport, exits in ~300ms — no SIGKILL needed).
- **USB resilience** — partial-write loop in `bulk_usb::write_all` retries on short writes; `try_reconnect` re-establishes the device on `NoDevice`/`Pipe` errors; the D-Bus `Connected` property reflects live device state. Send/reconnect run via `tokio::task::block_in_place` so D-Bus calls stay responsive (~1ms) during USB stalls.
- **Config validation** — `Config::load()` rejects out-of-range values (`tick_rate ∈ [1,60]`, `jpeg_quality ∈ [10,100]`, `rotation ∈ {0,90,180,270}`, `poll_interval_ms ∈ [100,60000]`) with field-named error messages.

## Device Details

- **Cooler**: Thermalright Peerless Vision (reports as "GrandVision 360 AIO")
- **USB**: VID `0x87AD`, PID `0x70DB`, vendor-class bulk interface
- **Protocol**: USB bulk transfers, JPEG frames (cmd=2), 480x480
- **Handshake**: 64-byte magic → 64-byte response, `resp[24]=PM` (4), `resp[36]=SUB` (5)
- **Display orientation**: LCD mounted 180° rotated — frames need rotation before sending (configurable)

## Commands

```bash
cargo build                              # build
cargo test                               # run tests (214 tests)
cargo run --example preview_layout <name_or_path>  # render to PNG (no USB)
cargo run --example render_layout <name_or_path> [secs] [--mock]  # push to device
cargo run --example send_test_frame      # solid red hardware test
cargo run -- bench                       # USB throughput benchmark (~750 FPS)
systemctl --user status thermalwriter    # check daemon status
thermalwriter ctl status                 # query daemon via D-Bus
thermalwriter ctl mirror "command"       # xvfb capture mode (any X11 app)
thermalwriter setup-udev                 # one-shot: install udev rule for RAPL cpu_power access (re-execs under sudo)
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
- Built-in SVG layouts: svg/neon-dash-v2 (default), svg/neon-dash, svg/arc-gauge, svg/cyber-grid — all use transparent canvases so the global bg shows through. Per-panel rects survive. cyber-grid keeps its scanlines overlay (intentional cosmetic on top of any bg).
- Built-in HTML layouts: system-stats, gpu-focus, minimal, neon-dash, dual-gauge, fps-hero

## Config

`~/.config/thermalwriter/config.toml`:
```toml
[display]
tick_rate = 2
default_layout = "svg/neon-dash-v2.svg"
jpeg_quality = 85
rotation = 180  # 0, 90, 180, 270
mode = "svg"    # "svg", "html", or "xvfb"

[sensors]
poll_interval_ms = 1000

[background]
image = "skyline.png"  # filename only, lives under ~/.config/thermalwriter/backgrounds/. Omit or unset for no bg.

[xvfb]
command = "conky -c ~/.config/conky/lcd.conf"
tick_rate = 15  # 1-60 FPS for xvfb capture mode
```

Layouts in `~/.config/thermalwriter/layouts/` — built-in layouts seeded on first run.
Backgrounds in `~/.config/thermalwriter/backgrounds/` — placeholder PNGs seeded on first run; drop your own 480×480 PNG/JPEG and select via `busctl call ... SetBackground` or the Tauri GUI's BgGallery.

## Key Dependencies

rusb, zbus, tiny-skia, resvg, taffy, tera, fontdue, image, memmap2, sysinfo, tokio, clap, dirs, wait-timeout
