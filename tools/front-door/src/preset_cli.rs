//! Human views of preset inspection; JSON retains the complete typed result.
use serde_json::Value;

fn text(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

pub(super) fn human(data: &Value, full: bool) -> String {
    if let Some(presets) = data["presets"].as_array() {
        let mut rows = vec![vec![
            "NAME".into(),
            "LAYER".into(),
            "LEVEL".into(),
            "COST".into(),
            "ERRORS".into(),
        ]];
        for preset in presets {
            rows.push(vec![
                text(&preset["name"]),
                preset["layers"]
                    .as_array()
                    .and_then(|layers| layers.last())
                    .map_or_else(|| "-".into(), text),
                text(&preset["level"]),
                text(&preset["cost"]),
                preset["errors"]
                    .as_array()
                    .map(|errors| errors.iter().map(text).collect::<Vec<_>>().join("; "))
                    .unwrap_or_default(),
            ]);
        }
        let widths: Vec<_> = (0..5)
            .map(|column| {
                rows.iter()
                    .map(|row| row[column].chars().count())
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        return rows
            .iter()
            .map(|row| {
                row.iter()
                    .enumerate()
                    .map(|(column, cell)| format!("{cell:width$}", width = widths[column]))
                    .collect::<Vec<_>>()
                    .join("  ")
                    .trim_end()
                    .to_owned()
                    + "\n"
            })
            .collect();
    }
    if let Some(values) = data.get("values") {
        let mut output = format!("Preset {}\n", text(&data["name"]));
        for (label, key) in [
            ("Pattern", "pattern"),
            ("Parameters", "parameters"),
            ("Stages", "stages"),
            ("Roles", "roles"),
            ("Exports", "exports"),
        ] {
            if let Some(value) = values.get(key) {
                output.push_str(&format!("\n{label}\n"));
                if let Some(map) = value.as_object() {
                    for (name, value) in map {
                        if key == "parameters" {
                            let spec = super::is_spec(value);
                            let default_value = if spec {
                                value.get("default")
                            } else {
                                Some(value)
                            };
                            let kind =
                                value
                                    .get("type")
                                    .and_then(Value::as_str)
                                    .unwrap_or_else(|| match default_value {
                                        Some(Value::Bool(_)) => "boolean",
                                        Some(Value::Number(_)) => "number",
                                        Some(Value::String(_)) => "string",
                                        Some(Value::Array(_)) => "list",
                                        Some(Value::Object(_)) => "table",
                                        _ => "-",
                                    });
                            let default =
                                default_value.map_or_else(|| "-".into(), Value::to_string);
                            let doc = value.get("doc").and_then(Value::as_str).unwrap_or("");
                            let first = doc.find(". ").map_or(doc, |end| &doc[..=end]);
                            output.push_str(&format!("  {name}  {kind}  {default}  {first}\n"));
                        } else {
                            output.push_str(&format!("  {name}: {}\n", text(value)));
                        }
                    }
                } else if let Some(items) = value.as_array() {
                    for item in items {
                        output.push_str(&format!("  {}\n", text(item)));
                    }
                } else {
                    output.push_str(&format!("  {}\n", text(value)));
                }
            }
        }
        if full {
            output.push_str("\nOrigins\n");
            if let Some(origins) = data["origins"].as_object() {
                for (path, origin) in origins {
                    output.push_str(&format!("  {path}: {}\n", text(origin)));
                }
            }
        }
        return output;
    }
    if !full && let Some(parameters) = data.get("parameters").and_then(Value::as_array) {
        let names = parameters
            .iter()
            .filter_map(|p| p["name"].as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return format!("Preset {}\nParameters: {names}\n", text(&data["name"]));
    }
    serde_json::to_string_pretty(data).unwrap_or_default() + "\n"
}

#[cfg(test)]
mod tests {
    use super::human;
    use serde_json::json;

    #[test]
    fn compact_show_lists_parameter_docs_and_full_shows_origins() {
        let data = json!({"name":"example", "values":{"parameters":{"base":{"type":"string", "default":"main", "doc":"First sentence. Second sentence."}}}, "origins":{"parameters.base":"bundled"}});
        let compact = human(&data, false);
        assert!(compact.contains("base  string  \"main\"  First sentence"));
        assert!(!compact.contains("Origins"));
        assert!(human(&data, true).contains("parameters.base: bundled"));
    }

    #[test]
    fn review_council_compact_show_contains_parameter_docs() {
        let layers = super::super::Layers::empty();
        let (values, origins) = layers.resolve("review-council").unwrap();
        let data = json!({"name":"review-council", "values":values, "origins":origins});
        let before = data.clone();
        let shown = human(&data, false);
        for (name, value) in data["values"]["parameters"].as_object().unwrap() {
            assert!(shown.contains(name), "missing {name}");
            if let Some(doc) = value.get("doc").and_then(serde_json::Value::as_str) {
                let first = doc.find(". ").map_or(doc, |end| &doc[..=end]);
                assert!(shown.contains(first), "missing {name}: {first}");
            }
        }
        assert_eq!(data, before);
    }

    #[test]
    fn compact_explain_lists_parameter_names() {
        let data = json!({"name":"example", "parameters":[{"name":"base"}, {"name":"risk"}]});
        assert!(human(&data, false).contains("Parameters: base, risk"));
    }
}
