# Layout Patterns Reference

Concrete layout templates for common use cases. All examples are 480x480 — adapt dimensions proportionally for other display sizes.

## Pattern 1: Stacked Dashboard ("neon-dash")

Two hero cards (CPU + GPU) with a compact bottom bar. Best general-purpose layout.

```
┌──────────────────────────────┐
│ ┌──────────────────────────┐ │
│ │ CPU                      │ │  Hero card: label + big temp + util
│ │ 47°C                     │ │  height: 172px
│ │ 0.0% LOAD                │ │
│ └──────────────────────────┘ │
│ ┌──────────────────────────┐ │
│ │ GPU                      │ │  Hero card: label + big temp + util + power
│ │ 53°C                     │ │  height: 172px
│ │ 29%  53W                 │ │
│ └──────────────────────────┘ │
│ ┌────────┐┌────────┐┌──────┐│
│ │ 6.0G   ││ 1.4G   ││  --  ││  Bottom bar: 3 compact cards
│ │  RAM   ││ VRAM   ││ FPS  ││  height: 80px
│ └────────┘└────────┘└──────┘│
└──────────────────────────────┘
```

**Layout math:** 480 - 2×16 (padding) = 448 content. 172 + 172 + 80 + 2×12 (gaps) = 448. ✓

See `layouts/neon-dash.html` for the complete implementation.

## Pattern 2: Dual Comparison ("dual-gauge")

Side-by-side CPU vs GPU with color-tinted backgrounds. Strongest visual impact.

```
┌──────────────────────────────┐
│ ┌────────────┐┌────────────┐ │
│ │  (red bg)  ││ (blue bg)  │ │
│ │    CPU     ││    GPU     │ │
│ │    48      ││    53      │ │  Two hero panels, height: 320px
│ │    °C      ││    °C      │ │  width: 218px each
│ │   0.0%     ││   29%      │ │
│ │   LOAD     ││   LOAD     │ │
│ └────────────┘└────────────┘ │
│ ┌──────┐┌──────┐┌────┐┌────┐│
│ │ RAM  ││ VRAM ││ PWR││ FPS││  Bottom bar: 4 compact cards
│ └──────┘└──────┘└────┘└────┘│  height: 100px
└──────────────────────────────┘
```

**Key technique:** Dark red (`#1a0a10`) and dark blue (`#0a1420`) backgrounds create dramatic visual separation without needing borders.

See `layouts/dual-gauge.html` for the complete implementation.

## Pattern 3: Hero + Detail ("fps-hero")

One dominant metric with supporting details below. Best for single-focus use cases (gaming FPS, CPU-only monitoring).

```
┌──────────────────────────────┐
│ ┌──────────────────────────┐ │
│ │        FRAMERATE         │ │
│ │                          │ │  Hero area: 260px tall
│ │          144             │ │  Giant number (110px font)
│ │         6.9ms            │ │
│ └──────────────────────────┘ │
│ ┌────────────┐┌────────────┐ │
│ │ GPU        ││ CPU        │ │  Detail panels: 148px tall
│ │ 53°C       ││ 49°C       │ │  Secondary metrics
│ │ 29%  53W   ││ 0.0% 6.1G  │ │
│ └────────────┘└────────────┘ │
└──────────────────────────────┘
```

See `layouts/fps-hero.html` for the complete implementation.

## Pattern 4: Balanced Grid

Four equal quadrants, each with one metric. Good for at-a-glance monitoring.

```
┌──────────────────────────────┐
│ ┌────────────┐┌────────────┐ │
│ │ CPU        ││ GPU        │ │  Each quadrant: ~218x210px
│ │ 47°C       ││ 53°C       │ │  48-56px value
│ │ 0.0%       ││ 29%        │ │
│ └────────────┘└────────────┘ │
│ ┌────────────┐┌────────────┐ │
│ │ RAM        ││ FPS        │ │
│ │ 6.0/60.4G  ││ 144        │ │
│ │             ││ 6.9ms      │ │
│ └────────────┘└────────────┘ │
└──────────────────────────────┘
```

```html
<div style="display: flex; flex-direction: column; width: 480px; height: 480px;
            background: #08080f; padding: 16px; gap: 12px;">
  <div style="display: flex; flex-direction: row; height: 210px; gap: 12px;">
    <div style="display: flex; flex-direction: column; width: 218px;
                background: #12121e; padding: 14px; gap: 4px;">
      <span style="height: 18px; font-size: 12px; color: #888888;">CPU</span>
      <span style="height: 68px; font-size: 52px; color: #e94560;">
        {{ cpu_temp | default(value="--") }}°C</span>
      <span style="height: 28px; font-size: 22px; color: #c4546e;">
        {{ cpu_util | default(value="--") }}%</span>
    </div>
    <div style="display: flex; flex-direction: column; width: 218px;
                background: #12121e; padding: 14px; gap: 4px;">
      <span style="height: 18px; font-size: 12px; color: #888888;">GPU</span>
      <span style="height: 68px; font-size: 52px; color: #53d8fb;">
        {{ gpu_temp | default(value="--") }}°C</span>
      <span style="height: 28px; font-size: 22px; color: #5aabb8;">
        {{ gpu_util | default(value="--") }}%</span>
    </div>
  </div>
  <div style="display: flex; flex-direction: row; height: 210px; gap: 12px;">
    <div style="display: flex; flex-direction: column; width: 218px;
                background: #12121e; padding: 14px; gap: 4px;">
      <span style="height: 18px; font-size: 12px; color: #888888;">MEMORY</span>
      <span style="height: 68px; font-size: 52px; color: #cc9eff;">
        {{ ram_used | default(value="--") }}</span>
      <span style="height: 28px; font-size: 22px; color: #bb86fc;">
        / {{ ram_total | default(value="--") }} GB</span>
    </div>
    <div style="display: flex; flex-direction: column; width: 218px;
                background: #12121e; padding: 14px; gap: 4px;">
      <span style="height: 18px; font-size: 12px; color: #888888;">FPS</span>
      <span style="height: 68px; font-size: 52px; color: #20f5d8;">
        {{ fps | default(value="--") }}</span>
      <span style="height: 28px; font-size: 22px; color: #03dac6;">
        {{ frametime | default(value="--") }}ms</span>
    </div>
  </div>
</div>
```

## Design Principles Across All Patterns

### Fill the Screen
Use ALL available space. A 480x480 display with only 100px of content in the center looks broken, not minimal.

### 4-6 Metrics Maximum
More than 6 metrics on one screen forces small text. Use multiple layouts with rotation instead.

### Information Hierarchy
Every layout has exactly ONE most important thing. Make it 3-4x larger than everything else:
- Hero value: 52-96px
- Secondary values: 18-28px
- Labels: 10-14px

### Visual Grouping
Related metrics go in the same card. CPU temp + CPU util belong together. Don't scatter.

### Consistent Card Structure
Every metric card follows: **label → value → secondary info** (top to bottom). Don't mix orderings.

### Adapting to Other Display Sizes
The layouts above target 480x480. For other sizes:
1. Scale all dimensions proportionally (multiply by `new_size / 480`)
2. Round to whole pixels
3. Re-verify layout math sums to the new dimensions
4. Font sizes below 10px become illegible — adjust hierarchy if scaling down
