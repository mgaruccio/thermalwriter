# Adversarial Review Findings — Design Review Resolution Work

Date: 2026-07-02
Scope: uncommitted working tree implementing `docs/design-review-plan.md` (SVG fallback
background clearing, `render_preview` background compositing, HTML layout fixes,
dynamic daemon-status GUI).
Reviewer: Claude only — Gemini was skipped this round (CLI auth broken upstream), so
this lacks the usual second-model independence.

Verified clean before reporting: `cargo fmt --check`, clippy (no warnings), all 25
render/frontmatter tests, GUI `test_cached_renderer_background_clearing`, the
null-background invoke path, daemon-vs-sidebar layout name matching, and
`theme_background` precedence across all four resolution cases.

## Resolution

The code and documentation findings in this punch-list have been fixed; launch
verification covered the safely available real GUI/device state.

- Verification: `cargo fmt --check`, `cargo test -p thermalwriter-gui`, `npm run check`
  from `gui/`, root `cargo test`, and a launched `npm run tauri:dev` session driven
  through the Tauri MCP bridge WebSocket on port 9223.
- Launch verification exercised the real GUI window, background tile selection and
  live canvas preview updates, the observed USB-connected badge state, Apply, and
  sidebar `active-daemon` movement after Apply.
- The launched machine had an online daemon and connected USB display. Disconnected
  and daemon-down badge states remain not safely exercised in launch verification
  because forcing them would require disrupting the user's real daemon or hardware;
  the daemon-down stale-highlight bug was fixed in the generic apply failure path by
  clearing `daemonStatus`.
- The pre-verification Apply run exposed a real active-layout race: `apply_to_daemon`
  updated layout variables but did not wait for the daemon's active renderer/status to
  switch. The GUI command now calls the daemon's acked `set_layout` path after saving
  vars, so `get_status().active_layout` and the sidebar highlight update before Apply
  is reported as live.

## Findings

### 1. RESOLVED — GUI never launch-verified (process gap, not a code defect)

The GUI was launched with `npm run tauri:dev` and driven through the Tauri MCP bridge.
Background tile selection changed the live preview canvas, the observed titlebar badge
reported `USB CONNECTED`, Apply moved `active_layout` to `svg/arc-gauge.svg`, and the
sidebar `active-daemon` class followed. The original `svg/neon-dash-v2.svg` layout and
`anime.png` background were restored after verification.

### 2. RESOLVED — background re-decoded on every preview render

`gui/src-tauri/src/commands.rs` now caches the decoded preview background pixmap in
`RendererCache`, keyed by background name. It validates the requested background path on
every call, reuses the cached pixmap for the same name, decodes again when the name
changes, and clears the cache when the GUI selects no background.

Regression coverage: `test_preview_background_cache_semantics`.

### 3. RESOLVED — status poll never starts if any boot invoke fails

`gui/src/App.svelte` now starts the first `probeDaemon()` and installs the 5-second
poll interval before the boot metadata `Promise.all`. A layout/background/sensor invoke
failure can still show an error, but it no longer strands the titlebar in the probing
state without retries.

### 4. RESOLVED — RGBA hex accepted but alpha is a silent no-op

The alpha semantics are documented in
`skills/designing-layouts/references/components.md`: `theme_background` is the opaque
page-clear color when no image background is selected, and any `#RRGGBBAA` alpha byte is
ignored for that page clear. Other SVG fills/strokes may still use alpha as before.

### 5. RESOLVED — stale `daemonStatus` after generic apply failure

`gui/src/App.svelte` now sets `daemonStatus = null` when the generic Apply failure path
marks `daemonState = "down"`, so stale `active-daemon` highlighting is removed
immediately instead of waiting for the next poll.
