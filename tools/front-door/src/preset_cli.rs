//! Human views of preset inspection; JSON retains the complete typed result.
use serde_json::Value;

fn text(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

pub(super) fn human(data: &Value) -> String {
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
                        output.push_str(&format!("  {name}: {}\n", text(value)));
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
        output.push_str("\nOrigins\n");
        if let Some(origins) = data["origins"].as_object() {
            for (path, origin) in origins {
                output.push_str(&format!("  {path}: {}\n", text(origin)));
            }
        }
        return output;
    }
    serde_json::to_string_pretty(data).unwrap_or_default() + "\n"
}
