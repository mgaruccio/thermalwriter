#!/usr/bin/env python3
"""Generate the README comparison charts (static SVG, light + dark).

Data below comes from docs/comparison-methodology.md — regenerate the SVGs
after re-running the measurements there, then commit both. No dependencies;
run: python3 scripts/make_comparison_charts.py

Design notes (kept deliberately boring): emphasis form — thermalwriter's bar
in the accent hue, alternatives in a de-emphasis gray. Identity is carried by
the row labels and every bar carries a direct value label, so the gray's
sub-3:1 contrast is relieved per the palette validation (labels + the tables
in the methodology doc). Accent/gray CVD ΔE: 28.1 dark, 45.8 light.
"""

from pathlib import Path

OUT_DIR = Path(__file__).resolve().parent.parent / "docs" / "assets" / "comparison"

# ---------------------------------------------------------------------------
# Measured data (see docs/comparison-methodology.md for protocol + machine)
# ---------------------------------------------------------------------------

MEMORY = {
    "title": "Memory while driving the LCD",
    "subtitle": "avg PSS over 60 s, fresh start, whole process tree · lower is better",
    "unit": "MB",
    "rows": [
        ("thermalwriter daemon", 81.3, True),
        ("TRCC-Linux daemon (headless)", 106.9, False),
        ("thermalright-lcd-control GUI", 278.0, False),
        ("TRCC-Linux GUI", 284.2, False),
    ],
}

CPU = {
    "title": "CPU while keeping the LCD live",
    "subtitle": ("% of one core, stock sensor theme at each tool's default cadence "
                 "· lower is better"),
    "unit": "%",
    "decimals": 2,
    "rows": [
        ("thermalwriter daemon · 2 fps", 0.41, True),
        ("thermalright-lcd-control GUI", 0.42, False),
        ("TRCC-Linux daemon · 0.5 fps", 1.06, False),
        ("TRCC-Linux GUI · 0.5 fps", 1.26, False),
    ],
}

INSTALL = {
    "title": "Installed size",
    "subtitle": "release tarball extracted vs. documented pip install · lower is better",
    "unit": "MB",
    "rows": [
        ("thermalwriter daemon", 20.2, True),
        ("thermalright-lcd-control (pip venv)", 530.0, False),
        ("TRCC-Linux (pip venv)", 847.0, False),
    ],
}

FOOTNOTE = ("Ryzen 9 9950X3D · CachyOS Linux · 2026-07-24 · "
            "same machine, same protocol · docs/comparison-methodology.md")

THEMES = {
    "dark": {
        "surface": "#24283b", "ring": "rgba(255,255,255,0.10)",
        "ink": "#c0caf5", "ink2": "#9aa5ce", "muted": "#737aa2",
        "grid": "#2f344d", "accent": "#6a8fe0", "dim": "#565f89",
    },
    "light": {
        "surface": "#fcfcfb", "ring": "rgba(11,11,11,0.10)",
        "ink": "#1a1b26", "ink2": "#52514e", "muted": "#898781",
        "grid": "#e1e0d9", "accent": "#2e7de9", "dim": "#a8aecb",
    },
}

# Browsers resolve system-ui; keep the stack unquoted — resvg (used for the
# local verification render) fails to parse quoted family names.
FONT = "system-ui, sans-serif"

W = 920
PAD = 28
LABEL_W = 250          # left gutter for row labels
ROW_H = 44
BAR_H = 20
TITLE_BLOCK = 66
AXIS_BAND = 26
FOOT_BLOCK = 30


def nice_max(v: float) -> float:
    """Round up to a clean axis maximum."""
    import math
    if v <= 0:
        return 1.0
    mag = 10 ** math.floor(math.log10(v))
    for m in (1, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10):
        if v <= m * mag:
            return m * mag
    return 10 * mag


def _is_clean_step(step: float) -> bool:
    import math
    s = step / 10 ** math.floor(math.log10(step))
    return any(abs(s - c) < 1e-9 for c in (1, 2, 2.5, 5))

def fmt(v: float, decimals: int = 1) -> str:
    if v < 10:
        return f"{v:,.{decimals}f}"
    return f"{v:,.0f}"


def bar_path(x: float, y: float, w: float, h: float, r: float) -> str:
    """Left edge square (baseline), right edge rounded (data end)."""
    r = min(r, w / 2, h / 2)
    return (f"M {x:.1f} {y:.1f} h {w - r:.1f} "
            f"a {r} {r} 0 0 1 {r} {r} v {h - 2 * r:.1f} "
            f"a {r} {r} 0 0 1 {-r} {r} h {-(w - r):.1f} z")


def render(spec: dict, theme_name: str) -> str:
    t = THEMES[theme_name]
    rows = spec["rows"]
    plot_w = W - PAD * 2 - LABEL_W - 90   # 90px reserved for tip labels
    h = TITLE_BLOCK + len(rows) * ROW_H + AXIS_BAND + FOOT_BLOCK + PAD
    vmax = nice_max(max(v for _, v, _ in rows))

    s = []
    decimals = spec.get("decimals", 1)
    s.append(f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{h}" '
             f'viewBox="0 0 {W} {h}" role="img" '
             f'aria-label="{spec["title"]}: '
             + "; ".join(f"{n} {fmt(v, decimals)} {spec['unit']}" for n, v, _ in rows) + '">')
    s.append(f'<rect x="0.5" y="0.5" width="{W-1}" height="{h-1}" rx="12" '
             f'fill="{t["surface"]}" stroke="{t["ring"]}"/>')
    s.append(f'<text x="{PAD}" y="{PAD + 8}" font-family="{FONT}" font-size="17" '
             f'font-weight="600" fill="{t["ink"]}">{spec["title"]}</text>')
    s.append(f'<text x="{PAD}" y="{PAD + 30}" font-family="{FONT}" font-size="12.5" '
             f'fill="{t["ink2"]}">{spec["subtitle"]}</text>')

    x0 = PAD + LABEL_W
    y0 = TITLE_BLOCK + 10
    plot_h = len(rows) * ROW_H

    # hairline gridlines + axis ticks (solid, recessive), skipping the baseline
    ticks = next((t for t in (5, 4, 3)
                  if _is_clean_step(vmax / t)), 4)
    for i in range(ticks + 1):
        gx = x0 + plot_w * i / ticks
        val = vmax * i / ticks
        if i > 0:
            s.append(f'<line x1="{gx:.1f}" y1="{y0}" x2="{gx:.1f}" '
                     f'y2="{y0 + plot_h}" stroke="{t["grid"]}" stroke-width="1"/>')
        label = f"{val:,.0f}" if vmax >= 10 else f"{val:,.{decimals}f}"
        s.append(f'<text x="{gx:.1f}" y="{y0 + plot_h + 18}" font-family="{FONT}" '
                 f'font-size="11" fill="{t["muted"]}" text-anchor="middle" '
                 f'font-variant-numeric="tabular-nums">{label}</text>')
    s.append(f'<text x="{x0 + plot_w + 34}" y="{y0 + plot_h + 18}" '
             f'font-family="{FONT}" font-size="11" fill="{t["muted"]}" '
             f'text-anchor="middle">{spec["unit"]}</text>')
    # baseline
    s.append(f'<line x1="{x0}" y1="{y0}" x2="{x0}" y2="{y0 + plot_h}" '
             f'stroke="{t["muted"]}" stroke-width="1"/>')

    for i, (name, v, emph) in enumerate(rows):
        cy = y0 + i * ROW_H + ROW_H / 2
        bw = max(2.0, plot_w * v / vmax)
        color = t["accent"] if emph else t["dim"]
        weight = "600" if emph else "400"
        ink = t["ink"] if emph else t["ink2"]
        s.append(f'<text x="{x0 - 12}" y="{cy + 4.5}" font-family="{FONT}" '
                 f'font-size="13" font-weight="{weight}" fill="{ink}" '
                 f'text-anchor="end">{name}</text>')
        s.append(f'<path d="{bar_path(x0, cy - BAR_H / 2, bw, BAR_H, 4)}" '
                 f'fill="{color}"/>')
        s.append(f'<text x="{x0 + bw + 9}" y="{cy + 4.5}" font-family="{FONT}" '
                 f'font-size="13" font-weight="600" fill="{t["ink"]}">'
                 f'{fmt(v, decimals)}</text>')

    s.append(f'<text x="{PAD}" y="{h - PAD + 6}" font-family="{FONT}" '
             f'font-size="10.5" fill="{t["muted"]}">{FOOTNOTE}</text>')
    s.append("</svg>")
    return "\n".join(s)


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    for key, spec in (("memory", MEMORY), ("cpu", CPU), ("install", INSTALL)):
        for mode in ("dark", "light"):
            out = OUT_DIR / f"{key}-{mode}.svg"
            out.write_text(render(spec, mode), encoding="utf-8")
            print(f"wrote {out}")


if __name__ == "__main__":
    main()
