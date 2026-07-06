//! Output rendering: machine-readable JSON mode vs a compact human summary.

use serde_json::Value;

#[derive(Clone, Copy)]
pub struct Output {
    pub json: bool,
}

impl Output {
    /// Render a successful result. In JSON mode the raw value is printed
    /// verbatim; otherwise a `summary` line plus a readable body.
    pub fn ok(&self, summary: &str, value: &Value) {
        let rendered = render(self.json, summary, value);
        if !rendered.is_empty() {
            println!("{rendered}");
        }
    }
}

/// Pure rendering core behind `Output::ok`, split out so formatting can be
/// tested without capturing stdout.
fn render(json: bool, summary: &str, value: &Value) -> String {
    if json {
        return serde_json::to_string_pretty(value).unwrap_or_default();
    }
    let mut lines = Vec::new();
    if !summary.is_empty() {
        lines.push(summary.to_string());
    }
    match value {
        Value::Null => {}
        Value::Array(items) => lines.push(render_array(items)),
        _ => lines.push(serde_json::to_string_pretty(value).unwrap_or_default()),
    }
    lines.join("\n")
}

fn render_array(items: &[Value]) -> String {
    if items.is_empty() {
        return "(none)".to_string();
    }
    let mut lines = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            lines.push("-".repeat(50));
        }
        match item {
            Value::Object(_) => {
                let id = item.get("id").and_then(Value::as_str).unwrap_or("?");
                let title = item.get("title").and_then(Value::as_str);
                match title {
                    Some(t) => lines.push(format!("{id}  {t}")),
                    None => lines.push(id.to_string()),
                }
                lines.push(serde_json::to_string_pretty(item).unwrap_or_default());
            }
            _ => lines.push(item.to_string()),
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_mode_prints_the_raw_value_and_ignores_the_summary() {
        let rendered = render(true, "ignored summary", &json!({"id": "ent_1"}));
        assert_eq!(rendered, serde_json::to_string_pretty(&json!({"id": "ent_1"})).unwrap());
    }

    #[test]
    fn human_mode_prints_the_summary_line_before_the_body() {
        let rendered = render(false, "created entity ent_1", &json!({"id": "ent_1"}));
        let mut lines = rendered.lines();
        assert_eq!(lines.next(), Some("created entity ent_1"));
    }

    #[test]
    fn human_mode_with_an_empty_summary_omits_the_summary_line() {
        let rendered = render(false, "", &json!({"id": "ent_1"}));
        assert!(!rendered.starts_with('\n'));
        assert!(rendered.contains("\"id\": \"ent_1\""));
    }

    #[test]
    fn null_value_in_human_mode_renders_only_the_summary() {
        let rendered = render(false, "deleted", &Value::Null);
        assert_eq!(rendered, "deleted");
    }

    #[test]
    fn empty_array_renders_as_none_placeholder() {
        let rendered = render(false, "0 entities", &json!([]));
        assert!(rendered.ends_with("(none)"));
    }

    #[test]
    fn array_of_objects_lists_id_and_title_with_separators() {
        let items = json!([
            {"id": "ent_1", "title": "first"},
            {"id": "ent_2"}
        ]);
        let rendered = render(false, "2 entities", &items);
        assert!(rendered.contains("ent_1  first"));
        assert!(rendered.contains(&"-".repeat(50)));
        assert!(rendered.contains("ent_2"));
        assert!(!rendered.contains("ent_2  "));
    }

    #[test]
    fn array_of_scalars_renders_each_item_as_json() {
        let rendered = render(false, "", &json!(["a", "b"]));
        assert!(rendered.contains("\"a\""));
        assert!(rendered.contains("\"b\""));
    }
}
