use std::collections::HashMap;
use tera::{Function, Result, Value};

/// One-dimensional flow: fixed card extent, leftover becomes inter-item gap.
///
/// Grafana's dashboard auto-grid sizes panels in tracks (row height / columns)
/// rather than raw pixels, then fits them to the viewport
/// (https://grafana.com/docs/grafana/latest/visualizations/dashboards/build-dashboards/create-dashboard/).
/// CSS Box Alignment `space-between` is the same leftover rule: first and last
/// items flush to the inset edges, equal space only *between* siblings
/// (https://www.w3.org/TR/css-align-3/).
///
/// Cards do not grow. Negative leftover clamps to start-alignment (gap 0).
///
/// Arguments:
///   count: number of items (>= 0)
///   item:  fixed extent of each item along the axis
///   origin: start of the content box
///   span:  length of the content box
///   gap_min: optional floor for the distributed gap (default 0)
pub struct StackFunction;

impl Function for StackFunction {
    fn call(&self, args: &HashMap<String, Value>) -> Result<Value> {
        let count = args.get("count").and_then(value_as_u32).unwrap_or(0) as usize;
        let item = args.get("item").and_then(value_as_f64).unwrap_or(0.0);
        let origin = args.get("origin").and_then(value_as_f64).unwrap_or(0.0);
        let span = args.get("span").and_then(value_as_f64).unwrap_or(0.0);
        let gap_min = args.get("gap_min").and_then(value_as_f64).unwrap_or(0.0);

        if count == 0 || item <= 0.0 {
            return Ok(Value::Array(Vec::new()));
        }

        let leftover = span - item * count as f64;
        let gap = if count == 1 {
            0.0
        } else if leftover <= 0.0 {
            0.0
        } else {
            (leftover / (count - 1) as f64).max(gap_min.max(0.0))
        };

        let positions = (0..count)
            .map(|i| {
                let pos = origin + i as f64 * (item + gap);
                json_number(pos)
            })
            .collect();
        Ok(Value::Array(positions))
    }
}

fn value_as_f64(value: &Value) -> Option<f64> {
    value.as_f64().or_else(|| value.as_i64().map(|n| n as f64))
}

fn value_as_u32(value: &Value) -> Option<u32> {
    if let Some(n) = value.as_u64() {
        return u32::try_from(n).ok();
    }
    if let Some(n) = value.as_i64() {
        return u32::try_from(n).ok();
    }
    value.as_f64().and_then(|n| {
        (n.is_finite() && n >= 0.0 && n <= f64::from(u32::MAX)).then_some(n as u32)
    })
}

fn json_number(n: f64) -> Value {
    Value::from(n.round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tera::{Context, Tera};

    fn render(template: &str) -> String {
        let mut tera = Tera::default();
        tera.register_function("stack", StackFunction);
        tera.add_raw_template("t", template).unwrap();
        tera.render("t", &Context::new()).unwrap()
    }

    #[test]
    fn leftover_becomes_equal_gaps_space_between() {
        // 3×10 in 50 → leftover 20 → gap 10 → 0, 20, 40 (last ends at 50).
        assert_eq!(
            render("{% set ys = stack(count=3, item=10, origin=0, span=50) %}{{ ys[0] }} {{ ys[1] }} {{ ys[2] }}"),
            "0 20 40"
        );
    }

    #[test]
    fn overflow_start_aligns_with_zero_gap() {
        assert_eq!(
            render("{% set ys = stack(count=3, item=10, origin=5, span=20) %}{{ ys[0] }} {{ ys[1] }} {{ ys[2] }}"),
            "5 15 25"
        );
    }

    #[test]
    fn empty_count_is_empty_array() {
        assert_eq!(render("{% set ys = stack(count=0, item=10, span=50) %}{{ ys | length }}"), "0");
    }
}
