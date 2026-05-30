# GUI Streaming + Conky/Cava/btop — Execution Checkpoint

**Updated:** 2026-05-30 · **Status:** ✅ ALL PHASES COMPLETE — hardware + live-GUI verified, signed off by Mike, merging to master. Phase 3 GUI Stream tab driven end-to-end (Start conky → live un-rotated preview → Stop). Two killer runtime bugs caught only by launching the app: dialog-config boot panic (`2235989`) + self-referential `$effect` reactivity freeze (`d727801`) — both fixed + reviewed. Visual verification via `npm run tauri:dev` + Tauri MCP is REQUIRED for GUI work; unit tests + review miss runtime/reactivity bugs.

## Phase 2 — DONE (hardware-verified, both reviewers approved every task)

Commits on `feat/gui-streaming` (base `8316c48` → `7b1dd00`):
| Commit | What |
|--------|------|
| `bc6503a` | Task 5: tmpfs last-frame readback ($XDG_RUNTIME_DIR/thermalwriter/last.jpg, block_in_place, xvfb-only via FrameSource::is_streaming(), cleared on exit) |
| `c4bea3b` | Task 6: session-only mode transition + mode_change_lock + tick-rate push/restore |
| `7b1f94e` `6b0ad3f` `10638fa` | Task 8b: seed conky-480.conf + cava-480.conf wrapper configs + startup wiring |
| `94e0744` `3e83efa` | Task 7+11: argv-based preset launch (no shell) + generic set_mode_argv D-Bus method + global SDL_VIDEODRIVER=x11 |
| `0c0593b` | Task 8: resolve_binaries D-Bus method (daemon PATH, absolute paths) |
| `f6be082` `9e801aa` `d09bfa4` | review fixes (frame_dump honesty+fsync, Task 6 real-state tests, clippy/no-op cleanup) |
| `6631a8a` `368ff4e` | **Hardware-found bug:** set_layout didn't restore tick_rate on stream exit; + set_layout_vars-while-streaming guard (option b) + restore_from_streaming helper |
| `7b1dd00` | **Hardware-found bug:** cava preset used `--config`, must be `-p`; + preset argv regression tests |

**Hardware-verified:** conky streams full-frame (tmpfs frame captured); cava streams audio-reactive bars (test tone); tick-rate push(15)/restore(2) confirmed; clean SIGTERM teardown + last.jpg cleared; dead-child detection refuses mode=xvfb. 58 lib tests green, clippy clean.

**Mike's decisions at the Phase 2 milestone:**
1. cava bars sparse at 480px → POLISH the cava-480.conf now (Task #14, dev-2) — re-verify on hardware.
2. tmpfs frame is 180° rotated (post-rotation) → GUI UN-ROTATES it for the live preview (Task #17).
3. Proceed to Phase 3.

## Phase 3 — IN PROGRESS (GUI Stream tab)
- Task #14 cava polish (dev-2) → Task #15 Tauri commands apply_stream/stop_stream/read_frame/resolve_binaries + dialog plugin (dev-1, gui/src-tauri) → Task #16 StreamTab + streamPresets.ts registry + tab nav (dev-2, gui/src) → Task #17 live preview polling, GUI un-rotates 180°, structured offline detection (dev-2).
- Pinned Tauri command contract: apply_stream({argv}), stop_stream({layout}), read_frame()→bytes, resolve_binaries({names})→map.
- Partition: dev-1 = gui/src-tauri/*; dev-2 = gui/src/* + layouts/wrappers/cava-480.conf.

---
### (original Phase-1 checkpoint below)

This file lets a fresh session resume the plan execution without re-deriving context.

## How to Resume

1. Read the plan: `docs/plans/2026-05-29-gui-streaming-conky.md` (tasks, file:line anchors, review checklists). The **Spike Results appendix** at the bottom holds verified Phase-0 findings — honor them.
2. Read this checkpoint.
3. Re-invoke `forge:executing-plans`. Recreate the team (below). Phase 2 = Tasks 5–8b; Phase 3 = Tasks 9–11. Each phase ends at a milestone requiring user sign-off.

## Where the Work Lives

- **Branch:** `feat/gui-streaming`
- **Worktree:** `/home/mike/code/thermalrighter/.worktrees/gui-streaming` (git worktree; `.worktrees/` is gitignored)
- **Live daemon:** systemd user service runs `~/.cargo/bin/thermalwriter daemon` (the OLD installed binary, NOT this branch). To hardware-test branch code: `systemctl --user stop thermalwriter`, run `target/debug/thermalwriter daemon` in background, test via `target/debug/thermalwriter ctl ...`, then kill it and `systemctl --user start thermalwriter` + `ctl layout svg/neon-dash-v2.svg`. ALWAYS restore.

## Team to Recreate (executing-plans model)

TeamCreate `gui-streaming`, then spawn (all `sonnet`, `run_in_background`):
- **dev-1** — owns main.rs/dbus.rs (Tasks 6, and Phase 3 daemon-side bits). Did Tasks 1, 2.
- **dev-2** — owns xvfb.rs/config.rs/tick.rs. Did Task 3 + fixes.
- **spec-reviewer** — persistent; reviews every task vs plan killer items.
- **quality-reviewer** — persistent; reviews code quality concurrently.
Run a kickoff (each states role + concrete watch-fors) before claiming tasks. Both reviewers fire in parallel on each completion; route findings at dev pause points; milestones are hard gates.

NOTE: Phase 1's team was shut down at this checkpoint. Recreate fresh.

## Phase 1 — DONE (6 commits on feat/gui-streaming)

| Commit | Task | What |
|--------|------|------|
| `2024876` | Task 1 | oneshot-`ack` on every `ModeChange` variant; `set_mode` awaits ack and only mutates `state.mode`/`active_layout` on Ok; on Err returns `zbus::fdo::Error::Failed`. Logic in `set_mode` (dbus.rs) + listener arms (main.rs). |
| `cb738fe` | Task 2 | Defer xvfb-handle drop until replacement source confirmed sent. Extracted `service::mode_handler::build_layout_source`. All 3 arms: build+send new source FIRST, drop old handle only after. |
| `483559e` | Task 3 | Atomic display alloc via `-displayfd` (replaced TOCTOU `find_unused_display`). |
| `e026224` | Task 3-fix ⭐ | **High display base `:100` + retry-on-collision.** See gotcha below. |
| `2886fdc` | quality | Close pipe fds (OwnedFd) on Xvfb spawn-failure path. |
| `ff10c92` | Task 1b | **Child-liveness check**: after spawning the `sh -c` child, 150ms grace + `try_wait`; if it already exited, drop the handle (kills Xvfb, removes fbdir) and return Err. Closes "dead child reported as streaming" gap. |

**Tests:** 198 passing, 0 failing, 1 ignored. Both reviewers APPROVED every task (killer items verified against source).

**Hardware-verified (on the real cooler, worktree binary as daemon):**
- Task 1: occupying displays `:100–:119` forces a real `start()` failure → `set_mode` returns a clean D-Bus error and **mode stays unchanged**.
- Task 1b: `ctl mirror "this-binary-does-not-exist"` → D-Bus error (`Streamed child exited immediately … status: exit 127`) + **mode unchanged**. Good mirror still switches to xvfb on `:100`.
- Task 2: bad layout while streaming → error, **stream keeps rendering** (no frozen frame).
- Task 3: daemon's Xvfb comes up on `:100`, not `:1`. cava renders bars end-to-end (verified with a tone playing).

## ⚠️ Critical Gotchas Discovered (carry into Phase 2)

1. **Display base MUST be ≥100.** Bare `Xvfb -displayfd` scans from `:0` and on this box returns `:1` — colliding with the live `Xwayland :1` desktop (squats its X namespace; a streamed app could attach to the real desktop). Fixed: `DISPLAY_BASE=100`, `DISPLAY_MAX_TRIES=20`, explicit `Xvfb :B -displayfd <fd>` with retry-on-collision (explicit numbers do NOT auto-scan). Don't regress this.
2. **cava SDL needs `SDL_VIDEODRIVER=x11`.** The daemon env carries `WAYLAND_DISPLAY`; SDL auto-probes Wayland and crashes with `double free or corruption`. Forcing `SDL_VIDEODRIVER=x11` (set it in the cava preset launch / seeded-config wrapper) makes it stable. Without a WM the SDL window still fills 480×480 at (0,0) — no matchbox needed.
3. **cava `bars`:** use `bars = 0` (auto) or `≤ 22`. `bars = 24` aborts cava at 480px (`window too narrow`). cava reaching audio = default-sink `.monitor` flips SUSPENDED→RUNNING; a blank frame with RUNNING monitor just means silence (e.g. paused/corked player), not a bug.
4. **cava-from-daemon audio CONFIRMED.** Daemon env has `XDG_RUNTIME_DIR=/run/user/1000` + pipewire/pulse sockets; cava reaches the default-sink monitor. Task 6 is green-lit.
5. **Daemon PATH is rich, gap is immaterial.** systemd user PATH already has all app binaries in `/usr/bin` (Xvfb, cava, conky, btop, kitty, alacritty present; nvtop/xterm absent). Task 8 (`resolve_binaries`) stays — justified as robustness (return absolute paths to avoid exec-time re-resolution), not PATH-gap mitigation.
6. **Child-liveness caveat:** the 150ms check fails apps that daemonize/fork-and-parent-exit. Seeded configs MUST force foreground (`conky background=false`); prefer alacritty / `kitty --single-instance=no` for TUIs (design doc item 6).

## Open Non-Blocking Quality Items (from Phase 1 reviews — address opportunistically)

- `dbus.rs` ack-contract unit test asserts on a local sentinel, not through the real `set_mode` codepath. Real protection is the code ordering (verified by review + hardware). Strengthening needs a D-Bus harness.
- `mode_handler` re-reads the layout file a second time in the template hot-swap path (Layout arm) — minor double-read.

## Remaining Work

**Phase 2 — Daemon Streaming Surface (Tasks 5–8b), then milestone 8d:**
- Task 5: tmpfs last-frame readback (`$XDG_RUNTIME_DIR/thermalwriter/last.jpg`), write wrapped in `block_in_place`, xvfb-mode-only, removed on exit. (independent — can parallel)
- Task 6 [DO-CONFIRM]: session-only mode transition + tick-rate push + `mode_change_lock`. Builds on Task 1's ack contract. NEVER persist `display.mode="xvfb"`.
- Task 7 [DO-CONFIRM]: argv-based xvfb launch for presets (structured argv, no shell); custom commands keep `sh -c`. Needs xvfb.rs `start` to accept argv + dbus/proxy signature.
- Task 8 [DO-CONFIRM]: daemon-side `resolve_binaries(names)->map` (absolute paths, daemon PATH).
- Task 8b [DO-CONFIRM]: seed conky + cava starter configs to `~/.config/thermalwriter/wrappers/` (apply gotchas 2/3/6 + LCD brightness: opacity ≥0.7, colors ≥#999999, fonts ≥14px). cava wrapper must set `SDL_VIDEODRIVER=x11`.
- Task 8c review, Task 8d milestone (hardware screenshot of conky streaming).

**Phase 3 — GUI Stream Tab (Tasks 9–11), then final milestone 13:**
- Task 9: Tauri commands (`apply_stream`, `stop_stream`, `read_frame`, `resolve_binaries`) + `tauri-plugin-dialog`; register in lib.rs.
- Task 10: preset registry (`streamPresets.ts`) + `StreamTab.svelte` (model on BgGallery) + tab nav in App.svelte. Per-terminal flags: alacritty/kitty `-o`, xterm `-fa/-fs` (xterm absent here). Use real Unicode, not HTML entities (Svelte 5).
- Task 11 [DO-CONFIRM]: live preview polling (~333ms read_frame→canvas), xvfb-only, gate on `get_status().mode`, clear interval on tab leave; replace App.svelte string-match offline detection with structured D-Bus error.
- Task 12 review, Task 13 final milestone.

## Uncommitted at Checkpoint

- `docs/plans/2026-05-29-gui-streaming-conky.md` — Spike Results appendix (Phase 0 findings). Commit alongside this checkpoint.
