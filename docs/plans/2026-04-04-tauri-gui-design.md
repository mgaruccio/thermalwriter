---
date: 2026-04-04
topic: tauri-gui
---

# Tauri GUI — Control Panel for thermalwriter

## What We're Building

A Tauri v2 desktop app (Svelte frontend) that serves as a control panel for the thermalwriter daemon. It lets users switch layouts, configure which sensor feeds into each layout slot, adjust settings (tick rate, quality, rotation), and see a live preview of the LCD display — all without touching config files or the CLI.

The GUI is a regular window (not a system tray app). The daemon remains the owner of the display — the GUI is a client that talks to it over D-Bus via Tauri commands.

## Why This Approach

**Considered and rejected:**

- **React / Vue frontend** — Svelte is the most natural Tauri pairing: smallest bundle, best reactivity model, and Mike prefers it.
- **Direct D-Bus from frontend** — would require a JS D-Bus binding or WebSocket bridge. Tauri commands as a proxy layer is simpler, keeps the frontend pure UI, and the Rust backend already has zbus.
- **Separate repo** — the GUI depends on shared types (sensor descriptors, slot definitions) and the D-Bus interface contract. Monorepo with a Cargo workspace keeps everything in sync.
- **System tray app** — the daemon runs independently via systemd. The GUI is for configuration, not monitoring — you open it, make changes, close it.
- **Shared memory for live preview** — overkill for 2 FPS at 30-50KB JPEG frames. D-Bus `GetFrame()` polling is simpler and sufficient.
- **Tera variable parsing for slots** — would work with zero layout changes but gives a poor UX (raw variable names like `cpu_temp` as slot labels). SVG data attributes give layout authors control over slot names and labels.

## Architecture

```
┌──────────────────────────────────────────────┐
│               Tauri App                       │
│                                               │
│  ┌──────────────────────────────────────────┐ │
│  │          Svelte Frontend                  │ │
│  │                                           │ │
│  │  ┌─────────────┐  ┌─────────────────┐    │ │
│  │  │ LayoutPicker│  │  LivePreview     │    │ │
│  │  │ SlotEditor  │  │  (480x480 JPEG)  │    │ │
│  │  │ Settings    │  │                   │    │ │
│  │  └──────┬──────┘  └────────┬─────────┘    │ │
│  │         │                  │               │ │
│  │         └───── invoke() ───┘               │ │
│  └──────────────┬────────────────────────────┘ │
│                 │ Tauri commands                │
│  ┌──────────────▼────────────────────────────┐ │
│  │        Rust Backend (src-tauri)            │ │
│  │   commands.rs → zbus proxy → D-Bus        │ │
│  └──────────────┬────────────────────────────┘ │
└─────────────────┼────────────────────────────┘
                  │ D-Bus (session bus)
┌─────────────────▼────────────────────────────┐
│          thermalwriter daemon                 │
│   com.thermalwriter.Service                   │
│   /com/thermalwriter/display                  │
└───────────────────────────────────────────────┘
```

## Repo Structure

Full restructure into a Cargo workspace:

```
thermalwriter/
├── Cargo.toml              # [workspace] members = ["crates/*", "gui/src-tauri"]
├── crates/
│   ├── daemon/             # existing daemon (moved from src/)
│   │   ├── Cargo.toml
│   │   └── src/
│   └── shared/             # shared types: sensor descriptors, slot definitions
│       ├── Cargo.toml
│       └── src/lib.rs
├── gui/
│   ├── src-tauri/          # Tauri Rust backend
│   │   ├── Cargo.toml      # depends on shared crate + zbus
│   │   └── src/
│   │       ├── main.rs
│   │       └── commands.rs
│   ├── src/                # Svelte frontend
│   │   ├── App.svelte
│   │   ├── lib/
│   │   │   ├── LayoutPicker.svelte
│   │   │   ├── SlotEditor.svelte
│   │   │   ├── LivePreview.svelte
│   │   │   └── Settings.svelte
│   │   └── main.ts
│   ├── package.json
│   ├── svelte.config.js
│   ├── vite.config.ts
│   └── tauri.conf.json
├── layouts/                # SVG/HTML layout templates (unchanged)
├── examples/               # preview_layout, render_layout, etc.
├── skills/
└── docs/
```

## Slot System — SVG Data Attributes

Layouts declare configurable metric slots via data attributes on SVG elements:

```svg
<text x="120" y="80"
      data-slot="primary-temp"
      data-slot-label="Primary Temperature"
      data-slot-default="cpu_temp"
      data-slot-format="{value}°C">
  {{ primary_temp }}°C
</text>
```

**Attributes:**

| Attribute | Required | Purpose |
|-----------|----------|---------|
| `data-slot` | yes | Unique slot ID within the layout |
| `data-slot-label` | yes | Human-readable name shown in the GUI |
| `data-slot-default` | yes | Sensor key used when no user binding exists |
| `data-slot-format` | no | Display format (e.g., `{value}°C`, `{value}W`) |

**Binding storage** — per-layout TOML in config directory:

```toml
# ~/.config/thermalwriter/bindings/neon-dash-v2.toml
[slots]
primary-temp = "cpu_temp"
secondary-temp = "gpu_temp"
power-display = "cpu_power"
```

**Resolution flow in the daemon:**

1. Parse SVG for `data-slot` attributes → build slot map: `slot_id → (variable_name, default_sensor)`
2. Load bindings file (if it exists); fall back to `data-slot-default` for unbound slots
3. Build Tera context by mapping each slot's variable name to its bound sensor's current value
4. Tera renders `{{ primary_temp }}` → actual sensor reading

Elements without `data-slot` attributes continue to work as before — they use the literal sensor key as the Tera variable name.

## D-Bus Extensions

New methods added to `com.thermalwriter.Display`:

| Method | Signature | Purpose |
|--------|-----------|---------|
| `GetFrame()` | `→ ay` (byte array) | Returns current JPEG frame (~30-50KB) |
| `GetLayoutSlots(name)` | `s → a(sss)` | Returns `(slot_id, label, current_sensor)` tuples |
| `SetSlotBinding(layout, slot, sensor)` | `sss → ()` | Bind a slot to a sensor; writes to bindings TOML |
| `GetSensorList()` | `→ a(sss)` | Returns `(key, value, provider)` for all sensors |

All additive — no changes to existing D-Bus interface.

The daemon stashes the latest JPEG frame in an `Arc<Mutex<Vec<u8>>>` after each tick. `GetFrame()` clones and returns it.

## Tauri Commands

Thin proxy layer — each command maps 1:1 to a D-Bus call:

```rust
#[tauri::command] async fn get_status() -> Result<HashMap<String, String>, String>;
#[tauri::command] async fn set_layout(name: String) -> Result<String, String>;
#[tauri::command] async fn get_frame() -> Result<Vec<u8>, String>;
#[tauri::command] async fn get_layout_slots(name: String) -> Result<Vec<SlotInfo>, String>;
#[tauri::command] async fn set_slot_binding(layout: String, slot: String, sensor: String) -> Result<(), String>;
#[tauri::command] async fn list_sensors() -> Result<Vec<SensorInfo>, String>;
#[tauri::command] async fn list_layouts() -> Result<Vec<String>, String>;
#[tauri::command] async fn set_tick_rate(rate: u32) -> Result<(), String>;
#[tauri::command] async fn set_jpeg_quality(quality: u8) -> Result<(), String>;
#[tauri::command] async fn set_rotation(degrees: u32) -> Result<(), String>;
```

The Tauri backend creates one `zbus` proxy connection on startup and reuses it. If the daemon isn't running, commands return user-friendly errors.

## Svelte Frontend

### Layout

Sidebar (left) + main area (right):

```
┌──────────────────┬──────────────────────────────┐
│  Layout Picker   │                              │
│  ┌────────────┐  │      Live Preview            │
│  │ neon-dash  │  │      ┌──────────────┐        │
│  │ arc-gauge  │  │      │  480x480     │        │
│  │ cyber-grid │  │      │  JPEG frame  │        │
│  │ fps-hero   │  │      │              │        │
│  └────────────┘  │      └──────────────┘        │
│                  │                              │
│  Slot Editor     │  Settings                    │
│  ┌────────────┐  │  ┌──────────────────────┐    │
│  │ Primary    │  │  │ Tick rate: [2] FPS   │    │
│  │ Temp: [▼]  │  │  │ Quality:  [85]      │    │
│  │ Secondary  │  │  │ Rotation: [180°]    │    │
│  │ Temp: [▼]  │  │  └──────────────────────┘    │
│  └────────────┘  │                              │
└──────────────────┴──────────────────────────────┘
```

### Components

1. **LayoutPicker** — lists available layouts. Click to switch via `set_layout()`. Active layout highlighted. Could show thumbnail previews later.

2. **SlotEditor** — appears when a layout is selected. Shows each slot's label with a dropdown of available sensors (from `list_sensors()`). Changes call `set_slot_binding()` and take effect on the next tick.

3. **LivePreview** — polls `get_frame()` at the daemon's tick rate. Renders the JPEG bytes into an `<img>` via a blob URL or base64 data URI. Displayed at 480x480 (1:1) or scaled to fit.

4. **Settings** — tick rate slider (1-30), JPEG quality slider (50-100), rotation dropdown (0/90/180/270). Each change calls the corresponding Tauri command immediately.

### Styling

Tokyo Night color palette to match Mike's desktop aesthetic. Dark background, muted text, accent colors from the theme.

## Key Decisions

- **Tauri v2 + Svelte**: lightweight, Rust-native backend, reactive frontend with minimal bundle
- **D-Bus proxy via Tauri commands**: clean separation, reuses existing daemon IPC, frontend stays pure UI
- **SVG data attributes for slots**: layout authors control the UX, descriptive labels, graceful fallback for unattributed elements
- **Per-layout binding files**: slot→sensor mappings stored as TOML, easy to edit by hand if needed
- **Polling for live preview**: simple, sufficient at 2-4 FPS, no shared memory complexity
- **Full workspace restructure**: daemon → `crates/daemon/`, shared types → `crates/shared/`, GUI → `gui/`

## External Prerequisites

- **Tauri v2 CLI**: `cargo install tauri-cli` — needed for `cargo tauri dev` / `cargo tauri build`
- **Node.js / npm**: required for the Svelte frontend build tooling
- **WebKitGTK**: Tauri's webview on Linux — likely already installed on Mike's desktop
- **No new credentials or API keys needed** — everything is local D-Bus

## Open Questions

- **Layout thumbnails in the picker**: should we render a static preview of each layout (via the daemon) or just show names? Could defer to v2.
- **Hot-reload in the GUI**: when the daemon detects a layout file change, should it notify the GUI to refresh the slot editor?
- **Error states**: what should the GUI show when the daemon isn't running? A "daemon offline" banner with a "start" button that runs `systemctl --user start thermalwriter`?

## Next Steps

→ writing-plans skill for implementation details
