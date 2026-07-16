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
- **D-Bus IPC** (zbus) — control interface (`com.thermalwriter.Service`); methods include `SetLayout`/`SetLayoutVars`/`SetDefaultLayout`/`SetBackground`/`ClearBackground`/`ListBackgrounds`/`SetMode`/`SetModeArgv`/`StartStreamPreset`/`ResolveBinaries`/`GetStatus`. `TickRate` is a writable property — change at runtime via `busctl set-property`. Heavy work (image decode, file writes) runs outside the state lock so concurrent calls don't block each other. **Concurrency hardening:** writes to `config.toml` are serialized via a process-global mutex + per-write atomic temp-file suffix (no lost updates under concurrent D-Bus calls); `SetBackground`/`ClearBackground` hold a `bg_change_lock` end-to-end (decode → disk → channel → state mirror) so disk, tick channel, and in-memory background never diverge under concurrent invocations. **Mode transitions** (layout↔stream) hold a `mode_change_lock` end-to-end and confirm the swap via a per-`ModeChange` oneshot `ack` before mutating `state.mode`/`active_layout`/`tick_rate` — a failed transition (bad command, dead child) returns a D-Bus error and leaves state unchanged. The old `xvfb_handle` is dropped only after the replacement source is confirmed sent (a failed layout read while streaming keeps the live stream rendering).
- **App streaming (Xvfb capture)** — streams any X11 app (conky/cava/btop) captured from a hidden Xvfb framebuffer to the LCD. `SetModeArgv(argv)` is the generic entry point (GUI builds the full argv); `StartStreamPreset(name)` is a CLI convenience mapping conky/cava/btop to argv. Display allocated atomically via `Xvfb -displayfd` starting at base `:100` (avoids colliding with the live desktop `:1`). All streamed children get `SDL_VIDEODRIVER=x11` (the daemon env carries `WAYLAND_DISPLAY`, which crashes SDL apps like cava) and `.process_group(0)`; a 150 ms liveness check fails the transition if the child exits immediately. **Streaming is session-only — NEVER persisted as a boot default** (`SetModeArgv`/`StartStreamPreset` never call `save_display_layout`; the daemon always boots from the saved layout). On stream start `xvfb.tick_rate` (or a GUI-set rate via `set_tick_rate`) is pushed; on any exit back to a layout (`SetMode` svg/html OR `SetLayout`) `restore_from_streaming` restores `display.tick_rate`. While streaming, the encoded JPEG is also written (via `block_in_place`) to `$XDG_RUNTIME_DIR/thermalwriter/last.jpg` for the GUI live preview, and removed on exit. `ResolveBinaries(names)` resolves binaries against the **daemon's** PATH to absolute paths (the daemon, not the GUI, execs them).
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
cargo test                               # run tests (~380 in main crate incl. integration; GUI tests via `cd gui/src-tauri && cargo test`)
cargo run --example preview_layout <name_or_path>  # render to PNG (no USB)
cargo run --example render_layout <name_or_path> [secs] [--mock]  # push to device
cargo run --example send_test_frame      # solid red hardware test
cargo run -- bench                       # USB throughput benchmark (~750 FPS)
systemctl --user status thermalwriter    # check daemon status
thermalwriter ctl status                 # query daemon via D-Bus
thermalwriter ctl mirror "command"       # xvfb capture mode (any X11 app, freeform sh -c)
thermalwriter ctl stream conky           # stream a built-in preset (conky/cava/btop) — session-only
thermalwriter setup-udev                 # one-shot: install udev rule for RAPL cpu_power access (re-execs under sudo)
cargo bench                              # criterion micro-benches, hot pipeline stages (NOT the `bench` subcommand above)
scripts/profile.sh --list                # scenario profiling harness (flamegraphs/dhat/RSS)
```

See `docs/profiling.md` for the full profiling harness + criterion workflow, and `docs/profiling-baselines.md` for the current committed baseline numbers.

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

Frontmatter var types (declared in the `{# vars: #}` block, surfaced as GUI controls): `color` (color picker), `text` (text field), `sensor` (sensor dropdown), `number` / `number(min,max,step)` (numeric field; renders as a slider in the GUI when min+max are given). Defaults are auto-injected into the Tera context, so `{{ var }}` always has a value even with no saved override. neon-dash-v2 exposes `panel_opacity: number(0,1,0.05)` driving the panel gradient's `stop-opacity` — the GUI-controllable knob for how much the global bg shows through the panels. Adding a new type means updating BOTH `frontmatter.rs` (parse/validate) and the GUI's `validate_vars` + App.svelte render branch; the daemon does NOT type-check vars (only the GUI does), but it must be rebuilt for its frontmatter parser to recognize a new type's default.

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
Wrappers in `~/.config/thermalwriter/wrappers/` — `conky-480.conf` + `cava-480.conf` starter configs seeded on first run (seed-if-missing, won't clobber edits). Tuned for the 480×480 LCD: conky `background=false` + opaque desktop window; cava `channels=mono` + `bars=20`/`bar_width=20`/`bar_spacing=4` (fills 480px), `-p` flag (NOT `--config`). `SDL_VIDEODRIVER=x11` is injected daemon-side, not in the config.

## Tauri Config GUI

Sub-project under `gui/` — Svelte 5 + Tauri 2. Talks to the daemon over D-Bus; falls back to direct `config.toml` writes if the daemon is offline.

- **Design system**: Tokyo Night Terminal HUD (see `gui/src/app.css`). Themes: Tokyo Night Storm (default), Tokyo Night, Catppuccin Mocha, Gruvbox Material, Nord — switched via `data-theme` on `<html>` and persisted in localStorage as `tw-theme`. Typography: Major Mono Display + IBM Plex Mono/Sans loaded from Google Fonts (CSP allows `fonts.googleapis.com`/`fonts.gstatic.com`).
- **Dev mode**: `cd gui && npm run tauri:dev`. The MCP bridge (`tauri-plugin-mcp-bridge`) is wired up debug-only — once running, Claude's Tauri MCP can screenshot/inspect/drive the webview on `localhost:9223`.
- **Live preview**: `render_preview` seeds the renderer with synthetic sensor history (mirroring `examples/preview_layout.rs`) whenever the layout's frontmatter declares history metrics — without it, layouts using `graph(data=…_history)` (e.g. neon-dash-v2) error out in Tera, since the GUI has no live daemon feeding real history.
- **Backgrounds**: GUI fetches thumbnails through the `read_background` Tauri command (returns raw bytes; frontend wraps in a Blob URL). BgGallery's "Import" tile lets users add a PNG/JPEG without touching the filesystem — the file's bytes go to the `import_background` command, which validates the extension + decodes via `render::background::decode_to_pixmap` (rejecting non-images and >8 MB files), then atomically writes a non-clobbering filename into `backgrounds/` and returns it so the gallery refreshes and selects it.
- **Color suggestion** ("◑ Suggest" button in the Variables tab, enabled when a background + color vars exist): `suggest_colors(layout, background)` extracts dominant hues from the background (`src/render/palette.rs`: Material You pipeline — Celebi Wu+k-means quantize → `Score` chroma/population ranking via the `material-colors` crate) and rebuilds each overlay role in HCT with fixed tone/chroma recipes (only *hue* comes from the image, so accents stay vivid on the washed-out LCD). Contrast floor: accent tone ≥ image **median** tone + 45 (median, NOT p95 — a dark wallpaper with one big bright feature like ronin-moon's moon must not push accents into pastel). Achromatic images fall back to Tokyo Night blue via the Score fallback param. Role→var mapping is name-convention-based in `assign_scheme_to_vars` (primary/cpu, secondary/gpu, accent/tertiary/fps, text, dim/muted/label, background/panel; unmatched color vars cycle the three accents). Suggestions merge into the GUI's live `values` (previewed, not persisted until Apply).
- **Stream tab** (`gui/src/lib/StreamTab.svelte` + declarative registry `gui/src/lib/streamPresets.ts`): a Variables|Stream tab nav surfaces app streaming. Presets (conky/cava/btop/nvtop/custom) grey out based on the **daemon's** `resolve_binaries` map (NOT the GUI's PATH — the daemon execs them); the argv is built with daemon-resolved absolute paths. Start → `apply_stream(argv)` then `set_tick_rate(fps)`; Stop → `stop_stream(layout)`. Live preview polls `read_frame` (raw JPEG bytes of `last.jpg`) at ~3 fps into an `<img>` with **`transform: rotate(180deg)`** — the tmpfs frame is post-rotation (as sent to the physically-flipped LCD), so the GUI un-rotates it for upright display. The poll pauses when the Stream tab is hidden (streaming + daemon stream stay alive). Terminal-wrapped presets (btop/nvtop) use per-terminal flags: alacritty/kitty `-o`, xterm `-fa/-fs`. The conky/custom Browse button uses `@tauri-apps/plugin-dialog` `open()` for full absolute paths (needs the `dialog:default` capability; do NOT add `"dialog": {}` to `tauri.conf.json` — tauri-plugin-dialog v2 takes no config and the empty map panics on boot). **Gotcha (Svelte 5):** a `$effect` that both reads (e.g. `...fieldValues`) and writes the same `$state` triggers `effect_update_depth_exceeded` and freezes ALL reactivity — read via `untrack(() => …)` to break the cycle. Unit tests + code review do NOT catch runtime issues like this or the dialog-config panic; launch the app (`npm run tauri:dev` + Tauri MCP) to verify.

## Key Dependencies

rusb, zbus, tiny-skia, resvg, taffy, tera, fontdue, image, memmap2, sysinfo, tokio, clap, dirs, wait-timeout, material-colors
