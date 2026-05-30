---
date: 2026-05-29
topic: gui-streaming-conky
---

# GUI App Streaming + Per-App Wrappers (Conky, Cava, btop)

## What We're Building

A new **Stream** tab in the Tauri/Svelte config GUI that surfaces the daemon's
existing Xvfb capture mode as a first-class feature. Users pick an app preset
(Conky, Cava, btop, nvtop) or a custom command, set a few options, and hit
**Start** — the app renders into a hidden 480×480 virtual display and streams to
the cooler LCD. A continuous live preview mirrors the device in the GUI canvas.

The daemon already does the hard part (generic X11 capture via `XvfbSource`,
`ModeChange::Xvfb`, `set_mode("xvfb", cmd)` over D-Bus). This work is concentrated
in: (1) the GUI Stream tab, (2) a declarative preset registry, and (3) two
small daemon additions — **stream persistence** and **last-frame readback**.

Scope is the hidden-Xvfb-sandbox surface only: no real-desktop-window mirroring,
no media/video playback.

## Why This Approach

**Chosen: Approach A — generic Stream tab with a declarative preset registry.**
Considered also: (B) conky-first minimal streaming, deferring other presets;
(C) a rich visual preset gallery with in-GUI config editing.

A wins because the daemon plumbing already exists, so all four apps are reachable
now for little more than B's conky-only scope, while the data-driven registry
makes "add an app later" a single row. C was rejected as over-engineered — the
user explicitly scoped wrapper depth to "launcher + ship a starter config," not
in-GUI config editing.

## Key Decisions

- **Reuse the daemon's existing xvfb mode** (`set_mode("xvfb", cmd)`, `dbus.rs:305`;
  `XvfbSource` in `render/xvfb.rs`; process lifecycle in `service/xvfb.rs`). No new
  capture engine.
- **GUI mirrors the existing layout `apply()` flow** (`App.svelte:165`): D-Bus
  primary, direct `config.toml` write as offline fallback.
- **Preset registry is declarative data** (GUI-side): per-app `id`, `label`,
  `binary`, `needs_terminal`, `command_template`, `fields`, `default_fps`.
  Adding nvtop / a new app = one row.
- **Persist streams** — `set_mode("xvfb")` must write `display.mode = "xvfb"` and
  `[xvfb].command` / `[xvfb].tick_rate` to `config.toml` (new `save_xvfb` helper
  using the existing atomic temp-file + `CONFIG_WRITE_LOCK` pattern). Today the
  xvfb branch persists nothing, so streams die on restart (gap vs.
  `save_display_layout`, `dbus.rs:258`). Config keys already exist (`config.rs:73`).
- **Stop streaming returns to the saved layout** AND persists `display.mode` back
  to `svg`/`html`, so a restart doesn't silently resume a stopped stream.
- **New D-Bus `get_last_frame() -> (bytes, w, h)`** returns the most recent
  JPEG the tick loop sent (no extra encode). GUI polls ~3–5fps and paints the
  480×480 canvas; frontend wraps bytes in a Blob URL (as BgGallery does).
- **Live preview toggle, uniform across modes.** OFF (default for layouts) =
  existing local `render_preview` with synthetic data, preserving the
  edit-before-apply flow. ON = poll `get_last_frame()` for real device output.
  Forced ON + locked for streaming; auto-disabled (with a note) when daemon
  offline. A "● LIVE" indicator shows which mode is on screen. **In scope now**
  (not deferred) since the readback is built regardless.
- **Cava audio = default-output monitor.** Seeded cava config uses
  `method = pulse`, `source = auto` → captures the default PipeWire sink monitor,
  visualizing any audio (incl. bitperfect playing through the default output).
  Optional freeform "audio source" field, default auto. No source enumeration in v1.
- **Terminal emulator = preference-ordered auto-detect**
  (`kitty → alacritty → ghostty → foot → xterm`), first installed wins,
  user-overridable. Resolved in the GUI and baked into the command string; daemon
  stays generic. Per-emulator invocation lookup with a 480×480-tuned font size.
- **Ship starter configs** for conky (`LCD Minimal`) and cava, seeded to
  `~/.config/thermalwriter/` on first run (existing seed-if-missing pattern),
  user-editable. Conky preset offers built-in vs. custom-path (native file picker).
- **Missing-binary UX:** grey out presets whose `binary` (or required terminal)
  isn't on PATH, with an install hint.

## External Prerequisites

- **Xvfb** — required for the capture feature itself. Present on dev machine. ✓
- **kitty / alacritty** — present on dev machine; both verified to render in a bare
  480×480 Xvfb. ✓ Other machines rely on the detection fallback.
- **btop** ✓, **cava** ✓ installed on dev machine; **nvtop** not installed (preset
  surfaces as "binary missing"). End users install the apps they want.
- **PipeWire/Pulse** — needed for cava's default-monitor capture. Standard on the
  target desktop.
- No credentials / network services required.

## Probed Assumptions

| Assumption | Tag | Probe | Result |
|-----------|-----|-------|--------|
| Daemon's xvfb mode is fully generic (any X11 command) and reachable via D-Bus `set_mode("xvfb", cmd)` | [verified] | Explore agent read of `dbus.rs:305`, `render/xvfb.rs`, `service/xvfb.rs` (2026-05-29) | confirmed |
| `set_mode("xvfb")` does NOT persist to config.toml (stream lost on restart) | [verified] | Read `dbus.rs:305-348` vs `dbus.rs:258` `save_display_layout` (2026-05-29) | confirmed — drove the persistence decision |
| `[xvfb]` config keys (`command`, `tick_rate`) already exist and are read on boot | [verified] | Read `config.rs:73-87`, `config.rs:162` (2026-05-29) | confirmed |
| No existing "current frame" readback path in daemon or GUI | [verified] | Explore agent map of D-Bus methods + Tauri commands (2026-05-29) | confirmed — needs new `get_last_frame` |
| kitty & alacritty render in a bare 480×480 Xvfb | [verified] | Spawned Xvfb :97 480x480x24, launched each via `-e sleep` (2026-05-29) | confirmed (both STARTED OK) |
| bitperfect plays through default output by default, so cava can read the default sink's PipeWire monitor | [verified] | Read `../bitperfect/src-tauri/src/audio.rs:66-68` (cpal default_output_device) (2026-05-29) | confirmed for default-output path |
| bitperfect exclusive `hw:` bit-perfect mode has no monitor for cava to tap | [verified] | Read `audio.rs:997` (bitperfect path), `lib.rs:235` (set_audio_device hw:) (2026-05-29) | confirmed — explicitly deferred |

## Open Questions

- None blocking. Deferred (out of scope): cava capture of bitperfect's exclusive
  `hw:` bit-perfect stream (would require the player to tee to a FIFO — a
  bitperfect-side change). nvtop preset ships but is untested until installed.
- ghostty is listed in the terminal preference order but unverified in headless
  Xvfb (GPU/OpenGL → software-GL risk); kitty/alacritty are the proven defaults.

## Next Steps

→ writing-plans skill for implementation details
