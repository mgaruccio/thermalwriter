# Agent Testing Runbook (Tauri MCP Bridge)

This document describes how an automated agent (or human with the [MCP Server for Tauri](https://github.com/hypothesi/mcp-server-tauri)) can drive **dev builds** of Thermalwriter Config through representative user journeys.

> **Release builds must not expose MCP.** Production GUI artifacts are built without the `devtools` feature; the `tauri-plugin-mcp-bridge` crate is not linked and release QA asserts the binary contains no `plugin:mcp-bridge` / `mcp-bridge:default` strings. Only `npm run tauri:dev` (which passes `--features devtools`) enables the bridge.

## Prerequisites

| Requirement | Notes |
| --- | --- |
| Rust 1.85+, Node 22 | Same as `CONTRIBUTING.md` |
| `gui/` dependencies | `cd gui && npm ci` |
| `dbus-run-session` | From `dbus-x11` / `dbus` — required for isolated D-Bus |
| MCP server | `npx mcp-server-tauri` (or configured in your agent's MCP settings) |

## Safe isolated setup (`dbus-run-session` + isolated config)

**`XDG_CONFIG_HOME` alone does not isolate D-Bus.** The daemon registers `com.thermalwriter.Service` on the **session bus**. If your normal user daemon is running, an agent-started null daemon on the same bus collides, the GUI may talk to the live daemon, and a careless `thermalwriter ctl stop` stops production.

**Run daemon, GUI, and corroborating `ctl` inside a dedicated `dbus-run-session`.** Your normal login-session daemon and its D-Bus name stay untouched.

The MCP bridge listens on **localhost WebSocket** (`127.0.0.1:9223` by default). The MCP server connects from outside the `dbus-run-session` — that is expected and safe.

### One-time launcher (copy into your agent harness)

```sh
TW_REPO="$(git rev-parse --show-toplevel)"
export TW_AGENT_HOME="$(mktemp -d)"
export TW_REPO

# Optional: note whether a live daemon was healthy before tests (sanity check after cleanup).
TW_LIVE_DAEMON_OK=0
if thermalwriter ctl status >/dev/null 2>&1; then
  TW_LIVE_DAEMON_OK=1
  echo "note: live user daemon is running — agent session uses a private bus; it will not be stopped"
fi

TW_SESSION_PID=""
TW_DAEMON_PID=""
TW_GUI_PID=""

tw_kill_tracked() {
  local pid="$1"
  if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
    kill -TERM "$pid" 2>/dev/null || true
    for _ in 1 2 3 4 5; do
      kill -0 "$pid" 2>/dev/null || return 0
      sleep 0.2
    done
    kill -KILL "$pid" 2>/dev/null || true
  fi
}

tw_agent_cleanup() {
  tw_kill_tracked "$TW_GUI_PID"
  tw_kill_tracked "$TW_DAEMON_PID"
  tw_kill_tracked "$TW_SESSION_PID"
  rm -rf "$TW_AGENT_HOME"
}

trap tw_agent_cleanup EXIT INT TERM

# Private D-Bus + isolated config. Children inherit DBUS_SESSION_BUS_ADDRESS.
dbus-run-session -- bash -c '
  set -euo pipefail
  export TW_AGENT_HOME="'"$TW_AGENT_HOME"'"
  export TW_REPO="'"$TW_REPO"'"
  export XDG_CONFIG_HOME="$TW_AGENT_HOME/.config"
  export XDG_RUNTIME_DIR="'"${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"'"
  mkdir -p "$XDG_CONFIG_HOME/thermalwriter"/{layouts,backgrounds,wrappers}

  # Agent corroboration from outside this shell: source this file.
  printf "DBUS_SESSION_BUS_ADDRESS=%s\n" "$DBUS_SESSION_BUS_ADDRESS" \
    > "$TW_AGENT_HOME/dbus.env"
  printf "XDG_CONFIG_HOME=%s\n" "$XDG_CONFIG_HOME" \
    >> "$TW_AGENT_HOME/dbus.env"

  # --- offline journeys: GUI only (no daemon) ---
  if [[ "${TW_AGENT_MODE:-offline}" == "offline" ]]; then
    cd "$TW_REPO/gui"
    npm run tauri:dev &
    echo $! > "$TW_AGENT_HOME/gui.pid"
    wait
    exit 0
  fi

  # --- online journeys: null-transport daemon + GUI on the same private bus ---
  THERMALWRITER_TRANSPORT=null thermalwriter daemon &
  echo $! > "$TW_AGENT_HOME/daemon.pid"

  cd "$TW_REPO/gui"
  npm run tauri:dev &
  echo $! > "$TW_AGENT_HOME/gui.pid"

  wait
' &
TW_SESSION_PID=$!

# Wait for dbus.env (session ready).
for _ in $(seq 1 50); do
  [[ -f "$TW_AGENT_HOME/dbus.env" ]] && break
  sleep 0.2
done
[[ -f "$TW_AGENT_HOME/dbus.env" ]] || { echo "agent session failed to start" >&2; exit 1; }

# Track child PIDs written by the inner shell (kill only these on cleanup).
if [[ -f "$TW_AGENT_HOME/daemon.pid" ]]; then
  TW_DAEMON_PID="$(cat "$TW_AGENT_HOME/daemon.pid")"
fi
if [[ -f "$TW_AGENT_HOME/gui.pid" ]]; then
  TW_GUI_PID="$(cat "$TW_AGENT_HOME/gui.pid")"
fi
```

### Corroborating CLI from the agent (same private bus)

Never run bare `thermalwriter ctl` during agent tests — that hits the **live** session bus.

```sh
# One-off status on the isolated bus:
env "$(grep -v '^#' "$TW_AGENT_HOME/dbus.env" | xargs)" \
  thermalwriter ctl status

# Or wrap any ctl invocation:
tw_agent_ctl() {
  env "$(grep -v '^#' "$TW_AGENT_HOME/dbus.env" | xargs)" thermalwriter ctl "$@"
}
```

### Null-transport profiles

Default null transport uses the built-in bulk 480×480 fixture (`bulk-87ad-70db-pm4-sub5-fbl72`). For a different negotiated profile, export before starting the session:

```sh
export THERMALWRITER_PROFILE=ly-0416-5408-pm65-sub3-fbl192
```

Set `TW_AGENT_MODE=offline` before the launcher block when running Journey 1 without a daemon.

### Dev GUI / MCP

Inside the session, `npm run tauri:dev` starts Vite + Tauri with `devtools` + MCP on `127.0.0.1:9223` (or the next free port in 9223–9322). Connect your MCP server to that host/port from **outside** `dbus-run-session`.

The plugin logs initialization on stderr, e.g. `MCP Bridge plugin initialized ... on 127.0.0.1:9223`.

## MCP tools overview

With the bridge active, the MCP server exposes WebView automation (`webview_dom_snapshot`, `webview_find_element`, `webview_interact`, `webview_screenshot`, `webview_execute_js`, …) and IPC tools (`ipc_execute_command`, `ipc_monitor`, `ipc_get_captured`, `ipc_get_backend_state`).

Prefer **semantic inspection** over brittle CSS hooks:

- `webview_dom_snapshot` with accessibility tree — locate buttons by visible label text ("Apply", "Variables", "Stream", theme names).
- `webview_find_element` with text or role queries when the snapshot exposes stable names.
- Corroborate UI actions with `ipc_get_captured` / `tw_agent_ctl status` where applicable.

## Journey 1 — GUI offline (daemon stopped)

**Goal**: layout browse, local preview, offline Apply persists to config.

1. Start launcher with `TW_AGENT_MODE=offline` (no daemon PID file).
2. `tw_agent_ctl status` should fail (no daemon on the private bus).
3. Snapshot DOM — confirm status shows offline/daemon unavailable wording.
4. Select a built-in layout (e.g. neon-dash-v2) via sidebar/list interaction.
5. Snapshot preview region — image/canvas should update (not a blank error tile).
6. Open **Variables**, change a text/color var, click **Apply**.
7. Read `$TW_AGENT_HOME/.config/thermalwriter/config.toml` — confirm `[layout_vars]` entry.
8. Restart GUI (kill tracked `TW_GUI_PID`, relaunch inside session) — values reload from disk.

**Pass criteria**: preview renders; config file updated; no daemon required; live user daemon untouched.

## Journey 2 — GUI online (null-transport daemon)

**Goal**: Live Apply reaches D-Bus; status reflects daemon.

1. Start launcher with default `TW_AGENT_MODE` (daemon + GUI in session).
2. GUI status should show online/connected semantics.
3. Change layout from GUI — verify `tw_agent_ctl status` reports new `active_layout`.
4. Toggle a variable and **Apply** — confirm both config file and daemon state.

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

**Requires**: `Xvfb` on PATH, isolated daemon running, at least one stream binary resolved (e.g. `cava`).

1. Open **Stream** tab.
2. Confirm unavailable presets are disabled (daemon `resolve_binaries` map).
3. Start **cava** (or conky with wrapper config) at modest FPS.
4. Poll stream preview `<img>` — JPEG bytes should update (~3 fps); preview may be rotated 180° in CSS (expected).
5. `tw_agent_ctl status` — `mode` should reflect xvfb/stream.
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

The `trap tw_agent_cleanup EXIT` kills only **tracked** PIDs (`TW_SESSION_PID`, `TW_DAEMON_PID`, `TW_GUI_PID`) and removes `$TW_AGENT_HOME`. It does **not** call `thermalwriter ctl stop` on the live bus and does **not** use broad `pkill`.

If a live daemon was healthy before tests (`TW_LIVE_DAEMON_OK=1`), verify it is still healthy after cleanup:

```sh
thermalwriter ctl status
```

## Result report template

```markdown
## Agent test run — Thermalwriter Config

- **Date**:
- **Branch/tag**:
- **GUI command**: `npm run tauri:dev` (inside `dbus-run-session`)
- **MCP host:port**: 127.0.0.1:9223
- **Isolated XDG_CONFIG_HOME**: yes (`$TW_AGENT_HOME/.config`)
- **Private D-Bus**: yes (`dbus-run-session`)
- **Daemon**: null-transport / offline

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
