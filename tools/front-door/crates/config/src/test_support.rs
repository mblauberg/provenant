//! Origin coverage assertion shared with domain crates via `test-support`.
#![allow(clippy::panic)]
use crate::ResolvedConfig;
use serde::Serialize;
use serde_json::Value;

/// Assert that every serialised effective leaf (including empty containers and
/// array members) has an exact origin. The origins metadata is not config data.
pub fn assert_leaf_origins<T: Serialize>(snapshot: &ResolvedConfig<T>) {
    let mut value = match serde_json::to_value(snapshot) {
        Ok(value) => value,
        Err(error) => panic!("cannot serialise effective config: {error}"),
    };
    if let Some(object) = value.as_object_mut() {
        object.remove("origins");
    }
    fn leaves(value: &Value, prefix: &str, output: &mut Vec<String>) {
        match value {
            Value::Object(map) if !map.is_empty() => {
                for (key, child) in map {
                    let key = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    leaves(child, &key, output);
                }
            }
            Value::Array(array) if !array.is_empty() => {
                for (index, child) in array.iter().enumerate() {
                    leaves(child, &format!("{prefix}.{index}"), output);
                }
            }
            _ => output.push(prefix.into()),
        }
    }
    let mut paths = Vec::new();
    leaves(&value, "", &mut paths);
    for path in paths {
        assert!(
            snapshot.origins.contains_key(&path),
            "missing origin for {path}"
        );
    }
}
