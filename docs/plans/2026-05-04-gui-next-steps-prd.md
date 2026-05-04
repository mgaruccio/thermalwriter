# Thermalwriter Config GUI Next Steps PRD

## Summary

The first GUI pass proves the core flow: users can open the Tauri app, select layouts, edit declared variables, preview locally, and save/apply changes without stopping the already-running daemon. The next pass should make the GUI safer and more faithful by only presenting layouts that can actually work, and by making the preview match the daemon's current saved visual state.

## Goals

- Show only valid, renderable SVG layouts as selectable options.
- Make the preview reflect the current background, color scheme, and saved layout overrides before the user edits anything.
- Keep the GUI usable while the daemon owns the USB interface; UI-only testing must remain the normal fast path.
- Improve feedback for invalid files, daemon availability, and saved-versus-unsaved state.

## Non-Goals

- Do not add HTML layout editing in this pass.
- Do not add daemon lifecycle controls such as start, stop, or restart.
- Do not add a visual drag-and-drop layout editor.
- Do not require hardware or USB access for preview.

## Requirements

### Layout Discovery and Validation

- `list_layouts` must only return `.svg` layouts.
- Each candidate must be validated before it is shown:
  - path stays inside the layout directory,
  - file can be read,
  - frontmatter parses,
  - `SvgRenderer::new` succeeds,
  - one mock-data render succeeds.
- Invalid layouts should be hidden from the selectable list.
- Add a secondary diagnostic path, such as a backend log line or future "Skipped layouts" developer panel, so bad user layouts are debuggable without cluttering v1 UI.

### Preview Fidelity

- Initial preview must use the same value precedence as daemon rendering:
  1. frontmatter defaults,
  2. current theme/manual palette,
  3. saved `[layout_vars."<layout>"]`,
  4. unsaved form edits.
- The UI form should initialize from saved layout vars when present, not just frontmatter defaults.
- Background and color scheme variables must be applied before first render so the preview matches the currently configured display state.
- If a saved override references a variable no longer declared by the layout, ignore it for rendering and do not show it in the form.

### User Experience Refinements

- Show active/current layout at the top of the list when known from config or daemon status.
- Distinguish state clearly:
  - "Saved and applied" when D-Bus apply succeeds,
  - "Saved, daemon unavailable" when config persistence succeeds but live apply cannot happen,
  - "Unsaved changes" after form edits.
- Disable Apply when there are no changes or no valid layout selected.
- Keep the 480x480 preview stable in size; no layout shift during rendering.
- Keep controls dense and utilitarian: this is a configuration tool, not a landing page.

## Suggested Implementation

- Move layout validation into a shared backend helper used by `list_layouts` and `render_preview`.
- Return `LayoutSummary { name, configurable, active }` for valid SVG layouts only.
- Add a helper that builds preview vars by merging defaults, saved config, and current form vars.
- Ensure `SvgRenderer` remains the single rendering path for both daemon and GUI preview.
- Add tests for:
  - HTML layouts excluded,
  - unreadable/unrenderable SVGs excluded,
  - saved theme/background overrides appear in first preview,
  - stale saved vars ignored,
  - Apply disabled/no-op behavior covered at the Svelte component level if a frontend test harness is added.

## Acceptance Criteria

- Running only `npm run tauri -- dev` opens a GUI with renderable SVG layouts only.
- Selecting the default layout immediately shows the saved/current color scheme and background in preview.
- Bad user SVG files do not appear as selectable options.
- The already-running daemon can remain active; no USB-busy error is required to test the GUI.
- Verification passes:
  - `cargo test`
  - `cargo test -p thermalwriter-gui`
  - `cd gui && npm run tauri -- build --debug`
