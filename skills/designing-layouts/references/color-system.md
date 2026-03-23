# Color System Reference

## Temperature Ramps

Use Tera conditionals to change text and/or background color based on temperature value.

### CPU/GPU Temperature

| Range | Status | Text Color | Background Tint |
|-------|--------|------------|-----------------|
| < 40°C | Cool/Idle | `#4FC3F7` | `#0d2137` |
| 40-60°C | Normal | `#66BB6A` | `#0d2818` |
| 60-75°C | Warm | `#FDD835` | `#2e2a0a` |
| 75-85°C | Hot | `#FF7043` | `#3e1a0a` |
| > 85°C | Critical | `#EF5350` | `#3e0a0a` |

### Utilization Percentage

| Range | Text Color | Background Tint |
|-------|------------|-----------------|
| 0-25% | `#42A5F5` | `#0d1a2e` |
| 25-50% | `#66BB6A` | `#0d2818` |
| 50-75% | `#FFA726` | `#2e2a0a` |
| 75-100% | `#EF5350` | `#3e0a1a` |

### FPS

| Range | Text Color | Meaning |
|-------|------------|---------|
| > 60 | `#66BB6A` | Smooth |
| 30-60 | `#FDD835` | Acceptable |
| < 30 | `#EF5350` | Choppy |

## Tera Implementation

```html
<!-- Temperature-coded CPU value -->
{% if cpu_temp and cpu_temp | int > 85 %}
<div style="display: flex; flex-direction: column; height: 120px; background: #3e0a0a; padding: 12px;">
  <span style="height: 20px; font-size: 14px; color: #EF5350;">CPU CRITICAL</span>
  <span style="height: 88px; font-size: 64px; color: #EF5350;">{{ cpu_temp }}°C</span>
</div>
{% elif cpu_temp and cpu_temp | int > 75 %}
<div style="display: flex; flex-direction: column; height: 120px; background: #3e1a0a; padding: 12px;">
  <span style="height: 20px; font-size: 14px; color: #FF7043;">CPU HOT</span>
  <span style="height: 88px; font-size: 64px; color: #FF7043;">{{ cpu_temp }}°C</span>
</div>
{% elif cpu_temp and cpu_temp | int > 60 %}
<div style="display: flex; flex-direction: column; height: 120px; background: #2e2a0a; padding: 12px;">
  <span style="height: 20px; font-size: 14px; color: #FDD835;">CPU</span>
  <span style="height: 88px; font-size: 64px; color: #FDD835;">{{ cpu_temp }}°C</span>
</div>
{% else %}
<div style="display: flex; flex-direction: column; height: 120px; background: #0d2818; padding: 12px;">
  <span style="height: 20px; font-size: 14px; color: #66BB6A;">CPU</span>
  <span style="height: 88px; font-size: 64px; color: #66BB6A;">{{ cpu_temp | default(value="--") }}°C</span>
</div>
{% endif %}
```

**Important:** Sensor values are strings. Use `| int` for numeric comparison. Check `cpu_temp` exists before comparing to avoid errors on missing data.

## Accent Color Assignments

### Fixed assignments (non-ramped)

| Metric | Primary | Dimmed | Use for |
|--------|---------|--------|---------|
| CPU | `#e94560` | `#c4546e` | Temperature, utilization, load |
| GPU | `#53d8fb` | `#5aabb8` | Temperature, utilization, load |
| RAM/VRAM | `#cc9eff` | `#bb86fc` | Usage amounts |
| FPS/Frametime | `#20f5d8` | `#03dac6` | Frame rate, timing |
| Power | `#FFD080` | `#FFB74D` | Wattage |
| Fan | `#4DD0E1` | `#3aa8b5` | RPM |

### Creating dimmed variants

Take the primary accent and reduce saturation/brightness by ~25%. Quick method: move each RGB channel ~20% toward the average of all three channels.

## Background Hierarchy

Three levels of depth create visual structure:

| Level | Hex | Usage |
|-------|-----|-------|
| Page background | `#08080f` | Deepest, the "void" |
| Card background | `#12121e` | Standard panels/cards |
| Elevated card | `#1a1a2e` | Highlighted or active panels |

### Color-tinted cards

For maximum visual impact, tint card backgrounds with the metric's accent hue at very low brightness (~10%):

| Tint | Hex | Usage |
|------|-----|-------|
| Red tint | `#1a0a10` | CPU-focused panels |
| Blue tint | `#0a1420` | GPU-focused panels |
| Purple tint | `#140a1a` | Memory panels |
| Green tint | `#0a1a10` | Health/status panels |

## Label and Text Colors

| Role | Hex | Usage |
|------|-----|-------|
| Labels | `#888888` | Minimum for LCD visibility. "CPU", "GPU", "RAM" |
| Secondary info | Dimmed accent | Utilization %, power — use dimmed version of metric accent |
| Muted text | `#666666` | Absolute minimum — only for decorative/non-essential text |
| Primary text | Full accent | Hero values — full brightness accent color |

**Rule:** Labels at `#888888` minimum. Anything dimmer becomes invisible on the LCD hardware in a case with ambient light.

## Alternative Palettes

### Racing HUD
- Background: `#0B0C10`
- Surface: `#1F2833`
- Primary: `#66FCF1`
- Secondary: `#45A29E`
- Text: `#C5C6C7`

### Sci-Fi Lab
- Background: `#0B0F1A`
- Surface: `#1B2A41`
- Primary: `#00B4D8`
- Secondary: `#90E0EF`
- Text: `#CAF0F8`

### Deep Navy
- Background: `#0C1120`
- Surface: `#162033`
- Primary: `#3A82FF`
- Secondary: `#8895A7`
- Text: `#F8FAFC`
