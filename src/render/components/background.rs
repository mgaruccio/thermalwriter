use std::collections::HashMap;
use tera::{Function, Result, Value};

/// Tera function that emits SVG background fragments.
///
/// Three modes:
///   1. `pattern=` — SVG pattern definitions + full-canvas rect
///      Supported patterns: "grid", "dots", "carbon", "hexgrid"
///      Args: pattern, color (default "#ffffff10"), spacing (default 20), w, h
///
///   2. `image_data=` — raster image embedded as base64 data URI
///      Args: image_data (base64 string), w, h, opacity
///
///   3. `source=` — image referenced by file path (resvg resolves via resources_dir)
///      Args: source (path string), w, h, opacity
///
/// Common args: w (default 480), h (default 480), opacity (default 1.0)
pub struct BackgroundFunction;

impl Function for BackgroundFunction {
    fn call(&self, args: &HashMap<String, Value>) -> Result<Value> {
        let w = args.get("w").and_then(|v| v.as_f64()).unwrap_or(480.0);
        let h = args.get("h").and_then(|v| v.as_f64()).unwrap_or(480.0);
        let opacity = args.get("opacity").and_then(|v| v.as_f64()).unwrap_or(1.0);

        // Mode 1: image_data — base64-encoded raster frame
        if let Some(Value::String(b64)) = args.get("image_data") {
            let svg = format!(
                r#"<g><image href="data:image/png;base64,{}" x="0" y="0" width="{}" height="{}" opacity="{}" preserveAspectRatio="xMidYMid slice"/></g>"#,
                b64, w, h, opacity
            );
            return Ok(Value::String(svg));
        }

        // Mode 2: source — file path reference
        if let Some(Value::String(src)) = args.get("source") {
            let svg = format!(
                r#"<g><image href="{}" x="0" y="0" width="{}" height="{}" opacity="{}" preserveAspectRatio="xMidYMid slice"/></g>"#,
                src, w, h, opacity
            );
            return Ok(Value::String(svg));
        }

        // Mode 3: pattern
        let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("grid");
        let color = args.get("color").and_then(|v| v.as_str()).unwrap_or("#ffffff10");
        let spacing = args.get("spacing").and_then(|v| v.as_f64()).unwrap_or(20.0);

        let pattern_def = match pattern {
            "dots" => format!(
                r#"<defs><pattern id="bg-pattern" x="0" y="0" width="{s}" height="{s}" patternUnits="userSpaceOnUse"><circle cx="{h}" cy="{h}" r="1" fill="{c}"/></pattern></defs>"#,
                s = spacing,
                h = spacing / 2.0,
                c = color
            ),
            "carbon" => format!(
                r#"<defs><pattern id="bg-pattern" x="0" y="0" width="{s}" height="{s}" patternUnits="userSpaceOnUse"><rect x="0" y="0" width="{h}" height="{h}" fill="{c}"/><rect x="{h}" y="{h}" width="{h}" height="{h}" fill="{c}"/></pattern></defs>"#,
                s = spacing,
                h = spacing / 2.0,
                c = color
            ),
            "hexgrid" => {
                let r = spacing / 2.0;
                let row_h = r * 1.732;
                let col_w = spacing * 1.5;
                format!(
                    r#"<defs><pattern id="bg-pattern" x="0" y="0" width="{cw}" height="{rh}" patternUnits="userSpaceOnUse"><polygon points="{r},0 {d},{q} {d},{t} {r},{rh} 0,{t} 0,{q}" fill="none" stroke="{c}" stroke-width="0.5"/></pattern></defs>"#,
                    cw = col_w,
                    rh = row_h,
                    r = r,
                    d = r * 2.0,
                    q = row_h / 4.0,
                    t = row_h * 3.0 / 4.0,
                    c = color
                )
            }
            _ => {
                // Default: grid
                format!(
                    r#"<defs><pattern id="bg-pattern" x="0" y="0" width="{s}" height="{s}" patternUnits="userSpaceOnUse"><line x1="{s}" y1="0" x2="{s}" y2="{s}" stroke="{c}" stroke-width="0.5"/><line x1="0" y1="{s}" x2="{s}" y2="{s}" stroke="{c}" stroke-width="0.5"/></pattern></defs>"#,
                    s = spacing,
                    c = color
                )
            }
        };

        let svg = format!(
            r#"<g opacity="{op}">{pat}<rect x="0" y="0" width="{w}" height="{h}" fill="url(#bg-pattern)"/></g>"#,
            op = opacity,
            pat = pattern_def,
            w = w,
            h = h
        );

        Ok(Value::String(svg))
    }

    fn is_safe(&self) -> bool {
        true // Don't HTML-escape the SVG output
    }
}
