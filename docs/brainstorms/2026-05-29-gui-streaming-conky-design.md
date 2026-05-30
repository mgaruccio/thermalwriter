---
date: 2026-05-29
topic: gui-streaming-conky
---

# GUI App Streaming + Per-App Wrappers (Conky, Cava, btop)

## Refinement Summary

**Refined on:** 2026-05-29
**Research agents used:** 3 (daemon xvfb lifecycle, GUI integration, app-config best-practices)
**Review agents used:** 1 architecture-strategist + adversarial Gemini (gemini-3.1-pro-preview)
**Adversarial review:** Completed

### Key Improvements
1. **Persistence reversed to session-only** (was: persist + auto-resume). Auto-resuming an
   arbitrary saved command on boot could brick the LCD to a permanent black screen with no
   GUI recovery. Streams are now runtime-only; the daemon always boots into the saved layout.
2. **Frame readback moved off D-Bus onto a tmpfs file.** Polling 50–200 KB JPEGs at 3–5 Hz
   through the session-bus broker would contend with the control channel the codebase works
   hard to keep responsive. Now: daemon writes `$XDG_RUNTIME_DIR/thermalwriter/last.jpg`,
   GUI reads it like it already reads backgrounds.
3. **Live device-mirror scoped to streaming only.** Layouts keep the local `render_preview`
   (shows unsaved edits); avoids an edit-vs-applied dual-source-of-truth.
4. **Robust child-process cleanup** (Gemini CRITICAL). Current `sh -c` children aren't put in
   their own process group, and conky/kitty fork or daemonize — orphan risk. Launch via
   `systemd-run --user --scope` (cgroup kills all descendants) or `setsid`+killpg.
5. **Atomic Xvfb display acquisition** (Gemini MAJOR). Replace the TOCTOU lockfile scan with
   `Xvfb -displayfd`.
6. **Structured argv for presets, `sh -c` only for custom commands** (Gemini MAJOR). Prevents
   file paths with spaces/metacharacters from breaking preset launches.
7. **Terminal/binary detection must reflect the daemon's systemd PATH**, not the GUI's
   interactive PATH (the daemon execs the command).

### Escalations
- None. All adversarial findings were verified against source and Agreed (no disputes).

---

## What We're Building

A new **Stream** tab in the Tauri/Svelte config GUI that surfaces the daemon's existing
Xvfb capture mode as a first-class feature. Users pick an app preset (Conky, Cava, btop,
nvtop) or a custom command, set a few options, and hit **Start** — the app renders into a
hidden 480×480 virtual display and streams to the cooler LCD. A continuous live preview
mirrors the device in the GUI canvas (streaming mode only).

The daemon already does the hard part (generic X11 capture via `XvfbSource`,
`ModeChange::Xvfb`, `set_mode("xvfb", cmd)` over D-Bus). This work is concentrated in:
(1) the GUI Stream tab, (2) a declarative preset registry, and (3) daemon additions —
**session-only mode transitions**, **tmpfs last-frame readback**, **robust child cleanup**,
and **atomic display acquisition**.

Scope is the hidden-Xvfb-sandbox surface only: no real-desktop-window mirroring, no
media/video playback.

## Why This Approach

**Chosen: Approach A — generic Stream tab with a declarative preset registry.**
Considered also: (B) conky-first minimal streaming, deferring other presets;
(C) a rich visual preset gallery with in-GUI config editing.

A wins because the daemon plumbing already exists, so all four apps are reachable now for
little more than B's conky-only scope, while the data-driven registry makes "add an app
later" a single row. C was rejected as over-engineered — wrapper depth was scoped to
"launcher + ship a starter config," not in-GUI config editing.

## Key Decisions

- **Reuse the daemon's existing xvfb mode** (`set_mode("xvfb", cmd)`, `dbus.rs:305`;
  `XvfbSource` in `render/xvfb.rs`; lifecycle in `service/xvfb.rs`). No new capture engine.
- **GUI mirrors the existing layout `apply()` flow** (`App.svelte:165`): D-Bus primary, with
  the offline-fallback caveat below. New `apply_stream` Tauri command follows `apply_to_daemon`.
- **Preset registry is declarative data** (GUI-side): per-app `id`, `label`, `binary`,
  `needs_terminal`, `args` (a structured argv template — see "Command construction"),
  `fields`, `default_fps`. Adding nvtop / a new app = one row. Presets whose `binary` (or
  required terminal) the **daemon** can't resolve are greyed out with an install hint.

- **Persistence = SESSION-ONLY** *(refined — was persist+auto-resume)*. Streaming is a
  runtime mode, not a saved boot default. The daemon always boots into the saved
  SVG/HTML layout; `display.mode` is never written as `"xvfb"`. Restarting the daemon while
  streaming returns to the layout — no auto-resume of an arbitrary subprocess. The GUI
  remembers the user's last stream choice in its own local settings so re-applying is one
  click. This eliminates the black-screen-brick class entirely. (Rationale: a broken saved
  command — missing binary, deleted config — would make the device boot black every login
  with no GUI recovery, since the liveness probe reads config.toml and would still report
  "online".)

- **Stop streaming returns to the saved layout** via `set_mode("svg"|"html", layout_name)`.

- **Mode transitions are atomic and consistent** *(refined — Gemini MAJOR / arch M2)*. A
  `mode_change_lock` (analogous to the existing `bg_change_lock`, `dbus.rs:491`) wraps the
  whole start/stop body — channel-send + state-mirror — so two GUI windows can't interleave
  a start and stop into an inconsistent state. On the return path, `set_mode` must also
  update `state.active_layout` (currently left stale) and push the correct tick rate into
  the tick loop's `tick_rate` channel (`xvfb.tick_rate` on start, `display.tick_rate` on
  stop; `tick.rs:94`).

- **Live preview via tmpfs file, STREAMING-ONLY** *(refined — was D-Bus get_last_frame,
  uniform)*. While in xvfb mode, the tick loop writes the latest JPEG to
  `$XDG_RUNTIME_DIR/thermalwriter/last.jpg` (atomic write + rename). The GUI reads it via a
  new `read_frame` Tauri command (like `read_background`, `commands.rs:288`) ~3 fps and
  paints the canvas. Image bytes never touch the D-Bus control channel. **On exit from xvfb
  mode the daemon truncates/removes `last.jpg`**, and the GUI gates the preview on the live
  `mode` from `get_status` (`dbus.rs:351`) so a stale frame is never shown as live *(Gemini
  MINOR #5)*. For SVG/HTML layouts the GUI keeps the existing local `render_preview`
  (`commands.rs:165`) — the device-mirror is not used for layouts.

- **Robust child-process cleanup** *(refined — Gemini CRITICAL #1)*. Today the child is
  `sh -c "<cmd>"` spawned with no new process group (`xvfb.rs:93`), so `killpg(child_pid)`
  (`xvfb.rs:36`) can miss it, and apps that fork/daemonize (conky with `background yes`,
  kitty's single-instance client/server) orphan. Two-part fix: (a) launch each stream child
  inside a transient `systemd-run --user --scope` unit so stopping the scope cgroup-kills all
  descendants regardless of forking; fall back to `setsid` + process-group kill if
  `systemd-run` is unavailable. (b) Force foreground in seeded configs (conky
  `background = false`) and avoid kitty's single-instance mode (`kitty --single-instance=no`
  or prefer alacritty for TUIs).

- **Atomic Xvfb display acquisition** *(refined — Gemini MAJOR #2)*. Replace the
  `find_unused_display` lockfile scan (`xvfb.rs:65`, TOCTOU-racy) with `Xvfb -displayfd <fd>`,
  which picks a free display atomically and reports it back.

- **Command construction: structured argv for presets, `sh -c` only for custom**
  *(refined — Gemini MAJOR #4)*. Preset commands are built as an argument vector and exec'd
  directly (no shell), so a conky config path containing spaces or shell metacharacters can't
  break parsing or be interpreted. The freeform **Custom** command keeps `sh -c` (power-user,
  intentional). This needs the daemon's `set_mode`/launch path to accept an argv form for
  presets (today it takes one shell string).

- **Cava audio = default-output monitor**. Seeded cava config uses `method = pulse`,
  `source = auto` → the default sink's PipeWire monitor (PulseAudio compat layer; note the
  known `method = pipewire` + `auto` bug that grabs the mic). cava uses its **native SDL
  output** (its own X11 window on llvmpipe software GL), not a terminal. Visualizes whatever
  plays on the default output (incl. bitperfect via the default sink). Exclusive `hw:`
  bit-perfect playback has no monitor → out of scope. **Audio access caveat** *(Gemini MINOR
  #6 / arch H4):* the daemon is a systemd user service; it needs the graphical session's
  `XDG_RUNTIME_DIR`/PipeWire socket. Detect "no audio connection" and surface a helpful
  message instead of a silent flatline; **probe cava-from-daemon for real before calling this
  verified** (the earlier probe checked an unrelated app).

- **Terminal emulator (for TUI apps btop/nvtop)** = preference-ordered detection
  **kitty → alacritty → xterm**, user-overridable (foot excluded: Wayland-only; ghostty
  untested → opt-in). kitty/alacritty both verified to render in a bare 480×480 Xvfb on
  llvmpipe. **Detection must reflect the DAEMON's environment** *(refined — Gemini MAJOR #3-
  adjacent / arch H3)*: the daemon (systemd user service, minimal PATH) execs the command, so
  the GUI must not detect against its own richer interactive PATH. Plan: the daemon exposes a
  `resolve`/`which` capability over D-Bus (or resolves absolute paths itself) and the GUI
  queries that. Launch: `alacritty -o font.size=12 -e btop`, `kitty -o font_size=12 -e btop`;
  btop needs `shown_boxes = "cpu"` + font ~12 to be legible at 480×480.

- **Ship starter configs** for conky (`LCD Minimal`: `own_window_type = desktop` since no WM
  in Xvfb, `double_buffer = true`, pinned 480×480, opaque Tokyo-Night bg, `background = false`)
  and cava, seeded to `~/.config/thermalwriter/` on first run (existing seed-if-missing). Keep
  the seed count minimal and pin/test against stated app versions (conky 1.22, cava current);
  a broken seeded config = black-screen bug, so it ties to the health note above. Conky preset
  offers built-in vs. custom-path (native file picker via `tauri-plugin-dialog`, to be added).

- **Security posture** *(arch M1)*: xvfb mode is intentionally an arbitrary-exec interface
  scoped to the **session-bus** trust boundary (anything that can call the method already runs
  as the user). So no path sandboxing is added for conky configs — it buys nothing. The
  structured-argv decision above is about *correctness/robustness*, not privilege. The service
  is session-bus only (`dbus.rs:651` `Builder::session()`); a system-bus variant would make
  this CRITICAL.

- **Fragile offline detection** *(Gemini MAJOR #3)*: the GUI detects daemon-offline by string-
  matching `"daemon is not running"` (`App.svelte:180`). A USB-stall-induced D-Bus *timeout*
  (a documented daemon risk) won't match and would mislead the user. Inspect the structured
  D-Bus error name (e.g. `org.freedesktop.DBus.Error.ServiceUnknown` / `NoReply`) instead.
  Pre-existing, but this feature relies on it — fix as part of the work.

## Edge Cases Handled

- **Daemon offline:** Stream tab shows an offline state and cannot preview (no daemon to
  produce frames). Applying a stream offline is disallowed (a stream is inherently runtime).
- **GUI opened while a stream is already running** (started by a prior GUI instance): GUI reads
  current `mode` from `get_status` on mount and reflects it, rather than assuming it started
  the stream.
- **Xvfb binary not installed:** `start()` bails (`xvfb.rs:90`); grey out the *entire* Stream
  tab, distinct from a per-preset binary-missing hint.
- **Stale tmpfs frame:** removed on xvfb exit + GUI gates on live mode (see live-preview
  decision).
- **Resolution fit:** TUI apps (btop/nvtop) won't fill 480×480 exactly; font sizing is per-app
  and will need iteration (budgeted). conky/cava can be told their geometry; TUIs are worse fits.
- **Display-number / lockfile leak:** mitigated by `-displayfd`; a crash that skips `Drop` can
  still leave a stale fbdir — low risk, worth a cleanup-on-start sweep.

## External Prerequisites

- **Xvfb** — required for the feature itself. Present on dev machine. ✓
- **kitty / alacritty** — present on dev machine; both verified to render in a bare 480×480
  Xvfb (llvmpipe software GL). ✓ Other machines rely on detection fallback.
- **btop** ✓, **cava** ✓ on dev machine; **nvtop** not installed (preset surfaces as
  "binary missing"). btop has GPU support and may cover the NVIDIA case.
- **systemd (user)** — for `systemd-run --user --scope` child supervision. Present (daemon is
  already a user service). Fallback to `setsid` if absent.
- **PipeWire/Pulse** — for cava's default-monitor capture; daemon needs the session's
  `XDG_RUNTIME_DIR`.
- **tauri-plugin-dialog** — to add for the conky config file picker.
- No credentials / network services required.

## Probed Assumptions

| Assumption | Tag | Probe | Result |
|-----------|-----|-------|--------|
| Daemon's xvfb mode is fully generic and reachable via `set_mode("xvfb", cmd)` | [verified] | Read `dbus.rs:305`, `render/xvfb.rs`, `service/xvfb.rs` (2026-05-29) | confirmed |
| `set_mode("xvfb")` does NOT persist to config.toml | [verified] | Read `dbus.rs:305-348` vs `dbus.rs:258` (2026-05-29) | confirmed — now moot (session-only) |
| Tick loop holds the JPEG only as a local binding (no shared buffer) | [verified] | Read `tick.rs:172-178` (2026-05-29) | confirmed — drove tmpfs-write decision |
| `sh -c` child is spawned WITHOUT its own process group → killpg can miss forked/daemonized children | [verified] | Read `xvfb.rs:36, 93-97` (2026-05-29) | confirmed — drove cgroup/setsid fix |
| `find_unused_display` scans lockfiles (TOCTOU race) | [verified] | Read `xvfb.rs:65-73` (2026-05-29) | confirmed — drove `-displayfd` fix |
| kitty & alacritty render in a bare 480×480 Xvfb on llvmpipe | [verified] | Spawned Xvfb :97 480x480x24, launched each (2026-05-29) | confirmed |
| `source = auto` (method=pulse) resolves to the default sink monitor | [verified] | cava upstream README + `pactl list sources` on dev box (2026-05-29) | confirmed for default-output path |
| GUI detects offline via string-match `"daemon is not running"` | [verified] | Read `App.svelte:180` (2026-05-29) | confirmed — replace with structured error |
| Daemon (systemd user service) can reach PipeWire for cava | [assumed] | NOT yet probed from the daemon's own env | **deferred — must probe before build** |
| Daemon's systemd PATH differs from GUI's interactive PATH | [assumed] | reasoned from systemd user-unit defaults | deferred — verify, drives detection design |

## Open Questions

- **Probe cava from the daemon's actual environment** before implementing the cava preset —
  confirm it reaches the default-sink monitor as a systemd user service.
- **Confirm `systemd-run --user --scope`** cleanly supervises + tears down a forking child
  (conky `background yes`, kitty single-instance) in this setup; settle the `setsid` fallback.
- Verify the daemon-vs-GUI PATH gap and finalize the `resolve`/`which` D-Bus surface.
- Deferred (out of scope): cava capture of bitperfect's exclusive `hw:` stream (needs a
  player-side FIFO); nvtop preset ships untested until installed; ghostty terminal support.

## Next Steps

→ writing-plans skill for implementation details
