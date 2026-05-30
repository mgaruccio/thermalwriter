# GUI App Streaming + Per-App Wrappers (Conky/Cava/btop) — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use forge:executing-plans to implement this plan task-by-task.

**Goal:** Add a GUI "Stream" tab that surfaces the daemon's existing Xvfb capture mode with per-app presets (conky, cava, btop, nvtop, custom), a live preview, and hardened daemon mode-transition logic.

**Architecture:** The daemon already streams arbitrary X11 apps captured from a hidden Xvfb framebuffer (`set_mode("xvfb", cmd)`). This work (1) hardens the daemon's mode-transition correctness (atomic confirm, defer-drop, atomic display alloc), (2) adds session-only streaming with a tmpfs live-frame readback, a daemon-side binary `resolve` method, and structured-argv launches, (3) seeds 480×480 starter configs, and (4) builds the GUI Stream tab + declarative preset registry. Streaming is runtime-only — never persisted as a boot default.

**Tech Stack:** Rust (tokio, zbus, rusb, resvg), Tauri 2, Svelte 5 ($state runes), Xvfb, conky/cava/btop, kitty/alacritty/xterm.

**Required Skills:**
- `forge:writing-tests`: Invoke before Tasks 1, 2, 3, 5, 6 — TDD for the daemon correctness fixes; covers unit/integration test design for async + process logic.
- `forge:verification-before-completion`: Invoke before every Milestone — run the actual commands, read output, before claiming done.
- `forge:expectation-driven-development`: Invoke before Task 0 — pre-register expected outcomes for the two verification spikes.
- `serial_test` (crate, not a skill): tests that mutate env/cwd/process state use `#[serial]` (see memory `feedback_serial_test_for_env_mutation`).

## Context for Executor

### Key Files (daemon)
- `src/service/xvfb.rs` — Xvfb + child lifecycle. `kill_process_group` (`:35`), `XvfbHandle` Drop (`:55-73`), `find_unused_display` (`:76-84`, TOCTOU lockfile scan — to replace), `start(command,w,h)` (`:90-165`), child spawned via `sh -c` with `.process_group(0)` (`:148-155`). `XvfbHandle::screen_file()` returns the XWD path.
- `src/service/dbus.rs` — `ModeChange` enum (`:19-30`: `Layout{name,vars}`, `Xvfb{command}`, `Background{image}`), `ServiceState` (`:32-46`: `mode_change_tx: mpsc::Sender<ModeChange>`, `active_layout`, `mode`, `tick_rate`, `bg_change_lock: Arc<Mutex<()>>`, `config`), `set_mode` impl (`:305-348` — currently sets `state.mode` immediately after channel send, no confirm), `set_background` end-to-end-locked pattern (`:473-525`), `get_status` (`:351`, returns `mode`), interface bound to **session** bus (`:651` `Builder::session()`).
- `src/main.rs` — mode-change listener task (`:242-351`): holds `xvfb_handle` (`:244`), `ModeChange::Layout` drops handle at `:252-253` BEFORE fallible layout read at `:256` (warns at `:312`), `ModeChange::Xvfb` at `:325-347` drops handle then `xvfb_manager::start` (fails → `warn!` at `:346`). Channels: `source_tx` (`:117`), `tick_rate_tx: watch::Sender<u32>` (`:126`), `mode_tx: mpsc::Sender<ModeChange>` size 4 (`:128`). Boot xvfb branch (`:136-148`).
- `src/service/tick.rs` — `run_tick_loop`. Render → `encode_jpeg` (`~:172`) → `tokio::task::block_in_place(|| transport.send_frame(&jpeg))` (`~:181`). The `jpeg: Vec<u8>` is a local binding, not stored.
- `src/config.rs` — `DisplayConfig.mode` (`:38`), `XvfbConfig{command, tick_rate}` (`:73-87`, tick_rate validated 1-60 at `:162`), `CONFIG_WRITE_LOCK` (`:23`), atomic helpers `save_layout_vars` (`:186`), `save_display_layout` (`:261`), `save_background_image` (`:321`) — all temp-file + `fsync` + rename. Seeded built-ins via `include_str!` (`:388-451`, seed-if-missing).
- `src/dbus_types.rs` — `Display` zbus proxy (`:15-35`): `set_mode(mode,command)` (`:23`), `get_status`, `list_sensors`, `set_default_layout`, etc.
- `src/cli.rs` — `CtlCommand::Mirror{command}` (`:60-63`) → `set_mode("xvfb",cmd)` (`:126-132`).

### Key Files (GUI)
- `gui/src-tauri/src/commands.rs` — `#[tauri::command]` fns. D-Bus pattern: `zbus::Connection::session()` + `DisplayProxy::new` (`:252-262`). `apply_to_daemon` (`:241-276`), `save_config` fallback (`:216-238`), `render_preview` → RGBA bytes (`:165-210`), `read_background` raw bytes (`:288-296`), `import_background` (`:359-366`, validate+atomic write). Path validation `validate_layout_path`/`validate_background_path` (canonicalize + starts_with, `:513-565`).
- `gui/src-tauri/src/lib.rs` — `invoke_handler![generate_handler!(...)]` command registry (~`:44-58`); new commands must be added here.
- `gui/src-tauri/src/error.rs` — `AppError::DaemonUnavailable` message template (`:30-32`); GUI matches its string.
- `gui/src/App.svelte` — single-page grid `main-grid` (`:258`), panels `sidebar`/`preview-pane`/`config-pane` (`:260/:325/:358`). `$state` block (`:44-61`), `onMount` (`:72-96`), `apply()` D-Bus-then-fallback (`:165-199`, offline detection via string-match `"daemon is not running"` at `:180`), `render_preview` canvas paint (`:145-162`), `kindClass()` already returns `"kind-xvfb"` (unused, `:201-205`).
- `gui/src/lib/BgGallery.svelte` — component pattern (`$props`, lazy thumbnails via Blob URL `:59-72`); model `StreamTab.svelte` on this.
- `gui/src-tauri/Cargo.toml` — deps: `tauri 2`, `zbus`, `tokio`, `serde`, `tauri-plugin-mcp-bridge`. **No `tauri-plugin-dialog`** (add for file picker).
- `gui/src-tauri/tauri.conf.json` — no `plugins` section yet; CSP allows google fonts.

### Research Findings (verified 2026-05-29)
- **Software GL works in bare Xvfb** (llvmpipe/Mesa): kitty + alacritty both render headless at 480×480 (process-liveness verified). **foot is Wayland-only — exclude.** ghostty untested → opt-in only.
- **cava**: `method = pulse` + `source = auto` reliably resolves the **default sink's monitor** (what's on speakers) on PipeWire-via-Pulse; `method = pipewire` + auto has a known mic-grab bug. cava's **SDL output opens its own X11 window** (no terminal needed); in a WM-less Xvfb verify `sdl_width/sdl_height = 480` fills the root at (0,0) — else run `matchbox-window-manager` or `xdotool` resize.
- **conky** in WM-less Xvfb: `own_window_type = 'desktop'`, `own_window = true`, `double_buffer = true`, `minimum_width/height = 480`, `gap_x/y = 0`, `alignment = 'top_left'`, opaque `own_window_colour`, `background = false` (foreground so daemon can SIGTERM). hwmon temp indices are machine-specific.
- **btop/nvtop** are TUIs needing a terminal. Legible at 480×480 with `font ~12` + `shown_boxes = "cpu"`. Per-terminal launch: `alacritty -o font.size=12 -e btop`, `kitty -o font_size=12 -e btop`, `xterm -fa 'DejaVu Sans Mono' -fs 12 -e btop` (**xterm rejects `-o`**). btop built with GPU support may cover NVIDIA without nvtop.
- **Daemon env**: runs as a systemd **user** service → minimal PATH, distinct from the GUI's interactive PATH. Binary/terminal detection MUST reflect the daemon's env (it execs the command), so detection goes through a daemon-side `resolve` D-Bus method, not GUI-local PATH probing.
- **On-box availability**: Xvfb ✓, kitty ✓, alacritty ✓, btop ✓, cava ✓, nvtop ✗, xterm ✗, foot ✗, ghostty ✗.

### Relevant Patterns
- End-to-end lock for a multi-step state change: `set_background` holds `bg_change_lock` across decode→disk→channel→state (`dbus.rs:473-525`). Mirror this for mode transitions (`mode_change_lock`).
- Atomic file write: temp + `fsync` + rename under `CONFIG_WRITE_LOCK` (`config.rs:186-258`). Reuse for the tmpfs frame write (without the config lock — different file).
- Blocking I/O inside the async tick loop: wrap in `tokio::task::block_in_place` (`tick.rs:~181`).
- Tauri command + D-Bus proxy round-trip: `commands.rs:241-276`.

## Execution Architecture

**Team:** 2 devs, 1 spec reviewer, 1 quality reviewer (≤10 impl tasks).
**Task dependencies:**
- Task 0 (spikes) gates Tasks 6 (cava preset) and 8 (detection) — run first.
- Tasks 1, 2, 3 (daemon correctness) are largely independent of each other but ALL must land before Task 4/5 (they touch the same listener/`xvfb.rs`); sequence 1→2→3 to avoid merge churn on `main.rs`.
- Task 5 (tmpfs readback) is independent of Tasks 6-8; can parallel.
- Phase 4 GUI tasks (9-11) depend on Phase 2 daemon surface (Tasks 4,5,8) being merged.
**Phases:**
- Phase 0: Task 0 (verification spikes)
- Phase 1: Tasks 1-3 + review + milestone (daemon correctness hardening)
- Phase 2: Tasks 4-8 + reviews + milestone (daemon streaming surface + configs)
- Phase 3: Tasks 9-11 + review + milestone (GUI)
**Milestones:** after Phase 1, after Phase 2, after Phase 3 (final).

---

## Phase 0 — Verification Spikes

### Task 0: Verify cava-from-daemon audio + daemon PATH gap [READ-DO]

> Invoke `forge:expectation-driven-development` first. Pre-register: (a) cava launched *by the daemon* shows moving bars with audio playing; (b) `systemctl --user show-environment` PATH differs from interactive `echo $PATH`.

**Files:** none modified — this is a spike. Record findings in `docs/plans/2026-05-29-gui-streaming-conky.md` under a new "Spike Results" appendix.

**Step 1:** Capture the daemon's PATH: `systemctl --user show-environment | grep -i path` and compare to interactive `echo $PATH`. Note any dir (e.g. `~/.local/bin`, `/opt`) present in one but not the other.

**Step 2:** With music playing on the default output, start a cava stream through the daemon's own path:
`thermalwriter ctl mirror "cava"` (daemon execs it). Wait 3s. `thermalwriter ctl status` should show `mode=xvfb`. Pull a frame (once Task 5 exists) OR temporarily `cargo run --example render_layout` — for the spike, simplest is to confirm cava's process is alive under the daemon and reads audio: check `pactl list source-outputs` shows a cava client on the sink monitor.

**Step 3:** Record in the appendix: PATH delta (drives Task 8 design), whether cava-from-daemon captured audio (confirms/redesigns Task 6), and whether cava's SDL window filled 480×480 or showed X stipple (drives the WM decision in Task 6).

**Step 4:** Restore: `thermalwriter ctl layout svg/neon-dash-v2.svg`.

**Exit:** Both `[assumed]` rows in the design doc's Probed Assumptions become `[verified]` or trigger a design note. **If cava-from-daemon cannot reach audio**, escalate to user before building Task 6.

### Task 0b: Milestone — spike results
**Present to user:** PATH delta, cava audio result, cava window-fill result, and any design adjustments. **Wait for user response** before Phase 1.

---

## Phase 1 — Daemon Correctness Hardening

### Task 1: Atomic mode-transition with synchronous confirmation [READ-DO]

> Invoke `forge:writing-tests` first. Gemini MAJOR #1: `set_mode` currently mutates `state.mode`/tick-rate before the async listener confirms the swap; a failed Xvfb start leaves the daemon reporting "streaming" while rendering the old layout.

**Files:**
- Modify: `src/service/dbus.rs` (`ModeChange` enum `:19-30`, `set_mode` `:305-348`)
- Modify: `src/main.rs` (listener `:250-348`)
- Test: `src/service/dbus.rs` `#[cfg(test)]` module (or `tests/mode_transition.rs`)

**Step 1: Write the failing test.** Drive a `ModeChange` through a stub listener that returns `Err`; assert the helper that performs the transition returns `Err` and does NOT mutate a passed-in state mirror. (Test the confirm-channel contract, not the whole daemon.)
```rust
#[tokio::test]
async fn failed_transition_leaves_state_unchanged() {
    // listener stub replies Err; transition fn must propagate Err and not set mode
}
```

**Step 2: Run it — expect FAIL** (method/field not present).

**Step 3: Implement.** Add a reply channel to every `ModeChange` variant:
```rust
pub enum ModeChange {
    Layout { name: String, vars: HashMap<String,String>, ack: oneshot::Sender<anyhow::Result<()>> },
    Xvfb  { command: String, ack: oneshot::Sender<anyhow::Result<()>> },
    Background { image: Option<PathBuf>, ack: oneshot::Sender<anyhow::Result<()>> },
}
```
In `set_mode`: build the variant with `let (ack_tx, ack_rx) = oneshot::channel();`, send it, `ack_rx.await`. Only on `Ok` set `state.mode`/`active_layout`/tick-rate; on `Err` return `zbus::fdo::Error::Failed`. In `main.rs`, each arm sends `ack.send(Ok(()))` after the new source is confirmed sent, or `ack.send(Err(e))` on failure (replace the bare `warn!` at `:346` and `:312`).

**Step 4: Run tests — expect PASS.** Also `cargo build` and `cargo test`.

**Step 5: Manual check.** `thermalwriter ctl mirror "this-binary-does-not-exist"` must return a D-Bus error and leave `ctl status` showing the *previous* mode (not `xvfb`).

**Step 6: Commit.** `feat(daemon): confirm mode transitions before mutating state`

### Task 2: Defer handle-drop until replacement source confirmed [READ-DO]

> Gemini MAJOR #2: `main.rs:252-253` drops `xvfb_handle` before the fallible layout read; a failed read freezes the dead Xvfb mmap forever.

**Files:** Modify `src/main.rs` (`ModeChange::Layout` `:250-313`, `ModeChange::Xvfb` `:325-347`). Test: `tests/` integration or a unit on the ordering helper.

**Step 1: Write failing test** — simulate a `Layout` transition whose source-build fails; assert the previous `xvfb_handle` is still `Some` (not dropped). Extract the transition body into a testable fn returning `Result<Option<NewSource>>` if needed.

**Step 2: Run — expect FAIL.**

**Step 3: Implement.** Reorder both arms: build the new `FrameSource` and `source_tx.send(...).await` it FIRST; only after success do `if let Some(h) = xvfb_handle.take() { drop(h); }`. On build/read failure, leave the old handle in place, `ack.send(Err(..))`, and `continue` (the working stream keeps running). Applies to Layout→*, Xvfb→Xvfb, Xvfb→Layout.

**Step 4: Run tests — expect PASS;** `cargo test`.

**Step 5: Manual.** Start a stream, then `thermalwriter ctl layout does/not/exist.svg` → must error and the stream must keep rendering (not freeze).

**Step 6: Commit.** `fix(daemon): drop xvfb handle only after replacement source confirmed`

### Task 3: Atomic Xvfb display acquisition via -displayfd [READ-DO]

> Replaces the TOCTOU `find_unused_display` lockfile scan (`xvfb.rs:76-84`).

**Files:** Modify `src/service/xvfb.rs` (`start` `:90-165`, delete/replace `find_unused_display`). Test: `xvfb.rs` `#[cfg(test)]` (use `#[serial]` — touches process/display state).

**Step 1: Write failing test** — `start("true",480,480)` returns a handle whose `display_num` is a real, running display; two sequential `start` calls get distinct display numbers. (Gate behind a `cfg!`/env check if Xvfb absent in CI.)

**Step 2: Run — expect FAIL.**

**Step 3: Implement.** Launch `Xvfb -displayfd <writeendfd> -screen 0 {w}x{h}x24 -fbdir {fbdir} -ac -nolisten tcp` with a pipe; read the chosen display number from the fd, build the `:N` display string from it. Keep `.process_group(0)`, the screen-file wait, and the temp-fbdir naming (use the returned N). Remove `find_unused_display`.

**Step 4: Run tests — expect PASS;** `cargo test`.

**Step 5: Commit.** `refactor(xvfb): allocate display atomically via -displayfd`

### Task 4: Review Tasks 1-3

**Trigger:** both reviewers start when Task 3 completes.

**Killer items (blocking):**
- [ ] `set_mode` (`dbus.rs`) awaits the `ack` oneshot and does NOT set `state.mode`/`active_layout`/tick-rate on `Err` — verify with `ctl mirror <bad-binary>` leaving status unchanged.
- [ ] Every `ModeChange` arm in `main.rs` sends exactly one `ack` (Ok or Err) on all paths — no path drops the sender silently (would hang `set_mode`).
- [ ] `xvfb_handle` is dropped only AFTER `source_tx.send` succeeds, in all three transition arms (`main.rs`).
- [ ] A failed layout read while streaming leaves the old stream rendering (manual test), not a frozen frame.
- [ ] `-displayfd` path returns the actual display; `find_unused_display` is gone (no dead code).
- [ ] Display/process tests use `#[serial]`; `cargo test` passes.
- [ ] No `warn!`-and-continue swallows a transition failure that the caller needed to see.

**Quality items (non-blocking):**
- [ ] Transition body extracted into a testable fn rather than inline in the `match`.
- [ ] `oneshot` import + enum docs updated.
- [ ] Error messages name the failing command/layout.

**Resolution:** killer findings block the milestone.

### Task 5-milestone: Milestone — daemon correctness hardened
**Present to user:** the three fixes, test results, the two manual checks (bad-binary, frozen-frame). **Wait for user response.**

---

## Phase 2 — Daemon Streaming Surface

### Task 5: tmpfs last-frame readback (streaming-only, block_in_place) [READ-DO]

> Gemini MINOR #3: high-fps sync write must not stall the executor.

**Files:** Modify `src/service/tick.rs` (after `encode_jpeg` `~:172`), thread an `is_xvfb: bool` / mode signal into the loop. Add helper `src/service/frame_dump.rs` (or inline). Test: unit on the atomic-write helper.

**Step 1: Write failing test** — `write_frame_atomic(dir, &bytes)` creates `dir/last.jpg` with exact bytes via temp+rename; concurrent calls never yield a torn file.

**Step 2: Run — expect FAIL.**

**Step 3: Implement.** When the active mode is xvfb, after encoding, `tokio::task::block_in_place(|| write_frame_atomic(runtime_dir, &jpeg))` where `runtime_dir = $XDG_RUNTIME_DIR/thermalwriter` (create once). On exit from xvfb mode, remove/truncate `last.jpg`. Do NOT write for svg/html modes.

**Step 4: Run tests — expect PASS.**

**Step 5: Commit.** `feat(daemon): write last xvfb frame to tmpfs for GUI preview`

### Task 6: Session-only mode transition + tick-rate + lock [DO-CONFIRM]

**Files:** Modify `src/service/dbus.rs` (`set_mode`, add `mode_change_lock` to `ServiceState`), `src/main.rs` (tick-rate push). **Coordination required:** confirm with the Task 1 dev the final `ack`/oneshot contract before building on it.

**Implement:** Wrap the whole `set_mode` start/stop body in a new `mode_change_lock` (mirror `bg_change_lock`, `dbus.rs:473-525`). On stream start push `xvfb.tick_rate` into `tick_rate_tx`; on stop push `display.tick_rate` and set `state.active_layout` to the restored layout. **Never** call `save_display_layout` with `mode="xvfb"` — streaming is session-only (boot always loads the saved layout).

**Confirm checklist:**
- [ ] Failing test written FIRST (concurrent start+stop from two callers leaves a consistent mode).
- [ ] `mode_change_lock` held across channel-send + state-mirror (not just the disk write).
- [ ] `display.mode` is never persisted as `"xvfb"` (grep config writes).
- [ ] tick-rate restored to `display.tick_rate` on stop; set to `xvfb.tick_rate` on start.
- [ ] `state.active_layout` updated on the return-to-layout path (no stale `get_status`).
- [ ] Test asserts on resulting `mode`+`active_layout`, not just "no panic".
- [ ] Committed.

### Task 7: argv-based xvfb launch for presets [DO-CONFIRM]

> Gemini MINOR #5: presets exec a structured argv (no shell); custom commands keep `sh -c`.

**Files:** Modify `src/service/xvfb.rs` (`start` to accept argv), `src/service/dbus.rs` (new `set_mode_argv` or extend `ModeChange::Xvfb` with an argv form), `src/dbus_types.rs` (proxy). Keep the existing `sh -c` path for the freeform custom command.

**Confirm checklist:**
- [ ] Failing test FIRST: a preset arg containing a space (`-c /path with space/x.conf`) launches without word-splitting.
- [ ] Preset path uses `Command::new(argv[0]).args(&argv[1..])`, no shell.
- [ ] Custom command still routes through `sh -c` (documented as intentional).
- [ ] `.process_group(0)` preserved on the child.
- [ ] D-Bus signature additions reflected in `dbus_types.rs` proxy + `cli.rs` if needed.
- [ ] Committed.

### Task 8: Daemon-side binary `resolve` D-Bus method [DO-CONFIRM]

> arch H3: detection must use the DAEMON's PATH. Spike Task 0 confirmed the gap.

**Files:** Modify `src/service/dbus.rs` (new `resolve_binaries(names: Vec<String>) -> HashMap<String,String>` returning name→absolute-path-or-empty, resolved in the daemon's env), `src/dbus_types.rs` (proxy).

**Confirm checklist:**
- [ ] Failing test FIRST: resolves a known binary (`sh`) to an absolute path, unknown → empty.
- [ ] Uses the daemon process's own `PATH` (which the daemon inherits), not a hardcoded list.
- [ ] Returns absolute paths so the GUI can bake them into argv (avoids re-resolution mismatch).
- [ ] Proxy method added to `dbus_types.rs`.
- [ ] Committed.

### Task 8b: Seed conky + cava starter configs [DO-CONFIRM]

**Files:** Add `layouts/wrappers/conky-480.conf`, `layouts/wrappers/cava-480.conf` (or a `wrappers/` dir); modify `src/config.rs` seed-if-missing block (`:388-451`) to `include_str!` + write them to `~/.config/thermalwriter/wrappers/` on first run.

**Confirm checklist:**
- [ ] conky config: `own_window_type='desktop'`, `double_buffer=true`, 480×480 pinned, `background=false`, opaque Tokyo-Night bg, colors ≥ `#999999`, fonts ≥ 14px (memory `feedback_lcd_brightness`).
- [ ] cava config: `method=pulse`, `source=auto`, `sdl_width/height=480`, high-contrast gradient; WM/stipple decision from Task 0 applied.
- [ ] Seeded only if missing (don't clobber user edits).
- [ ] Manual: `thermalwriter ctl mirror "conky -c ~/.config/thermalwriter/wrappers/conky-480.conf"` renders full-frame on hardware (memory `feedback_hardware_verification`).
- [ ] Committed.

### Task 8c: Review Tasks 5-8b
**Killer items (blocking):**
- [ ] tmpfs frame write wrapped in `block_in_place`; only writes in xvfb mode; `last.jpg` removed on exit.
- [ ] `mode_change_lock` covers the full transition; `display.mode` never written as `xvfb`.
- [ ] tick-rate + `active_layout` restored on stop (verify `ctl status` after a stop).
- [ ] Preset argv path handles spaces; custom path still `sh -c`.
- [ ] `resolve_binaries` uses daemon PATH, returns absolute paths.
- [ ] Seeded configs render full-frame on hardware; not clobbered if present.
- [ ] `cargo test` passes.

**Quality items (non-blocking):**
- [ ] `frame_dump` helper unit-tested for torn-write safety.
- [ ] Wrapper configs documented in CLAUDE.md Config section.
- [ ] cava WM dependency (if any) noted in External Prerequisites.

### Task 8d: Milestone — daemon streaming surface complete
**Present to user:** tmpfs preview, session-only transition, argv launch, resolve method, seeded configs + a hardware screenshot of conky streaming. **Wait for user response.**

---

## Phase 3 — GUI Stream Tab

### Task 9: Tauri commands + dialog plugin [READ-DO]

**Files:** Modify `gui/src-tauri/Cargo.toml` (+`tauri-plugin-dialog = "2"`), `gui/src-tauri/tauri.conf.json` (plugins/capabilities), `gui/src-tauri/src/commands.rs` (+`apply_stream`, `stop_stream`, `read_frame`, `resolve_binaries`), `gui/src-tauri/src/lib.rs` (register in `generate_handler!`). Test: `commands.rs` `#[cfg(test)]`.

**Step 1: Write failing test** — `read_frame` returns the bytes of `$XDG_RUNTIME_DIR/thermalwriter/last.jpg` (point at a temp dir via env in test); missing file → a clean "no frame" error, not a panic.

**Step 2: Run — expect FAIL.**

**Step 3: Implement.** `apply_stream(argv: Vec<String>)` → `DisplayProxy::set_mode_argv`/`set_mode` (Task 7 contract) following `apply_to_daemon` (`commands.rs:241-276`). `stop_stream(layout)` → `set_mode("svg"|"html", layout)`. `read_frame()` → raw JPEG bytes (model `read_background` `:288-296`). `resolve_binaries(names)` → proxy call (Task 8). Register all in `lib.rs`. Add dialog plugin init.

**Step 4: Run tests — expect PASS;** `cd gui/src-tauri && cargo test`.

**Step 5: Commit.** `feat(gui): stream Tauri commands + dialog plugin`

### Task 10: Preset registry + StreamTab + tab nav [READ-DO]

**Files:** Create `gui/src/lib/streamPresets.ts` (declarative registry), `gui/src/lib/StreamTab.svelte` (model on `BgGallery.svelte`). Modify `gui/src/App.svelte` (add a Stream tab to the nav; reuse `kindClass()`'s existing `kind-xvfb`).

**Step 1:** Define the registry array: each entry `{ id, label, binary, needs_terminal, args (argv template with `{field}` slots), fields, default_fps }` for conky, cava, btop, nvtop, custom. Terminal wrapping maps the resolved terminal to its own flag syntax (alacritty/kitty `-o`, xterm `-fa/-fs`).

**Step 2:** Build `StreamTab.svelte`: app dropdown, per-preset fields (conky: built-in vs custom path + `@tauri-apps/plugin-dialog` `open()`), FPS control, Start/Stop. On mount call `resolve_binaries` for all preset binaries + terminals (daemon env) → grey out unresolved with an install hint. Greyed entirely if Xvfb unresolved.

**Step 3:** Wire Start → build argv from registry + resolved absolute paths → `invoke("apply_stream", {argv})`. Stop → `invoke("stop_stream", {layout: selectedLayout})`.

**Step 4:** Add tab switching to `App.svelte` (Variables | Stream) — minimal `$state` `activeTab`.

**Step 5:** `cd gui && npm run build` (or `tauri:dev`) — verify the tab renders, presets grey correctly. Use real Unicode chars, not HTML entities, in Svelte expressions (memory `feedback_svelte5_entity_decoding`).

**Step 6: Commit.** `feat(gui): stream tab + declarative preset registry`

### Task 11: Live preview polling + offline/mode gating [DO-CONFIRM]

**Files:** Modify `gui/src/lib/StreamTab.svelte` (poll loop), `gui/src/App.svelte` (offline detection fix). **Coordination required:** confirm `read_frame` error shape with the Task 9 dev.

**Implement:** While streaming, `setInterval(~333ms)` → `invoke("read_frame")` → Blob URL → canvas (model `BgGallery loadThumb` `:59-72`); `clearInterval` on tab leave / `onDestroy`. Gate "is streaming" on `get_status().mode == "xvfb"` (read on mount — handles a stream started by a prior GUI). Replace App.svelte's string-match offline detection (`:180`) with a structured D-Bus error-name check.

**Confirm checklist:**
- [ ] Failing test/manual FIRST: poll paints frames at ~3fps while streaming; stops on tab leave (no leaked interval).
- [ ] Live preview only for xvfb mode; svg/html keep `render_preview` (no dual source).
- [ ] Mount reads `get_status` mode — a daemon already streaming shows correct state.
- [ ] Offline detection uses the structured error, not `"daemon is not running"` substring.
- [ ] No interval leak (verify in devtools across tab switches).
- [ ] Committed.

### Task 12: Review Tasks 9-11
**Killer items (blocking):**
- [ ] `read_frame` returns bytes / clean error; registered in `lib.rs`.
- [ ] argv built with daemon-resolved absolute paths; per-terminal flag syntax correct (xterm uses `-fa/-fs`, not `-o`).
- [ ] Presets grey out based on **daemon** `resolve_binaries`, not GUI PATH.
- [ ] Poll interval cleared on tab leave/destroy (no leak); live preview xvfb-only.
- [ ] Offline detection uses structured D-Bus error name.
- [ ] `cd gui/src-tauri && cargo test` and `cd gui && npm run build` pass.

**Quality items (non-blocking):**
- [ ] StreamTab follows BgGallery component conventions.
- [ ] Registry is pure data (no logic creeping into `command_template`).
- [ ] Conky file picker uses native dialog; rejects non-files gracefully.

### Task 13: Milestone — GUI streaming complete (final)
**Present to user:** full Stream tab demo (pick conky → Start → live preview → Stop), preset greying on a machine missing nvtop, and on-hardware confirmation. Update CLAUDE.md (Tauri GUI + Config sections) and memories. **Wait for user response.**

---

## Notes
- Hardware verification is required before claiming streaming works (memories `feedback_hardware_verification`, `feedback_display_review`). Stop the daemon before `render_layout` tests; restart after.
- Design source: `docs/brainstorms/2026-05-29-gui-streaming-conky-design.md` (refined, adversarially reviewed).
