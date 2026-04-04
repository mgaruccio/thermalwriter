---
date: 2026-04-04
topic: tauri-gui
---

# thermalwriter GUI — Layout Configuration Tool

## Refinement Summary

**Refined on:** 2026-04-04
**Research agents used:** 4 (best-practices, framework-docs, repo-research, learnings)
**Review agents used:** 5 (architecture, security, performance, patterns, simplicity)
**Adversarial review:** Completed (Gemini 3.1 Pro)

### Key Improvements
1. Binary IPC: raw RGBA pixels via `ipc::Response` + canvas `putImageData()` — no PNG encode/decode
2. Feature-gate `rusb`/`memmap2` behind `daemon` feature — GUI build won't need `libusb-dev`
3. Simplified: no new D-Bus methods, no signal forwarding, no custom ColorPicker, text-based layout picker
4. Multi-line frontmatter parser noted as a real restructure, not just a new dispatch arm
5. Path traversal validation and variable value sanitization added as prerequisites
6. Theme/layout_vars precedence defined: frontmatter defaults → global theme → per-layout overrides

### Escalations Resolved
- D-Bus methods vs direct file I/O: **Keep separation** — new D-Bus methods stay
- Custom ColorPicker vs native: **Use native** `<input type="color">` (GTK dialog)
- Thumbnails vs text list: **Text list** for v1, live preview on selection
- D-Bus signal forwarding: **Removed** — not needed for a config tool
- PNG vs raw RGBA: **Raw RGBA** — local IPC has no bandwidth constraint

## What We're Building

A Tauri v2 + Svelte 5 GUI for configuring the thermalwriter cooler LCD display. The GUI reads variable declarations from SVG layout frontmatter, generates a configuration form (color pickers, text inputs, sensor dropdowns), and shows a live 480x480 preview that matches the real display pixel-for-pixel. Users pick a layout, tweak its variables, and apply — the daemon picks up the changes.

This is a **configuration tool**, not a persistent app. It talks to the running daemon over D-Bus for sensor lists and applying changes, but can preview layouts standalone using mock sensor data.

## Why This Approach

**Considered:**

- **egui native GUI** — 100% Rust, no web dependencies. Rejected because egui's styling is functional but not attractive, and the project prioritizes a gaming aesthetic. Color pickers and rich form controls are also weaker in egui.
- **Tauri + vanilla web** — No framework overhead. Rejected because a reactive framework like Svelte makes the dynamic form generation and live preview significantly cleaner.
- **Tauri + React/Solid** — Rich ecosystem. Rejected as heavier than needed — Svelte compiles away, has excellent Tauri integration, and is sufficient for what's ultimately a form + canvas.
- **SvelteKit** — Full framework with routing, SSR. Rejected because this is a single-view app with no routing needs. SvelteKit requires disabling SSR, configuring adapter-static, and adds unnecessary complexity.

**Chosen: Tauri v2 + Svelte 5 (plain, with Vite).** Shares the Rust rendering pipeline (SvgRenderer) for pixel-perfect preview, Svelte 5 runes (`$state`, `$derived`) make reactive form ↔ preview sync trivial, Vite provides fast HMR during development.

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
│  │  │  <canvas>)  │  │  from vars)    │  │   │
│  │  └──────┬─────┘  └───────┬────────┘  │   │
│  └─────────┼────────────────┼───────────┘   │
│            │ invoke()       │ invoke()       │
│  ┌─────────▼────────────────▼───────────┐   │
│  │  Rust Backend (Tauri commands)        │   │
│  │  - render_preview(layout, vars)       │   │
│  │  - list_layouts()                     │   │
│  │  - get_layout_vars(name)              │   │
│  │  - list_sensors()                     │   │
│  │  - save_config(layout, vars)          │   │
│  │  - apply_to_daemon(layout, vars)      │   │
│  └──────────────────────────────────────┘   │
│       │                           │          │
│       │ SvgRenderer              │ D-Bus    │
│       │ (resvg, tera)            │ (zbus)   │
│       ▼                          ▼          │
│   Raw RGBA frame          thermalwriter     │
│                           daemon            │
└─────────────────────────────────────────────┘
```

## Key Decisions

- **Variable schema in SVG frontmatter**: Layouts declare their configurable variables inline using `{# vars: ... #}`. Single source of truth — the schema travels with the layout file. No sidecar files. **Note:** The existing `LayoutFrontmatter` parser is line-by-line; the multi-line `vars:` block requires restructuring the parser to accumulate lines between `{#` and `#}` delimiters. This is a real parser change, not just a new dispatch arm.
- **Three variable types**: `color` (native GTK color picker via `<input type="color">`), `text` (text input), `sensor` (dropdown populated from daemon's sensor list). Covers cosmetic tweaks and data binding. More types added later as layouts need them.
- **Reuse SvgRenderer for preview**: The Tauri Rust backend calls the same `SvgRenderer` the daemon uses. Confirmed standalone-capable — `examples/preview_layout.rs` already renders without the daemon, USB, or tokio runtime.
- **Raw RGBA pixels for preview**: Use `tauri::ipc::Response` to return the raw RGBA pixmap bytes (~900KB at 480x480). Frontend paints with `<canvas>` + `putImageData()`. No PNG encoding/decoding overhead — local IPC has no bandwidth constraint.
- **Config persistence in config.toml**: Variable overrides stored in `[layout_vars."layout-name"]` sections. Use `toml_edit` (not serde serialization) to preserve user comments. Atomic writes (write to temp file, then rename).
- **Svelte 5 with Vite**: Plain Svelte 5 (not SvelteKit). Runes API (`$state`, `$derived`) for reactive form state. Vite for dev server and bundling.
- **Workspace structure**: The thermalwriter crate is already lib+bin. The Tauri app depends on `thermalwriter = { path = "../..", default-features = false }`. Use `default-members = ["."]` so `cargo build` from root only builds the daemon.
- **Feature-gate daemon dependencies**: Gate `rusb` and `memmap2` behind a `daemon` feature (default-enabled). The GUI build uses `default-features = false` to avoid requiring `libusb-dev`.
- **D-Bus connection lifecycle**: Single `zbus::Connection` created in Tauri `setup()`. The `zbus::Proxy` is `Clone + Send` — store directly as `tauri::State<DisplayProxy>`, no Mutex needed. Use `std::sync::Mutex` for the `SvgRenderer` (synchronous CPU work — never hold across `.await`).
- **Error handling**: `thiserror` errors with manual `serde::Serialize` impl (serialize as string). Frontend receives error text in `invoke()` catch.
- **Theme/layout_vars precedence**: Merge order is (1) frontmatter defaults, (2) `[theme]` global palette, (3) `[layout_vars."name"]` per-layout overrides. The GUI writes to `[layout_vars]` only.

## Variable Schema Format

In SVG frontmatter (alongside existing `history:` and `animation:` directives):

```
{# vars:
   theme_primary: color = "#00ff88" "Primary accent color"
   theme_secondary: color = "#ff6b9d" "Secondary accent color"
   theme_background: color = "#0a0a14" "Background color"
   theme_text_dim: color = "#666680" "Dim label color"
   theme_critical: color = "#ff4444" "Critical threshold color"
   theme_warning: color = "#ffaa00" "Warning threshold color"
   cpu_label: text = "CPU" "Label for CPU panel"
   gpu_label: text = "GPU" "Label for GPU panel"
   top_sensor: sensor = "cpu_temp" "Main metric for top panel"
   bottom_sensor: sensor = "gpu_temp" "Main metric for bottom panel"
#}
```

Format per line: `name: type = "default" "help text"`

All default values are quoted for unambiguous parsing. Variable names must match `[a-z_][a-z0-9_]*`. Color defaults must match `^#[0-9a-fA-F]{6,8}$`. Text defaults must not contain Tera delimiters (`{{`, `}}`, `{%`, `%}`).

Parsed by extending `LayoutFrontmatter` in `src/render/frontmatter.rs`. The parser must be restructured from single-line to multi-line block handling (accumulate lines between `{#` and `#}` delimiters, then dispatch on the directive prefix).

**Sensor fallback**: If a layout's default sensor key is not available on the current system, the dropdown shows the first available sensor with a warning indicator.

## D-Bus Extensions

Two new methods on `com.thermalwriter.Display`:

- **`GetLayoutVars(name: String) -> Vec<Dict<String, String>>`** — returns a list of dicts with keys `name`, `type`, `default`, `help` for each variable. Dict format is extensible (vs positional tuples).
- **`SetLayoutVars(name: String, vars: Dict<String, String>)`** — applies variable overrides in-memory to the running daemon (via `ModeChange` channel), persists to config.toml as a side-effect using `toml_edit`.

**Prerequisites (existing bugs to fix first):**
- Wire up the `ListSensors` placeholder to return `Vec<SensorDescriptor>` (key, name, unit) — not just `Vec<String>`. Snapshot `sensor_hub.available_sensors()` into `ServiceState` at startup.
- Fix `list_layouts` to include `.svg` files and recurse into the `svg/` subdirectory (currently only returns `.html`).
- Add path traversal validation to `set_layout` and `set_mode`: canonicalize the resolved path and verify it starts with the layout directory prefix. Reject names containing `..`.

### D-Bus Client Pattern in Tauri

The GUI acts as a D-Bus client. Key implementation details:

- Create a single `zbus::Connection::session()` in Tauri's `setup()` hook
- Store the proxy as `tauri::State<DisplayProxy>` — zbus proxies are `Clone + Send` and internally reference-counted, no Mutex needed
- Use `std::sync::Mutex<SvgRenderer>` for the renderer (synchronous CPU work). Use `spawn_blocking` in the render command to avoid blocking the async runtime.
- If the daemon is not running, the GUI still works — preview renders locally with mock sensor data, D-Bus connection errors are handled gracefully with a "daemon not running — changes saved but not applied" message

## User Flow

1. Open GUI → layout list with names and descriptions (parsed from frontmatter help text)
2. Select layout → config form populates with layout's declared variables, preview shows current rendering
3. Change a variable → preview re-renders live (~100ms debounce for color picker dragging)
4. Click "Apply" → saves to config.toml, tells daemon to reload with new values
5. Close GUI → daemon continues with applied settings

## Preview Rendering

Tauri command `render_preview` calls `SvgRenderer` with:
- Template variables from the user's current form state (variable overrides merged into the template context)
- Sensor data: mock values (reusing existing `--mock` pattern) when daemon is not running, or live sensor snapshot from daemon via D-Bus when available

Returns raw RGBA pixel bytes (~900KB) via `tauri::ipc::Response`. Frontend receives `ArrayBuffer`, paints onto a `<canvas>`:

```typescript
const buffer = await invoke('render_preview', { layout, vars });
const imageData = new ImageData(new Uint8ClampedArray(buffer), 480, 480);
ctx.putImageData(imageData, 0, 0);
```

Preview re-renders on variable change with ~100ms debounce. No encode/decode overhead.

**Note on pixel format**: `SvgRenderer` produces a `tiny_skia::Pixmap` (premultiplied RGBA). For the GUI, use `pixmap.data()` directly — `putImageData` expects premultiplied RGBA, which is what tiny_skia provides. Skip the `RawFrame::from_pixmap()` unpremultiply conversion entirely.

### SvgRenderer State

- Store as `tauri::State<std::sync::Mutex<SvgRenderer<'static>>>`. Reuse across invocations.
- When user selects a new layout, create a fresh `SvgRenderer` instance (not just `set_template`) to ensure usvg options and fontdb are correct for the new template.
- For layout list rendering, reuse a single `SvgRenderer` and cycle through layouts via `set_template()` to avoid repeated `load_system_fonts()` calls (~50-200ms each).

## Project Structure

```
thermalwriter/              (workspace root — new Cargo.toml wraps existing)
├── Cargo.toml              (workspace: members = [".", "gui/src-tauri"],
│                            default-members = ["."])
├── src/                    (daemon + library crate — existing, unchanged)
│   ├── lib.rs              (exports: transport, sensor, render, service, config, theme)
│   └── main.rs             (daemon entry point)
├── gui/
│   ├── src-tauri/
│   │   ├── Cargo.toml      (depends on thermalwriter = { path = "../..",
│   │   │                     default-features = false })
│   │   └── src/
│   │       ├── lib.rs       (tauri::Builder setup, command registration)
│   │       └── main.rs      (entry point — calls lib::run())
│   ├── src/                 (svelte frontend)
│   │   ├── App.svelte
│   │   ├── lib/
│   │   │   ├── LayoutList.svelte
│   │   │   ├── ConfigForm.svelte
│   │   │   └── Preview.svelte
│   │   └── main.ts
│   ├── package.json
│   ├── vite.config.ts
│   └── tauri.conf.json
└── layouts/
```

## Security Considerations

- **Path traversal**: All layout name resolution must canonicalize the path and verify it starts with the layout directory prefix. Reject names containing `..`. Apply in both D-Bus methods and Tauri commands.
- **Variable value validation**: Enforce type-specific constraints in the Tauri backend before storing or rendering. Color: `^#[0-9a-fA-F]{6,8}$`. Text: reject Tera delimiters (`{{`, `}}`, `{%`, `%}`). Don't rely on frontend validation alone.
- **Tauri capabilities**: Use explicit command allowlist in `tauri.conf.json`. Don't enable `fs`, `shell`, or `process` plugins. Keep CSP restrictive: `default-src 'self'; img-src 'self' blob:`.
- **Config atomicity**: Write to temp file, then `rename()` for atomic updates. Use `toml_edit` to preserve user comments.

## Scope

**In scope:**
- Layout list with names/descriptions
- Dynamic config form (color, text, sensor types)
- Live 480x480 canvas preview
- Save variable overrides to config.toml
- Apply changes to running daemon via D-Bus
- Frontmatter variable schema parsing
- Feature-gate daemon-only dependencies

**Out of scope (future):**
- Visual drag-and-drop layout editor
- Creating new layouts from the GUI
- Daemon lifecycle management (start/stop)
- Sensor history graphs in the GUI
- Additional variable types (number, boolean, enum)
- Thumbnail rendering for layout picker
- D-Bus signal forwarding to frontend

## Next Steps

→ writing-plans skill for implementation details
