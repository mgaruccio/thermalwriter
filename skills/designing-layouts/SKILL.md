---
name: designing-layouts
description: Use when creating, modifying, or reviewing thermalwriter LCD layouts. Also use when extending the rendering engine with new CSS properties, image support, or sensor providers.
---

# Designing Layouts for Thermalwriter

## Overview

Create attractive layouts for thermalwriter's LCD display. The display is a small square screen (currently 480x480) mounted on a CPU cooler inside a PC case. The target aesthetic is **gaming/enthusiast PC** — bold, dark, accented. Think NZXT CAM, Corsair iCUE, not a terminal dashboard.

**Attractive is more important than informational.** This is a consumer product, not a monitoring tool.

## The Core Discipline: Render and Look

The #1 failure mode is not looking at output. Every layout change MUST be rendered and visually inspected.

```bash
# Preview to PNG (fast iteration, no USB):
cargo run --example preview_layout <name_or_path>
# Output: /tmp/thermalwriter_<name>.png

# Push to hardware (final review):
systemctl --user stop thermalwriter
cargo run --example render_layout <name_or_path> [seconds] [--mock]
systemctl --user start thermalwriter
```

Both accept: file path (`layouts/my.html`), short name (`neon-dash`), or built-in name (`system-stats`).

Use `--mock` to inject realistic gaming-load data (144 FPS, 67°C CPU, 71°C GPU, 285W) when testing layouts that depend on sensors not currently active.

**Read the PNG after every render.** Unreviewed output is unknown output.

## Critical Engine Constraint: Explicit Heights

The rendering engine (taffy) cannot measure text. Every element that contains text MUST have an explicit `height` or it collapses to 0px and overlaps with siblings.

```html
<!-- BAD: span has no height, will overlap with siblings -->
<span style="font-size: 24px; color: #e94560;">72°C</span>

<!-- GOOD: height slightly larger than font-size -->
<span style="height: 30px; font-size: 24px; color: #e94560;">72°C</span>
```

Rule of thumb: `height ≈ font-size × 1.2`

This is the single most common layout bug. If text overlaps, check for missing heights.

## SVG Layouts (Primary Format)

SVG is the primary layout format. Use the `SvgRenderer` pipeline: SVG template → Tera substitution → resvg → Pixmap.

```xml
{# history: cpu_util=60s, gpu_temp=120s #}
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 480 480">
  <!-- Background pattern -->
  {{ background(pattern="grid", color="#ffffff08", spacing=24) }}

  <!-- Area graph: CPU utilization history -->
  {{ graph(data=cpu_util_history, x=0, y=380, w=480, h=100,
           style="area", fill="#e9456033", stroke="#e94560") }}

  <!-- Current CPU temp (hero value) -->
  <text x="240" y="200" text-anchor="middle" font-size="96"
        fill="{{ theme_primary }}" font-family="monospace">
    {{ cpu_temp | default(value="--") }}°C
  </text>
</svg>
```

Key SVG layout rules:
1. `viewBox="0 0 480 480"` — always 480x480 canvas
2. `{# history: ... #}` frontmatter declares metrics to buffer (see [components.md](./references/components.md))
3. Text uses absolute `x,y` coordinates — no flexbox in SVG
4. Components are Tera function calls that emit SVG fragments
5. Document order = z-order: background first, text last

### Component Composability Rules

These are binding contracts for how components interact:

1. **Position-independent.** Every component takes `x, y, w, h` and renders within that bounding box. No assumptions about position on canvas.

2. **Theme-aware defaults.** Components default to `theme_*` variables for colors, overridable with explicit hex. A layout using all defaults inherits the active theme automatically.

3. **Opt-in history.** Only metrics declared in `{# history: ... #}` frontmatter get buffered. Components needing history for an undeclared metric render empty — graceful degradation.

4. **Purely additive.** Components emit SVG elements only. They don't modify the canvas or other components. Compositing (opacity, layering) is your responsibility via standard SVG attributes.

5. **Single background rule.** At most one background per layout (last `{{ background() }}` call wins). Global config `background_image` overrides per-layout background when set.

6. **Document-order stacking.** Layering follows SVG document order: background → graphs/visualizations → panels → text. Control z-order by element placement in the template.

7. **Graceful degradation.** Missing sensor data → `default(value="--")`. Missing history → empty graph. Missing theme → default palette. Missing background asset → transparent. No component causes a render failure.

8. **Sensor polling independence.** Render tick rate and sensor poll rate are independent. Animation-driven tick rate increases do not increase sensor reads.

---

## Quick Start: Creating a Layout (HTML)

For HTML layouts using the legacy TemplateRenderer. A layout is an HTML file using a CSS subset with Tera template variables for sensor data.

```html
<div style="display: flex; flex-direction: column; width: 480px; height: 480px;
            background: #08080f; padding: 16px; gap: 12px;">

  <!-- Card with label + hero value + secondary info -->
  <div style="display: flex; flex-direction: column; height: 172px;
              background: #12121e; padding: 12px; gap: 4px;">
    <span style="height: 20px; font-size: 14px; color: #888888;">CPU</span>
    <span style="height: 88px; font-size: 64px; color: #e94560;">
      {{ cpu_temp | default(value="--") }}°C
    </span>
    <span style="height: 28px; font-size: 22px; color: #c4546e;">
      {{ cpu_util | default(value="--") }}% LOAD
    </span>
  </div>

</div>
```

Key patterns:
1. Root div sets display dimensions, page background, outer padding
2. Cards use darker background (`#12121e`) on darker page (`#08080f`) for depth
3. Every text span has explicit `height`
4. Sensor values use `{{ key | default(value="--") }}` for missing data
5. Colors create hierarchy: bright accent for hero, dimmed accent for secondary, gray for labels

## Design System

### Typography Scale

| Role | Font Size | Height | Purpose |
|------|-----------|--------|---------|
| Hero value | 64-96px | 88-120px | The number visible from across the room |
| Secondary value | 20-36px | 28-44px | Supporting metrics |
| Small value | 18-22px | 24-28px | Bottom bar / compact cards |
| Label | 10-14px | 14-20px | Category identifiers (CPU, GPU, RAM) |

Single font: JetBrains Mono (monospace). Numbers won't shift width when values change — no layout jitter.

### Color System

**Page backgrounds** — never pure black, use tinted near-blacks:
- `#08080f` — blue-tinted black (recommended default)
- `#0a0a14` — slightly lighter
- `#0a0a0a` — near-black

**Card backgrounds** — subtly lighter for depth:
- `#12121e` — standard card
- `#1a1a2e` — slightly lighter card
- `#111118` — very subtle elevation

**Color-tinted cards** for visual drama (strongest design tool):
- `#1a0a10` — dark red tint (CPU panels)
- `#0a1420` — dark blue tint (GPU panels)

**Accent colors by metric type:**

| Metric | Accent | Dimmed (for secondary text) |
|--------|--------|-----------------------------|
| CPU temp/load | `#e94560` | `#c4546e` |
| GPU temp/load | `#53d8fb` | `#5aabb8` |
| RAM/VRAM | `#cc9eff` | `#bb86fc` |
| FPS/frametime | `#20f5d8` | `#03dac6` |
| Power (watts) | `#FFD080` | `#FFB74D` |

**Labels and muted text:** `#888888` minimum — anything dimmer becomes invisible on LCD hardware.

For temperature ramps, utilization coding, alternative palettes, and Tera conditional examples, see [color-system.md](./references/color-system.md).

### Spacing

- **Outer padding**: 16-20px
- **Gap between cards**: 10-12px
- **Inner card padding**: 8-12px
- **Gap between text elements**: 2-4px

### Layout Arithmetic

All dimensions must be explicit pixels. Verify math before rendering:

```
Total height = root height (480)
Content height = total - 2 × padding
Available for rows = content height - (num_gaps × gap_size)
Sum of row heights must equal available height
```

For layout pattern examples with complete HTML, see [layout-patterns.md](./references/layout-patterns.md).

## Supported CSS Properties

| Property | Values | Notes |
|----------|--------|-------|
| `display` | `flex` | Default, only option |
| `flex-direction` | `row`, `column`, `row-reverse`, `column-reverse` | |
| `justify-content` | `center`, `space-between`, `space-around`, `flex-end` | |
| `align-items` | `center`, `flex-start`, `flex-end`, `stretch` | |
| `gap` | `Npx` | Between flex children |
| `padding` | `Npx` | Uniform all sides only |
| `margin` | `Npx` | Uniform all sides only |
| `width`, `height` | `Npx` | Required for layout |
| `font-size` | `Npx` | 10-120px tested range |
| `color` | `#rrggbb` or `#rgb` | Text color |
| `background` | `#rrggbb` or `#rgb` | Also `background-color` |
| `text-align` | `left`, `center`, `right` | |
| `border-radius` | `Npx` | Parsed but NOT rendered |

**Not supported:** `flex-grow`, `flex-shrink`, `flex-wrap`, `%` units, `em`/`rem`, per-side padding/margin, gradients, borders, shadows, images, grid, positioning, opacity, transforms.

## Available Sensor Keys

### Aggregate (always available)

| Key | Format | Source |
|-----|--------|--------|
| `cpu_temp` | Integer °C | hwmon (k10temp / coretemp alias) |
| `cpu_util` | Float % | sysinfo (average across all cores) |
| `cpu_power` | Integer W | RAPL powercap |
| `gpu_temp` | Integer °C | nvidia-smi / amdgpu sysfs |
| `gpu_util` | Integer % | nvidia-smi / amdgpu sysfs |
| `gpu_power` | Integer W | nvidia-smi / amdgpu sysfs |
| `ram_used` | Float GB (1 decimal) | sysinfo |
| `ram_total` | Float GB (1 decimal) | sysinfo |
| `vram_used` | Float GB (1 decimal) | nvidia-smi / amdgpu sysfs |
| `vram_total` | Float GB (1 decimal) | nvidia-smi / amdgpu sysfs |
| `fps` | Integer | MangoHud (requires active game) |
| `frametime` | Float ms | MangoHud (requires active game) |

### Per-core CPU (sysinfo)

| Key pattern | Format | Description |
|-------------|--------|-------------|
| `cpu_c0_util` – `cpu_cN_util` | Float % | Per-core utilization (0-indexed) |
| `cpu_c0_freq` – `cpu_cN_freq` | Integer MHz | Per-core frequency |

### Per-core temperature (hwmon coretemp)

| Key pattern | Format | Description |
|-------------|--------|-------------|
| `cpu_c0_temp` – `cpu_cN_temp` | Integer °C | Per-core temp from "Core N" hwmon label |

### CCD temps (hwmon k10temp, AMD Zen3+)

| Key | Format | Description |
|-----|--------|-------------|
| `cpu_ccd0_temp` | Integer °C | CCD0 temp (Tccd1 label, 0-indexed) |
| `cpu_ccd1_temp` | Integer °C | CCD1 temp (Tccd2 label) |

### Network (sysinfo, available after second poll)

| Key | Format | Description |
|-----|--------|-------------|
| `net_rx` | Integer B/s | Download throughput (sum of non-loopback interfaces) |
| `net_tx` | Integer B/s | Upload throughput |

Always use `{{ key | default(value="--") }}` — sensors may be unavailable.

## Dynamic Color Coding with Tera

Tera `{% if %}` conditionals can change colors based on sensor values. Sensor values are strings — use `| int` for numeric comparison, and guard with existence check.

For complete temperature/utilization color ramps and Tera implementation examples, see [color-system.md](./references/color-system.md).

## Extending the Rendering Engine

The engine is intentionally minimal. When a design needs something missing:

1. **Identify the gap** — which CSS property or feature is needed?
2. **Check parser.rs** — is it parsed but not rendered? (e.g., `border-radius`)
3. **Implement in the right layer:**
   - New CSS property → `parser.rs` (parse) + `layout.rs` (if layout-affecting) + `draw.rs` (render)
   - New visual feature (images, shapes) → `draw.rs` using tiny-skia
   - New sensor data → add provider in `src/sensor/`
4. **Test with `preview_layout`** before pushing to hardware

The rendering pipeline: Tera template substitution → HTML parsing → taffy flexbox layout → tiny-skia pixel rendering → JPEG encoding with rotation.

## Common Mistakes

| Mistake | Fix |
|---------|-----|
| Text overlaps | Add explicit `height` to every text element |
| Using < 50% of screen | Fill the space — use larger fonts, more padding, bigger cards |
| Too many metrics (> 8) | Cut to 4-6 and make them bigger. Rotate between layouts instead |
| Pure black background (#000) | Use tinted near-black (#08080f, #0a0a14) |
| All text same brightness | Use bright accents for values, dimmed accents for secondary, gray for labels |
| Labels too dim (< #888) | Minimum #888888 for visibility on hardware LCD |
| Using `border-radius` | Parsed but not rendered — don't rely on it for design |
| Missing `default(value="--")` | Sensor may be absent — always provide fallback |
| HTML comments `<!-- -->` | Parser treats `!` as tag name, corrupts entire layout. No comments. |
| Showing same metric twice | Each data point once. If GPU util is in a hero card, don't repeat in bottom bar |
| Not rendering before claiming done | Run preview_layout and READ the PNG. Every time. |

## References

- [components.md](./references/components.md) — Full component catalog: signatures, args, examples (graph, btop_bars, btop_net, btop_ram, background)
- [color-system.md](./references/color-system.md) — Temperature ramps, utilization coding, full palette
- [layout-patterns.md](./references/layout-patterns.md) — Concrete layout examples with complete HTML
- [rendering-engine.md](./references/rendering-engine.md) — Full engine details, pipeline architecture, extension guide
