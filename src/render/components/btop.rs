use std::collections::HashMap;
use tera::{Function, Result, Value};

/// Tera function that emits a btop-style CPU core utilization grid.
///
/// Arguments:
///   histories: array of history arrays (e.g. [cpu_c0_util_history, cpu_c1_util_history])
///              Pass the actual history arrays from context as explicit args.
///   x, y, w, h: bounding box
///   color: bar color (default: "#e94560")
///
/// Each row = a metric (core), each column = a time sample.
/// Rect opacity scales with utilization 0-100%.
pub struct BtopBarsFunction;

impl Function for BtopBarsFunction {
    fn call(&self, args: &HashMap<String, Value>) -> Result<Value> {
        // histories is an array of arrays
        let all_histories: Vec<Vec<f64>> = match args.get("histories") {
            Some(Value::Array(outer)) => outer
                .iter()
                .map(|row| match row {
                    Value::Array(inner) => inner.iter().filter_map(|v| v.as_f64()).collect(),
                    _ => Vec::new(),
                })
                .collect(),
            _ => return Ok(Value::String("<g></g>".to_string())),
        };

        if all_histories.is_empty() {
            return Ok(Value::String("<g></g>".to_string()));
        }

        let x = args.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let y = args.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let w = args.get("w").and_then(|v| v.as_f64()).unwrap_or(480.0);
        let h = args.get("h").and_then(|v| v.as_f64()).unwrap_or(100.0);
        let color = args.get("color").and_then(|v| v.as_str()).unwrap_or("#e94560").to_string();

        let mut max_samples = 0usize;
        for hist in &all_histories {
            max_samples = max_samples.max(hist.len());
        }

        if max_samples == 0 {
            return Ok(Value::String("<g></g>".to_string()));
        }

        let n_metrics = all_histories.len();
        let row_h = h / n_metrics as f64;
        let col_w = w / max_samples as f64;

        let mut rects = String::new();
        for (row, hist) in all_histories.iter().enumerate() {
            let rect_y = y + row as f64 * row_h;
            for (col, &val) in hist.iter().enumerate() {
                let rect_x = x + col as f64 * col_w;
                // Clamp value to 0-100 for opacity
                let opacity = (val.clamp(0.0, 100.0) / 100.0).max(0.05);
                rects.push_str(&format!(
                    r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" fill="{}" opacity="{:.2}"/>"#,
                    rect_x,
                    rect_y + 1.0, // 1px gap between rows
                    col_w - 1.0,  // 1px gap between columns
                    row_h - 2.0,
                    color,
                    opacity,
                ));
            }
        }

        Ok(Value::String(format!("<g>{}</g>", rects)))
    }

    fn is_safe(&self) -> bool {
        true
    }
}

/// Tera function that emits a btop-style mirrored network graph.
///
/// Arguments:
///   rx_data: array of RX bytes/sec values
///   tx_data: array of TX bytes/sec values
///   x, y, w, h: bounding box
///   rx_color: color for RX (above center)
///   tx_color: color for TX (below center)
pub struct BtopNetFunction;

impl Function for BtopNetFunction {
    fn call(&self, args: &HashMap<String, Value>) -> Result<Value> {
        let rx_data = match args.get("rx_data") {
            Some(Value::Array(arr)) => arr.iter().filter_map(|v| v.as_f64()).collect::<Vec<f64>>(),
            _ => Vec::new(),
        };
        let tx_data = match args.get("tx_data") {
            Some(Value::Array(arr)) => arr.iter().filter_map(|v| v.as_f64()).collect::<Vec<f64>>(),
            _ => Vec::new(),
        };

        if rx_data.is_empty() && tx_data.is_empty() {
            return Ok(Value::String("<g></g>".to_string()));
        }

        let x = args.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let y = args.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let w = args.get("w").and_then(|v| v.as_f64()).unwrap_or(480.0);
        let h = args.get("h").and_then(|v| v.as_f64()).unwrap_or(100.0);
        let rx_color = args.get("rx_color").and_then(|v| v.as_str()).unwrap_or("#53d8fb").to_string();
        let tx_color = args.get("tx_color").and_then(|v| v.as_str()).unwrap_or("#e94560").to_string();

        let center_y = y + h / 2.0;
        let half_h = h / 2.0;

        // Find max across both datasets for consistent scaling
        let max_val = rx_data.iter().chain(tx_data.iter())
            .cloned()
            .fold(0.0f64, f64::max);
        let scale = if max_val > 0.0 { half_h / max_val } else { 1.0 };

        let n_samples = rx_data.len().max(tx_data.len());
        let step_x = if n_samples > 1 { w / (n_samples - 1) as f64 } else { w };

        // Build RX polygon (above center — y decreases upward)
        let mut rx_svg = String::new();
        if !rx_data.is_empty() {
            let rx_points: Vec<String> = rx_data.iter().enumerate().map(|(i, &val)| {
                let px = x + i as f64 * step_x;
                let py = center_y - (val * scale);
                format!("{:.1},{:.1}", px, py)
            }).collect();
            let br = format!("{:.1},{:.1}", x + (rx_data.len() - 1) as f64 * step_x, center_y);
            let bl = format!("{:.1},{:.1}", x, center_y);
            rx_svg = format!(
                r#"<polygon points="{} {} {}" fill="{}" opacity="0.7"/>"#,
                rx_points.join(" "), br, bl, rx_color
            );
        }

        // Build TX polygon (below center — y increases downward)
        let mut tx_svg = String::new();
        if !tx_data.is_empty() {
            let tx_points: Vec<String> = tx_data.iter().enumerate().map(|(i, &val)| {
                let px = x + i as f64 * step_x;
                let py = center_y + (val * scale);
                format!("{:.1},{:.1}", px, py)
            }).collect();
            let tr = format!("{:.1},{:.1}", x + (tx_data.len() - 1) as f64 * step_x, center_y);
            let tl = format!("{:.1},{:.1}", x, center_y);
            tx_svg = format!(
                r#"<polygon points="{} {} {}" fill="{}" opacity="0.7"/>"#,
                tx_points.join(" "), tr, tl, tx_color
            );
        }

        // Center axis line
        let axis = format!(
            "<line x1=\"{:.1}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"#444444\" stroke-width=\"1\"/>",
            x, center_y, x + w, center_y
        );

        Ok(Value::String(format!("<g>{}{}{}</g>", axis, rx_svg, tx_svg)))
    }

    fn is_safe(&self) -> bool {
        true
    }
}

/// Tera function that emits a btop-style RAM usage area graph.
///
/// Arguments:
///   data: array of RAM usage values (GiB)
///   total: total RAM capacity (GiB) for scaling
///   x, y, w, h: bounding box
///   fill: area fill color
pub struct BtopRamFunction;

impl Function for BtopRamFunction {
    fn call(&self, args: &HashMap<String, Value>) -> Result<Value> {
        let data = match args.get("data") {
            Some(Value::Array(arr)) => arr.iter().filter_map(|v| v.as_f64()).collect::<Vec<f64>>(),
            _ => return Ok(Value::String("<g></g>".to_string())),
        };

        if data.is_empty() {
            return Ok(Value::String("<g></g>".to_string()));
        }

        let x = args.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let y = args.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let w = args.get("w").and_then(|v| v.as_f64()).unwrap_or(480.0);
        let h = args.get("h").and_then(|v| v.as_f64()).unwrap_or(100.0);
        let fill = args.get("fill").and_then(|v| v.as_str()).unwrap_or("#cc9eff").to_string();
        let total = args.get("total").and_then(|v| v.as_f64()).unwrap_or(1.0).max(0.001);

        let step_x = if data.len() > 1 { w / (data.len() - 1) as f64 } else { 0.0 };
        let bottom_y = y + h;

        let points: Vec<String> = data.iter().enumerate().map(|(i, &val)| {
            let px = x + i as f64 * step_x;
            let normalized = (val / total).clamp(0.0, 1.0);
            let py = bottom_y - (normalized * h);
            format!("{:.1},{:.1}", px, py)
        }).collect();

        let br = format!("{:.1},{:.1}", x + (data.len() - 1) as f64 * step_x, bottom_y);
        let bl = format!("{:.1},{:.1}", x, bottom_y);

        let area = format!(
            r#"<polygon points="{} {} {}" fill="{}" opacity="0.8"/>"#,
            points.join(" "), br, bl, fill
        );

        Ok(Value::String(format!("<g>{}</g>", area)))
    }

    fn is_safe(&self) -> bool {
        true
    }
}
