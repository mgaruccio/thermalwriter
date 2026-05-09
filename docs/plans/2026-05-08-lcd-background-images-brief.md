# LCD Background Images — Execution Brief

## Getting Started

1. Read the design doc: `docs/plans/2026-05-08-lcd-background-images-design.md`
2. Read the full plan: `docs/plans/2026-05-08-lcd-background-images-impl.md`
3. Set up a worktree on a new branch: `feature/lcd-background-images`
4. Invoke `forge:executing-plans`

## Top of Mind

- **The `feature/tauri-gui` branch already has uncommitted refactor work (`a5f125d`, `0b684d9`)** that is NOT yet on master. Don't accidentally branch off `feature/tauri-gui` — branch off `master` so this work is independent. If those commits get merged to master before this branch starts, rebase. Verify branch base: `git merge-base master HEAD` should equal `master`'s tip.
- **Daemon is currently stopped** (`systemctl --user stop thermalwriter` was run during planning). Restart at your discretion when you need to test daemon-side changes; expect possible Tera-substitution warnings until Task 1 (the preflight bug fix) lands. Don't restart-loop just to babysit logs.
- **Phase 0 (Task 1) is the bug fix** discovered during planning: `src/main.rs:186-219` constructs the new SvgRenderer on layout reload with `ThemePalette::default()` and never calls `set_history()`. That's why the user saw "Render failed: Tera template substitution failed" after clicking Apply in the GUI. Land this first; everything else builds on the same code path.
- **The user's customized 2 MB `~/.config/thermalwriter/layouts/svg/neon-dash-v2.svg` is untouched** by Phase 2's seeded-layout updates. They'll manually `cp` the new seed in when convenient. Don't write a migration that touches their config dir.
- **Pixel format: putImageData / RawFrame is straight RGBA; tiny_skia::Pixmap is premultiplied.** Same gotcha as the prior plan. `decode_to_pixmap` in Task 5 must premultiply before `Pixmap::from_vec`. `RawFrame::from_pixmap` already unpremultiplies on the way out — don't double-handle.
- **All Tauri commands return `Result<T, AppError>`.** Plan code samples that show `Result<T, String>` are pedagogical sketches, not specs. The killer rubric in the prior `2026-04-04-tauri-gui-impl.md` (line 746) is still authoritative for this plan.
- **Daemon binary lives at `~/.cargo/bin/thermalwriter`** (per `systemctl show`). To test daemon-side changes on real hardware: `cargo install --path . --force --bin thermalwriter && systemctl --user restart thermalwriter`. ~20s rebuild + a 2-second LCD blank during restart.
- **D-Bus introspection is the verification tool.** After daemon-side D-Bus changes: `busctl --user introspect com.thermalwriter.Service /com/thermalwriter/display` must show `SetBackground`, `ClearBackground`, `ListBackgrounds` after Task 9.
- **Live D-Bus monitor for GUI testing.** `busctl --user monitor com.thermalwriter.Service` shows method calls in real time — useful for confirming the GUI is talking to the daemon during Phase 3/4 manual tests.

## Rules

- **Evidence before claims:** Run the command, read the output, THEN claim the result. Don't trust prior runs across context boundaries.
- **Escalate, don't accommodate:** When something fails, surface it. Don't write code that tolerates broken systems. The Phase 0 preflight bug is exactly this pattern — it surfaced during the planning research because someone had been silently working around it.
- **TDD applies to DO-CONFIRM tasks too:** the first checklist item on every confirm list is "failing test written FIRST."
- **Commit at every step boundary the plan specifies.** Don't batch commits across tasks. One task = one (or rarely two) commits with the plan-specified message.
- **`tiny_skia::Pixmap::draw_pixmap` arguments:** `(x: i32, y: i32, source: PixmapRef, paint: &PixmapPaint, transform: Transform, mask: Option<&Mask>) -> Option<()>`. Returns `None` on impossible composite (zero-size source). Use `PixmapPaint::default()` for normal alpha-over.
