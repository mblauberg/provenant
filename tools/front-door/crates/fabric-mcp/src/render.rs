//! Compact user-facing text paired with typed structured MCP results.
use serde_json::Value;

const MAX_DIGEST_CHARS: usize = 280;
const MAX_FACTS: usize = 8;

/// Render only identifiers and state facts. Bodies, prompts and output tails
/// stay in structuredContent and are never repeated into this digest.
pub(crate) fn digest(tool: &str, value: &Value) -> String {
    let mut facts = Vec::new();
    collect(value, &mut facts);
    let mut text = tool.to_owned();
    for (name, value) in facts {
        if text
            .len()
            .saturating_add(name.len())
            .saturating_add(value.len())
            .saturating_add(3)
            > MAX_DIGEST_CHARS
        {
            break;
        }
        text.push(' ');
        text.push_str(name);
        text.push('=');
        text.push_str(&value);
    }
    text
}

fn collect(value: &Value, facts: &mut Vec<(&'static str, String)>) {
    if facts.len() >= MAX_FACTS {
        return;
    }
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if facts.len() >= MAX_FACTS {
                    return;
                }
                let label = match key.as_str() {
                    "id" | "attempt" | "task" | "message" | "claim" | "host" => Some("id"),
                    "state" | "status" | "phase" => Some("state"),
                    "route" => Some("route"),
                    "next" | "cursor" | "next_offset" => Some("next"),
                    "generation" => Some("generation"),
                    "digest" => Some("digest"),
                    _ => None,
                };
                if let Some(label) = label {
                    let rendered = if label == "route" {
                        route(value)
                    } else {
                        scalar(value)
                    };
                    if let Some(rendered) = rendered {
                        facts.push((label, rendered));
                    }
                }
                if label.is_none()
                    && !matches!(key.as_str(), "body" | "prompt" | "bytes" | "data" | "text")
                {
                    collect(value, facts);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                if facts.len() >= MAX_FACTS {
                    break;
                }
                collect(item, facts);
            }
        }
        _ => {}
    }
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if value.len() <= 96 => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn route(value: &Value) -> Option<String> {
    let exact = if let Some(value) = value.as_str() {
        value.to_owned()
    } else {
        let object = value.as_object()?;
        let adapter = object.get("adapter")?.as_str()?;
        let model = object.get("model")?.as_str()?;
        let effort = object.get("effort")?.as_str()?;
        format!("{adapter}/{model}@{effort}")
    };
    (exact.len() <= 180).then_some(exact)
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
