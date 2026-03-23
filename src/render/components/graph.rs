use std::collections::HashMap;
use tera::{Function, Result, Value};

/// Tera function that emits SVG line/area graph fragments.
///
/// Arguments:
///   data: array of f64 values (from history injection)
///   x, y, w, h: bounding box (defaults: 0, 0, 480, 100)
///   style: "line" or "area" (default: "line")
///   stroke: stroke color (default: "#e94560")
///   fill: fill color for area style (default: "none")
///   stroke_width: line width (default: 2)
pub struct GraphFunction;

impl Function for GraphFunction {
    fn call(&self, args: &HashMap<String, Value>) -> Result<Value> {
        let data = match args.get("data") {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_f64())
                .collect::<Vec<f64>>(),
            _ => return Ok(Value::String("<g></g>".to_string())),
        };

        if data.is_empty() {
            return Ok(Value::String("<g></g>".to_string()));
        }

        let x = args.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let y = args.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let w = args.get("w").and_then(|v| v.as_f64()).unwrap_or(480.0);
        let h = args.get("h").and_then(|v| v.as_f64()).unwrap_or(100.0);
        let style = args.get("style").and_then(|v| v.as_str()).unwrap_or("line").to_string();
        let stroke = args.get("stroke").and_then(|v| v.as_str()).unwrap_or("#e94560").to_string();
        let fill = args.get("fill").and_then(|v| v.as_str()).unwrap_or("none").to_string();
        let stroke_width = args.get("stroke_width").and_then(|v| v.as_f64()).unwrap_or(2.0);

        // Compute min/max for Y-axis scaling
        let min_val = data.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_val = data.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        // Fallback to 1.0 to avoid division by zero when all values are constant
        let range = if (max_val - min_val).abs() < 0.001 { 1.0 } else { max_val - min_val };

        // Generate points
        let step_x = if data.len() > 1 { w / (data.len() - 1) as f64 } else { 0.0 };
        let points: Vec<String> = data
            .iter()
            .enumerate()
            .map(|(i, &val)| {
                let px = x + i as f64 * step_x;
                let normalized = (val - min_val) / range;
                let py = y + h - (normalized * h);
                format!("{:.1},{:.1}", px, py)
            })
            .collect();

        let points_str = points.join(" ");

        let svg = match style.as_str() {
            "area" => {
                // Polygon: line points + bottom-right + bottom-left to close the area
                let bottom_right = format!("{:.1},{:.1}", x + w, y + h);
                let bottom_left = format!("{:.1},{:.1}", x, y + h);
                format!(
                    r#"<g><polygon points="{} {} {}" fill="{}" stroke="{}" stroke-width="{}"/></g>"#,
                    points_str, bottom_right, bottom_left, fill, stroke, stroke_width
                )
            }
            _ => {
                // Line: polyline
                format!(
                    r#"<g><polyline points="{}" fill="none" stroke="{}" stroke-width="{}" stroke-linejoin="round"/></g>"#,
                    points_str, stroke, stroke_width
                )
            }
        };

        Ok(Value::String(svg))
    }

    fn is_safe(&self) -> bool {
        true // Don't HTML-escape the SVG output
    }
}
