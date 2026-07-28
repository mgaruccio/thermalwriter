# Agent Testing Runbook (Tauri MCP Bridge)

This document describes how an automated agent (or human with the [MCP Server for Tauri](https://github.com/hypothesi/mcp-server-tauri)) can drive **dev builds** of Thermalwriter Config through representative user journeys.

> **Release builds must not expose MCP.** Production GUI artifacts are built without the `devtools` feature; the `tauri-plugin-mcp-bridge` crate is not linked and release QA asserts the binary contains no `plugin:mcp-bridge` / `mcp-bridge:default` strings. Only `npm run tauri:dev` (which passes `--features devtools`) enables the bridge.

## Prerequisites

| Requirement | Notes |
| --- | --- |
| Rust 1.85+, Node 22 | Same as `CONTRIBUTING.md` |
| `gui/` dependencies | `cd gui && npm ci` |
| MCP server | `npx mcp-server-tauri` (or configured in your agent's MCP settings) |
| Isolated config (recommended) | Separate `HOME` or `XDG_CONFIG_HOME` so tests do not mutate your live config |

### Dev GUI startup

```sh
cd gui
# Starts Vite + Tauri with devtools + MCP bridge on 127.0.0.1:9223 (or next free port 9223–9322)
npm run tauri:dev
```

The plugin logs initialization on stderr, e.g. `MCP Bridge plugin initialized ... on 127.0.0.1:9223`. Connect the MCP server to that host/port.

### Isolated runtime layout

```sh
export TW_AGENT_HOME="$(mktemp -d)"
export XDG_CONFIG_HOME="$TW_AGENT_HOME/.config"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
mkdir -p "$XDG_CONFIG_HOME/thermalwriter"/{layouts,backgrounds,wrappers}
```

Optional null-transport daemon in another terminal:

```sh
export XDG_CONFIG_HOME="$TW_AGENT_HOME/.config"
THERMALWRITER_TRANSPORT=null THERMALWRITER_PROFILE=grand-vision-480 \
  cargo run --release -- daemon
```

Stop the daemon with `thermalwriter ctl stop` or `pkill -f 'thermalwriter daemon'` when finished.

## MCP tools overview

With the bridge active, the MCP server exposes WebView automation (`webview_dom_snapshot`, `webview_find_element`, `webview_interact`, `webview_screenshot`, `webview_execute_js`, …) and IPC tools (`ipc_execute_command`, `ipc_monitor`, `ipc_get_captured`, `ipc_get_backend_state`).

Prefer **semantic inspection** over brittle CSS hooks:

- `webview_dom_snapshot` with accessibility tree — locate buttons by visible label text ("Apply", "Variables", "Stream", theme names).
- `webview_find_element` with text or role queries when the snapshot exposes stable names.
- Corroborate UI actions with `ipc_get_captured` / daemon `thermalwriter ctl status` where applicable.

## Journey 1 — GUI offline (daemon stopped)

**Goal**: layout browse, local preview, offline Apply persists to config.

1. Ensure no daemon: `thermalwriter ctl status` should fail.
2. Launch `npm run tauri:dev` with isolated `XDG_CONFIG_HOME`.
3. Snapshot DOM — confirm status shows offline/daemon unavailable wording.
4. Select a built-in layout (e.g. neon-dash-v2) via sidebar/list interaction.
5. Snapshot preview region — image/canvas should update (not a blank error tile).
6. Open **Variables**, change a text/color var, click **Apply**.
7. Read `config.toml` via shell — confirm `[layout_vars]` entry.
8. Restart GUI — values should reload from disk.

**Pass criteria**: preview renders; config file updated; no daemon required.

## Journey 2 — GUI online (null-transport daemon)

**Goal**: Live Apply reaches D-Bus; status reflects daemon.

1. Start null-transport daemon sharing `XDG_CONFIG_HOME`.
2. Relaunch or refresh GUI — status should show online/connected semantics.
3. Change layout from GUI — verify `thermalwriter ctl status` reports new `active_layout`.
4. Toggle a variable and **Apply** — confirm both config file and daemon state (status or preview fingerprint).

## Journey 3 — Background import and color suggestions

**Goal**: BgGallery import + Suggest colors.

1. Select a layout with color frontmatter vars and a transparent panel (neon-dash-v2).
2. Import a small PNG/JPEG via gallery Import control.
3. Select imported background — preview should composite under layout.
4. Click **◑ Suggest** (or equivalent) with background + color vars present.
5. Snapshot variables panel — accent colors should shift (not all defaults).
6. Apply and reopen — background filename persists in config.

**Error path**: import a non-image or >8 MB file — expect user-visible error, no partial file in `backgrounds/`.

## Journey 4 — Theme switching

**Goal**: Theme persists in localStorage without external font loads.

1. Open theme selector (header/settings).
2. Cycle Tokyo Night Storm → Catppuccin Mocha → Nord (or available set).
3. Snapshot computed styles on `html` or body — `data-theme` attribute changes.
4. Network tab / CSP: **no** requests to `fonts.googleapis.com` or `fonts.gstatic.com` (fonts are vendored in `gui/src/fonts.css`).
5. Reload app — theme should restore from localStorage.

## Journey 5 — Stream tab (daemon + Xvfb)

**Goal**: start stream, live preview, stop back to layout.

**Requires**: `Xvfb` on PATH, daemon running, at least one stream binary resolved (e.g. `cava`).

1. Open **Stream** tab.
2. Confirm unavailable presets are disabled (daemon `resolve_binaries` map).
3. Start **cava** (or conky with wrapper config) at modest FPS.
4. Poll stream preview `<img>` — JPEG bytes should update (~3 fps); preview may be rotated 180° in CSS (expected).
5. `thermalwriter ctl status` — `mode` should reflect xvfb/stream.
6. Stop stream — returns to saved layout; preview img polling pauses when tab hidden.

**Error path**: start with missing Xvfb — expect actionable error in UI or logs.

## Journey 6 — Invalid paths and IPC errors

| Action | Expected |
| --- | --- |
| Custom stream with non-absolute argv[0] | Rejected before spawn |
| Apply while daemon dies mid-call | Graceful error; config may still save locally |
| Layout file deleted on disk | Preview/apply error surfaced |

Capture `webview_screenshot` + last 50 lines of GUI stderr for failures.

## Cleanup

```sh
thermalwriter ctl stop 2>/dev/null || true
pkill -f 'thermalwriter-gui' 2>/dev/null || true
rm -rf "$TW_AGENT_HOME"
```

## Result report template

```markdown
## Agent test run — Thermalwriter Config

- **Date**:
- **Branch/tag**:
- **GUI command**: `npm run tauri:dev`
- **MCP host:port**: 127.0.0.1:9223
- **Isolated HOME**: yes/no
- **Daemon**: null-transport / live hardware / offline

| Journey | Result | Notes |
| --- | --- | --- |
| Offline layout/preview/apply | PASS/FAIL | |
| Online apply + status | PASS/FAIL | |
| Background + suggest colors | PASS/FAIL | |
| Theme switch + local fonts | PASS/FAIL | |
| Stream start/preview/stop | PASS/FAIL | |
| Error paths | PASS/FAIL | |

### Artifacts
- Screenshots: ...
- DOM snapshots: ...
- journalctl excerpt: ...
```

## Related docs

- `docs/gui.md` — feature reference
- `docs/troubleshooting.md` — user-facing fixes
- `CONTRIBUTING.md` — human developer checks
