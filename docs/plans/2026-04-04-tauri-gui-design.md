---
date: 2026-04-04
topic: tauri-gui
---

# thermalwriter GUI — Layout Configuration Tool

## What We're Building

A Tauri v2 + Svelte GUI for configuring the thermalwriter cooler LCD display. The GUI reads variable declarations from SVG layout frontmatter, generates a configuration form (color pickers, text inputs, sensor dropdowns), and shows a live 480x480 preview that matches the real display pixel-for-pixel. Users pick a layout, tweak its variables, and apply — the daemon picks up the changes.

This is a **configuration tool**, not a persistent app. It talks to the running daemon over D-Bus for sensor lists and applying changes, but can preview layouts standalone using mock sensor data.

## Why This Approach

**Considered:**

- **egui native GUI** — 100% Rust, no web dependencies. Rejected because egui's styling is functional but not attractive, and the project prioritizes a gaming aesthetic. Color pickers and rich form controls are also weaker in egui.
- **Tauri + vanilla web** — No framework overhead. Rejected because a reactive framework like Svelte makes the dynamic form generation and live preview significantly cleaner.
- **Tauri + React/Solid** — Rich ecosystem. Rejected as heavier than needed — Svelte compiles away, has excellent Tauri integration, and is sufficient for what's ultimately a form + canvas.

**Chosen: Tauri v2 + Svelte.** Shares the Rust rendering pipeline (SvgRenderer) for pixel-perfect preview, Svelte compiles away framework overhead, Tauri scaffolds Svelte projects natively.

## Architecture

```
┌─────────────────────────────────────────────┐
│            Tauri App                         │
│                                              │
│  ┌──────────────────────────────────────┐   │
│  │  Svelte Frontend                      │   │
│  │  ┌────────────┐  ┌────────────────┐  │   │
│  │  │ Preview     │  │ Config Form    │  │   │
│  │  │ (480x480    │  │ (generated     │  │   │
│  │  │  canvas)    │  │  from vars)    │  │   │
│  │  └──────┬─────┘  └───────┬────────┘  │   │
│  └─────────┼────────────────┼───────────┘   │
│            │ invoke()       │ invoke()       │
│  ┌─────────▼────────────────▼───────────┐   │
│  │  Rust Backend (Tauri commands)        │   │
│  │  - render_preview(vars) → PNG         │   │
│  │  - list_layouts() → Vec<LayoutInfo>   │   │
│  │  - get_layout_vars(name) → Vec<Var>   │   │
│  │  - list_sensors() → Vec<String>       │   │
│  │  - save_config(layout, vars)          │   │
│  │  - apply_to_daemon(layout, vars)      │   │
│  └──────────────────────────────────────┘   │
│       │                           │          │
│       │ SvgRenderer              │ D-Bus    │
│       │ (resvg, tera)            │ (zbus)   │
│       ▼                          ▼          │
│   Preview frame           thermalwriter     │
│                           daemon            │
└─────────────────────────────────────────────┘
```

## Key Decisions

- **Variable schema in SVG frontmatter**: Layouts declare their configurable variables inline using `{# vars: ... #}`. Single source of truth — the schema travels with the layout file. No sidecar files.
- **Three variable types**: `color` (color picker), `text` (text input), `sensor` (dropdown of available sensors from the daemon). Covers cosmetic tweaks and data binding. More types added later as layouts need them.
- **Reuse SvgRenderer for preview**: The Tauri Rust backend calls the same `SvgRenderer` the daemon uses. Preview is pixel-for-pixel identical to the real display.
- **Config persistence in config.toml**: Variable overrides stored in `[layout_vars."layout-name"]` sections. Daemon reads these on startup; GUI reads/writes them via Tauri commands.
- **Svelte frontend**: Compiles away, reactive bindings make form ↔ preview sync trivial, first-class Tauri scaffolding support.
- **Workspace structure**: GUI lives in `gui/` as a separate Tauri crate within the Cargo workspace, depending on the thermalwriter library for rendering code.

## Variable Schema Format

In SVG frontmatter (alongside existing `history:` and `animation:` directives):

```
{# vars:
   theme_primary: color = #00ff88 "Primary accent color"
   theme_secondary: color = #ff6b9d "Secondary accent color"
   theme_background: color = #0a0a14 "Background color"
   theme_text_dim: color = #666680 "Dim label color"
   theme_critical: color = #ff4444 "Critical threshold color"
   theme_warning: color = #ffaa00 "Warning threshold color"
   cpu_label: text = "CPU" "Label for CPU panel"
   gpu_label: text = "GPU" "Label for GPU panel"
   top_sensor: sensor = cpu_temp "Main metric for top panel"
   bottom_sensor: sensor = gpu_temp "Main metric for bottom panel"
#}
```

Format per line: `name: type = default "help text"`

Parsed by extending the existing `LayoutFrontmatter` in `src/render/frontmatter.rs`.

## D-Bus Extensions

Two new methods on `com.thermalwriter.Display`:

- **`GetLayoutVars(name: String) -> Vec<(String, String, String, String)>`** — returns `(name, type, default, help_text)` tuples for the named layout
- **`SetLayoutVars(name: String, vars: Dict<String, String>)`** — applies variable overrides, persists to config, triggers re-render

Wire up the existing `ListSensors` placeholder to return actual sensor keys.

## User Flow

1. Open GUI → layout picker grid (thumbnails of all available layouts)
2. Select layout → config form populates with layout's declared variables, preview shows current rendering
3. Change a variable → preview re-renders live (~100ms debounce for color picker dragging)
4. Click "Apply" → saves to config.toml, tells daemon to reload with new values
5. Close GUI → daemon continues with applied settings

## Preview Rendering

Tauri command `render_preview` calls `SvgRenderer` with:
- Template variables from the user's current form state
- Sensor data: mock values (reusing existing `--mock` pattern) when daemon is not running, or live sensor snapshot from daemon via D-Bus when available

Returns base64 PNG data URL. Frontend displays in `<img>` tag.

## Project Structure

```
thermalwriter/              (workspace root)
├── Cargo.toml              (workspace members: ".", "gui/src-tauri")
├── src/                    (daemon + library crate)
├── gui/
│   ├── src-tauri/
│   │   ├── Cargo.toml      (tauri app, depends on thermalwriter lib)
│   │   └── src/
│   │       └── main.rs     (tauri commands)
│   ├── src/                (svelte frontend)
│   │   ├── App.svelte
│   │   ├── lib/
│   │   │   ├── LayoutPicker.svelte
│   │   │   ├── ConfigForm.svelte
│   │   │   ├── Preview.svelte
│   │   │   └── ColorPicker.svelte
│   │   └── main.ts
│   ├── package.json
│   └── vite.config.ts
└── layouts/
```

## Scope

**In scope:**
- Layout picker with thumbnail grid
- Dynamic config form (color, text, sensor types)
- Live 480x480 preview
- Save variable overrides to config.toml
- Apply changes to running daemon via D-Bus
- Frontmatter variable schema parsing

**Out of scope (future):**
- Visual drag-and-drop layout editor
- Creating new layouts from the GUI
- Daemon lifecycle management (start/stop)
- Sensor history graphs in the GUI
- Additional variable types (number, boolean, enum)

## Open Questions

- Should the GUI be installable via the same `cargo install` as the daemon, or packaged separately (AppImage, .deb)?
- Thumbnail generation for the layout picker: render on-demand or cache at build time?

## Next Steps

→ writing-plans skill for implementation details
