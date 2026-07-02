# Design Review & Resolution Plan

Date: 2026-07-01
Status: Resolved

This document outlines the findings of a design and usability review for the `thermalwriter` layouts, rendering pipeline, and Svelte/Tauri GUI, and details a structured plan to resolve the identified issues.

---

## 1. Design & Appearance Findings (Layouts & Renderer)

### A. Transparent Backgrounds Rendering as Solid Black (#000000)
* **Problem:** SVG layouts (such as `arc-gauge.svg`, `cyber-grid.svg`, `neon-dash.svg`, and `neon-dash-v2.svg`) do not declare default background rectangles. In `src/render/mod.rs`, transparent pixels default to solid black (`#000000`) during raw RGB frame conversion. The layout guide (`SKILL.md`) states page backgrounds must never be pure black because they look washed out and low-contrast on hardware LCDs. Furthermore, the customizable layout variable `theme_background` is defined in SVG frontmatters but has no visual effect because there is no background element referencing it.
* **Decision:** Update `SvgRenderer` to clear the pixmap with the resolved theme background color when no background image is set.
* **Check:** Verify that transparent SVG templates render with a themed near-black background (e.g. `#08080f` or `#0a0a14`) and respond to custom background colors applied via layout variables or theme updates.
* **Resolution:** Implemented in `src/render/svg.rs`; transparent SVG pixels now clear to the resolved background color when no image background is configured, and regression tests cover fallback, theme, frontmatter default, and user override resolution.

### B. Font Family Discrepancy (DejaVu Sans Mono vs JetBrains Mono)
* **Problem:** `SKILL.md` instructs layout designers to use "JetBrains Mono" as the single monospace font. However, `src/render/svg.rs` embeds `JetBrainsMono-Regular.ttf` under the family name `"DejaVu Sans Mono"`. Because `usvg` matches font-family properties strictly by metadata, all SVG layouts are forced to use `font-family="DejaVu Sans Mono"` to resolve correctly.
* **Decision:** Retain the current font-database mapping in Rust (to avoid asset changes), but document this discrepancy in `SKILL.md` so layout authors target `"DejaVu Sans Mono"` instead of `"JetBrains Mono"`.
* **Check:** Confirm SVG text continues to render correctly using the embedded font without warnings.
* **Resolution:** Documented in `skills/designing-layouts/SKILL.md`; the renderer font mapping remains unchanged.

### C. Style Violations in HTML Layouts
* **Problem:** Legacy HTML layouts violate core layout guidelines:
  * **`gpu-focus.html`:** Lacks explicit `height` on every text/variable element, causing them to collapse to `0px` and overlap in the taffy rendering engine. It also uses low-contrast text colors (`#666666`, `#0f3460`, `#533483`) that are below the recommended `#888888` minimum threshold.
  * **`minimal.html`:** Lacks explicit heights, uses a prohibited pure black background (`#000000`), and uses low-contrast text colors (`#444444`, `#333333`).
  * **`fps-hero.html`:** Uses low-contrast labels (`#444444`), rendering them nearly invisible on LCD hardware.
* **Decision:** Add explicit heights (rule of thumb: `height ≈ font-size × 1.2`) to all text tags and adjust backgrounds and text colors to guidelines-compliant colors.
* **Check:** Run `preview_layout` to ensure elements do not overlap and colors are visible.
* **Resolution:** Updated `layouts/gpu-focus.html`, `layouts/minimal.html`, and `layouts/fps-hero.html`; rendered and visually inspected all three PNG previews for legible colors and non-overlapping text.

---

## 2. Usability Findings (Svelte/Tauri GUI)

### A. WYSIWYG Background Gallery Preview Gap
* **Problem:** In Svelte GUI `App.svelte`, selecting a background image in the gallery updates the local state `selectedBackground` but does not trigger a preview update. Furthermore, the `render_preview` Tauri command does not accept the background image parameter, so the live preview in the GUI is rendered without a background. The background choice only becomes visible after clicking "Apply".
* **Decision:** Extend the `render_preview` Tauri command to accept an optional `background` parameter, load the background pixmap, and set it on the renderer. Make Svelte's reactive effect track `selectedBackground` and forward it.
* **Check:** Confirm that clicking a background tile in the gallery immediately updates the canvas preview to render the layout layered on top of that background image.
* **Resolution:** `render_preview` now accepts `background`, validates/decodes the selected image, clears cached renderer backgrounds on `None`, and `App.svelte` schedules previews when `selectedBackground` changes.

### B. Hardcoded Device Status in Titlebar
* **Problem:** The Svelte GUI titlebar hardcodes device information: `Peerless Vision · 480 × 480 · USB 0x87AD/0x70DB`. If the physical USB cooler is disconnected, the status bar shows `Daemon · Online` based on D-Bus liveness, but the user is not warned that the device is missing. The active layout name is also discarded.
* **Decision:** Store the full `DaemonStatus` struct in the Svelte state. Dynamically update the titlebar with the actual resolution and USB connection status (showing a warning badge if disconnected), and highlight the currently active layout in the sidebar layout list.
* **Check:** Confirm the UI dynamically displays connection changes and highlights active layouts.
* **Resolution:** `App.svelte` now stores `DaemonStatus`, displays the dynamic resolution and USB connection badge, warns on disconnected USB, and marks the daemon-active layout in the sidebar.

---

## 3. Resolution Plan

### Phase 1: Background & Preview Compositing
1. [x] **Extend `render_preview` Command:** Update `gui/src-tauri/src/commands.rs` to accept an optional `background: Option<String>` parameter. If present, load the background pixmap via `validate_background_path` and `set_background` before rendering the preview.
2. [x] **Implement Fallback Clearance in `SvgRenderer`:** Update `src/render/svg.rs` so that if `self.background` is `None` (no image set), it resolves a background color (checking `theme_background` overrides -> default -> `theme.background` -> `#08080f`) and clears the pixmap canvas with it before executing `resvg::render`.
3. [x] **Connect Svelte States:** Modify `gui/src/App.svelte` to include `selectedBackground` in the reactive `$effect` block that schedules previews, and pass `selectedBackground` to the `render_preview` Tauri command invoke call.

### Phase 2: HTML Layout Corrections
1. [x] **Fix `gpu-focus.html`:** Add explicit `height` attributes to all text tags and update colors: `#666666` -> `#888888`, `#0f3460` -> `#53d8fb`, `#533483` -> `#cc9eff`.
2. [x] **Fix `minimal.html`:** Add explicit heights, change background color from `#000000` to `#08080f`, and update text colors to `#ffffff` and `#888888`.
3. [x] **Fix `fps-hero.html`:** Change labels from `#444444` to `#888888`.

### Phase 3: GUI Dynamic Integration & Documentation
1. [x] **Manage DaemonStatus in GUI:** In `gui/src/App.svelte`, define a `daemonStatus` state variable and assign the response of `get_status` to it.
2. [x] **Dynamic UI Indicators:** Update the GUI titlebar to display the resolution and connection state dynamically. If `connected` is false, show a red/orange badge. Highlight the layout name in the sidebar that matches the active layout.
3. [x] **Layout Guidelines Updates:** Add a warning block in `skills/designing-layouts/SKILL.md` explaining the font family name mismatch ("DejaVu Sans Mono" must be declared in SVGs to resolve to the embedded font).

---

## 4. Verification

* `cargo test -p thermalwriter --test render_tests` — passed.
* `cargo test -p thermalwriter-gui test_cached_renderer_background_clearing` — passed.
* `npm run check` in `gui/` — passed with 0 errors and 0 warnings.
* `cargo run --example preview_layout layouts/gpu-focus.html` — rendered `/tmp/thermalwriter_gpu-focus.png`; visually inspected after the width fix, with no overlap.
* `cargo run --example preview_layout layouts/minimal.html` — rendered `/tmp/thermalwriter_minimal.png`; visually inspected, with near-black background and visible text.
* `cargo run --example preview_layout layouts/fps-hero.html` — rendered `/tmp/thermalwriter_fps-hero.png`; visually inspected, with visible labels.
