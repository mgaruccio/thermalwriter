# SVG Component Catalog

Tera functions that emit SVG fragments. Call them inside `{{ ... }}` blocks in SVG templates.

All components:
- Take `x, y, w, h` bounding box arguments
- Have `is_safe() = true` so SVG output is not HTML-escaped
- Degrade gracefully: missing data → empty `<g></g>` group, no render error

---

## `graph` — Line / Area Sparkline

Draws a time-series line or area graph from a history array.

**Required:**
| Argument | Type | Description |
|----------|------|-------------|
| `data` | array of floats | History array (pass as Tera variable, e.g. `cpu_util_history`) |

**Optional:**
| Argument | Type | Default | Description |
|----------|------|---------|-------------|
| `x` | float | `0` | Left edge |
| `y` | float | `0` | Top edge |
| `w` | float | `480` | Width |
| `h` | float | `100` | Height |
| `style` | string | `"line"` | `"line"` or `"area"` |
| `stroke` | string | `"#e94560"` | Line/outline color |
| `fill` | string | `"none"` | Area fill color (only meaningful for `style="area"`) |
| `stroke_width` | float | `2` | Line width in SVG units |

**Example:**
```xml
{# history: cpu_util=120s #}
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 480 480">
  <!-- Area graph: cpu utilization, bottom quarter -->
  {{ graph(data=cpu_util_history, x=0, y=380, w=480, h=100,
           style="area", fill="#e9456033", stroke="#e94560") }}
</svg>
```

**Notes:**
- Y-axis auto-scales to the data range
- Constant data (all same value) does not divide by zero — range falls back to 1.0
- Empty data returns `<g></g>` silently

---

## `btop_bars` — Multi-row Utilization Grid

Renders a btop-style colored rectangle grid. Each row is a metric (e.g. a CPU core), each column is a time sample. Rectangle opacity scales with value (0–100%).

**Required:**
| Argument | Type | Description |
|----------|------|-------------|
| `histories` | array of arrays | Pass all history arrays together: `[cpu_c0_util_history, cpu_c1_util_history, ...]` |

**Optional:**
| Argument | Type | Default | Description |
|----------|------|---------|-------------|
| `x` | float | `0` | Left edge |
| `y` | float | `0` | Top edge |
| `w` | float | `480` | Width |
| `h` | float | `100` | Height |
| `color` | string | `"#e94560"` | Bar fill color |

**Example:**
```xml
{# history: cpu_c0_util=60s, cpu_c1_util=60s, cpu_c2_util=60s, cpu_c3_util=60s #}
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 480 480">
  {{ btop_bars(histories=[cpu_c0_util_history, cpu_c1_util_history,
                          cpu_c2_util_history, cpu_c3_util_history],
              x=10, y=200, w=460, h=80, color="#e94560") }}
</svg>
```

**Notes:**
- Values are treated as 0–100% for opacity — works best for utilization metrics
- History arrays must be in Tera context (via `{# history: ... #}` frontmatter)
- Tera functions see only explicit args, not context — pass arrays directly in the call

---

## `btop_net` — Mirrored Network Traffic Graph

Draws upload/download as mirrored area polygons on a center axis (btop-style).

**Required:**
| Argument | Type | Description |
|----------|------|-------------|
| `rx_data` | array of floats | Download history (bytes/sec) |
| `tx_data` | array of floats | Upload history (bytes/sec) |

**Optional:**
| Argument | Type | Default | Description |
|----------|------|---------|-------------|
| `x` | float | `0` | Left edge |
| `y` | float | `0` | Top edge |
| `w` | float | `480` | Width |
| `h` | float | `100` | Height |
| `rx_color` | string | `"#53d8fb"` | Download polygon color |
| `tx_color` | string | `"#e94560"` | Upload polygon color |

**Example:**
```xml
{# history: net_rx=60s, net_tx=60s #}
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 480 480">
  {{ btop_net(rx_data=net_rx_history, tx_data=net_tx_history,
             x=10, y=300, w=460, h=80,
             rx_color="#53d8fb", tx_color="#e94560") }}
</svg>
```

**Notes:**
- RX (download) renders above center axis; TX (upload) renders below
- Both are scaled to the same max value for visual symmetry
- Missing or empty data returns `<g></g>`

---

## `btop_ram` — RAM Usage Area Graph

Draws a filled area showing RAM usage as a fraction of total capacity.

**Required:**
| Argument | Type | Description |
|----------|------|-------------|
| `data` | array of floats | RAM usage history (GiB) |
| `total` | float | Total RAM capacity (GiB) for Y-axis scaling |

**Optional:**
| Argument | Type | Default | Description |
|----------|------|---------|-------------|
| `x` | float | `0` | Left edge |
| `y` | float | `0` | Top edge |
| `w` | float | `480` | Width |
| `h` | float | `100` | Height |
| `fill` | string | `"#cc9eff"` | Area fill color |

**Example:**
```xml
{# history: ram_used=60s #}
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 480 480">
  {{ btop_ram(data=ram_used_history, total=64.0,
             x=10, y=380, w=460, h=60, fill="#cc9eff88") }}
</svg>
```

**Notes:**
- Y-axis scale is absolute (data / total), not relative like `graph` component
- Use `fill` with alpha (`#cc9eff88`) for transparent overlay over other elements

---

## `background` — Background Pattern or Image

Renders a full-canvas background. Three modes: SVG pattern, base64 PNG, or file path.

**Optional (pattern mode):**
| Argument | Type | Default | Description |
|----------|------|---------|-------------|
| `pattern` | string | — | Pattern type: `"grid"`, `"dots"`, `"carbon"`, `"hexgrid"` |
| `color` | string | `"#ffffff10"` | Pattern stroke/fill color (use low alpha for subtle effect) |
| `spacing` | float | `20` | Pattern tile size in SVG units |

**Optional (image mode):**
| Argument | Type | Default | Description |
|----------|------|---------|-------------|
| `image_data` | string | — | Base64-encoded PNG data (from Tera context variable `__bg_image`) |
| `w` | float | `480` | Image width |
| `h` | float | `480` | Image height |
| `opacity` | float | `1.0` | Image opacity (0.0–1.0) |

**Optional (file mode):**
| Argument | Type | Default | Description |
|----------|------|---------|-------------|
| `source` | string | — | File path (relative to resources_dir) |

**Examples:**
```xml
<!-- Subtle grid pattern background -->
{{ background(pattern="grid", color="#ffffff08", spacing=24) }}

<!-- Base64 PNG from injected context variable -->
{{ background(image_data=__bg_image, w=480, h=480, opacity=0.4) }}

<!-- File reference -->
{{ background(source="bg/city.png", w=480, h=480, opacity=0.5) }}
```

**Notes:**
- Pattern mode emits `<defs><pattern>` + `<rect>` — unique pattern ID prevents collisions
- Image mode uses SVG `<image href="data:image/png;base64,...">` — resvg supports this
- Call background FIRST in document order so it appears behind other elements
- At most one background per layout (last call wins)

---

## History Frontmatter Reference

Declare metrics to buffer at the top of SVG templates:

```
{# history: metric_name=duration[s|m], ... [, metric@sample_hz] #}
```

| Token | Example | Meaning |
|-------|---------|---------|
| `metric=60s` | `cpu_util=60s` | Buffer 60 seconds of history for `cpu_util` |
| `metric=120s` | `gpu_temp=120s` | Buffer 2 minutes |
| `metric=300s@0.2hz` | `net_rx=300s@0.2hz` | Buffer 5 minutes, sample at 0.2 Hz |

Each declared metric becomes available as `{metric}_history` in the Tera context (e.g. `cpu_util_history`).

**Full example:**
```xml
{# history: cpu_temp=60s, cpu_util=60s, net_rx=300s, net_tx=300s, ram_used=120s #}
{# animation: fps=10 #}
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 480 480">
  ...
</svg>
```

---

## Theme Variables

Inject the active theme palette by calling `theme.inject_into_context()` (done automatically by the daemon and preview tools). Variables available in all SVG templates:

| Variable | Default |
|----------|---------|
| `theme_primary` | `#e94560` |
| `theme_secondary` | `#53d8fb` |
| `theme_accent` | `#20f5d8` |
| `theme_background` | `#08080f` |
| `theme_surface` | `#12121e` |
| `theme_text` | `#e0e0e0` |
| `theme_text_dim` | `#888888` |
| `theme_success` | `#00ff88` |
| `theme_warning` | `#ffaa00` |
| `theme_critical` | `#ff3333` |

Use as SVG attribute values: `fill="{{ theme_primary }}"` or `stroke="{{ theme_secondary }}"`.
