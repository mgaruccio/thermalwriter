# Tauri GUI — Execution Brief

## Getting Started
1. Switch to the worktree: `cd /home/mike/code/thermalrighter/.worktrees/tauri-gui` (branch `feature/tauri-gui`)
2. Read the design doc: `docs/plans/2026-04-04-tauri-gui-design.md`
3. Read the full plan: `docs/plans/2026-04-04-tauri-gui-impl.md`
4. Invoke `forge:executing-plans`

## Where We Are
- **Task 1 DONE** on branch `feature/tauri-gui` — two commits:
  - `dfa2dd5` feat(frontmatter): `VariableDecl` + multi-line block parsing
  - `6b102d6` fix(frontmatter): replaced `unwrap`, added warn logging + negative tests
- **Pick up at Task 4** — D-Bus fixes (`list_layouts` SVG support, wire `list_sensors`, path-traversal validation, add `get_layout_vars`/`set_layout_vars`), `Config::save_layout_vars` via `toml_edit`, new `tests/dbus_tests.rs`.
- Tasks 2–3 (review + milestone for Task 1) are effectively closed — treat the second commit as having absorbed the killer-item fixes.

## Top of Mind
- **The daemon is live.** `thermalwriter.service` is running as a systemd user service on master. Do NOT disrupt it while working on the GUI branch. For hardware tests in Phase 5, stop it first: `systemctl --user stop thermalwriter`.
- **Workspace split is in Task 7, not now.** The current branch still builds as a single crate. `rusb`/`memmap2` feature-gating only happens after Task 6.
- **D-Bus proxy extraction is a refinement fix.** Task 7 Step 5 moves `DisplayProxy` from `src/cli.rs:61-74` into a new unconditional `src/dbus_types.rs` module so both CLI and GUI can import it without duplicating. Don't skip this — it's called out as a killer item.
- **`putImageData` gotcha** — when you hit Task 10, the frontend expects **straight (un-premultiplied) RGBA**. Use `RawFrame` (from `FrameSource::render()`), not `pixmap.data()` directly. Plan covers this but it's easy to miss.
- **Plan was refined** via multi-agent + Gemini 3.1 Pro review; its killer-item checklists encode real failure modes. Run them seriously during review tasks — don't checkbox-theater them.
- Untracked `layouts/test-*.html` files on master are unrelated to this work — leave them alone.

## Rules
- **Evidence before claims:** Run the command, read the output, THEN claim the result.
- **Escalate, don't accommodate:** When something fails, surface it. Don't write code that tolerates broken systems.
- **TDD applies to DO-CONFIRM tasks too** — the first checklist item is always "failing test written FIRST."
- **Commit at every step boundary** the plan specifies. Don't batch commits across tasks.
