//! Lenient reads of `.agents/fabric-policy.json` lane settings.
use serde_json::{Map, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const POLICY: &str = ".agents/fabric-policy.json";
const INSTRUCTION_MODES: &[&str] = &["quarantine", "allow", "deny"];

fn load(workspace_root: &Path, field: &str) -> (Map<String, Value>, Vec<String>) {
    let path = workspace_root.join(POLICY);
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (Map::new(), vec![]),
        Err(_) => {
            return (
                Map::new(),
                vec![format!("{POLICY} is unreadable; using defaults")],
            )
        }
    };
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(_) => {
            return (
                Map::new(),
                vec![format!("{POLICY} is unreadable; using defaults")],
            )
        }
    };
    match crate::policy_json::field(text, field) {
        Ok(Some(policy)) => (policy, vec![]),
        Ok(_) => (
            Map::new(),
            vec![format!("{POLICY} is not an object; using defaults")],
        ),
        Err(_) => (
            Map::new(),
            vec![format!("{POLICY} is unreadable; using defaults")],
        ),
    }
}

pub fn instruction_changes(workspace_root: &Path) -> (String, Vec<String>) {
    let (policy, mut warnings) = load(workspace_root, "instruction_changes");
    match policy.get("instruction_changes") {
        None => ("quarantine".into(), warnings),
        Some(Value::String(value)) if INSTRUCTION_MODES.contains(&value.as_str()) => {
            (value.clone(), warnings)
        }
        Some(_) => {
            warnings.push(format!(
                "{POLICY} instruction_changes must be quarantine, allow or deny; using quarantine"
            ));
            ("quarantine".into(), warnings)
        }
    }
}

pub fn secret_scan_exclude(workspace_root: &Path) -> (Vec<PathBuf>, Vec<String>) {
    let (policy, mut warnings) = load(workspace_root, "secret_scan_exclude");
    let entries = match policy.get("secret_scan_exclude") {
        None => return (vec![], warnings),
        Some(Value::Array(entries)) => entries,
        Some(_) => {
            warnings.push(format!(
                "{POLICY} secret_scan_exclude must be a list; ignoring it"
            ));
            return (vec![], warnings);
        }
    };
    let Ok(root) = crate::pending::resolve_path(workspace_root) else {
        warnings.push(format!("{POLICY} is unreadable; using defaults"));
        return (vec![], warnings);
    };
    let mut excluded = Vec::new();
    for entry in entries {
        let Some(value) = entry.as_str().filter(|value| !value.is_empty()) else {
            warnings.push(format!(
                "{POLICY} secret_scan_exclude takes relative paths inside the project; ignoring {}",
                provenant_core::py::python_value_repr(entry)
            ));
            continue;
        };
        let path = Path::new(value);
        if path.is_absolute() {
            warnings.push(format!(
                "{POLICY} secret_scan_exclude takes relative paths inside the project; ignoring {}",
                provenant_core::py::python_value_repr(entry)
            ));
            continue;
        }
        let candidate = match crate::pending::resolve_path(&root.join(path)) {
            Ok(candidate) => candidate,
            Err(_) => {
                warnings.push(format!("{POLICY} secret_scan_exclude takes relative paths inside the project; ignoring {}", provenant_core::py::python_value_repr(entry)));
                continue;
            }
        };
        if candidate.starts_with(&root) {
            excluded.push(candidate);
        } else {
            warnings.push(format!(
                "{POLICY} secret_scan_exclude takes relative paths inside the project; ignoring {}",
                provenant_core::py::python_value_repr(entry)
            ));
        }
    }
    (excluded, warnings)
}

const DEFAULT_BRANCH: &str = "main";

pub fn tools_policy(workspace_root: &Path) -> (Map<String, Value>, Vec<String>) {
    let (policy, mut warnings) = load(workspace_root, "tools");
    let configured = match policy.get("tools") {
        None => Map::new(),
        Some(Value::Object(configured)) => configured.clone(),
        Some(_) => {
            warnings.push(format!("{POLICY} tools must be an object; using defaults"));
            Map::new()
        }
    };
    let mut tools = configured.clone();
    let branch = match configured.get("default_branch") {
        None => DEFAULT_BRANCH.to_owned(),
        Some(Value::String(value)) if !value.is_empty() => value.clone(),
        Some(_) => {
            warnings.push(format!(
                "{POLICY} tools.default_branch must be a non-empty string; using main"
            ));
            DEFAULT_BRANCH.to_owned()
        }
    };
    tools.insert("default_branch".into(), Value::String(branch));
    let configured_lint = match configured.get("lint") {
        None => Map::new(),
        Some(Value::Object(lint)) => lint.clone(),
        Some(_) => {
            warnings.push(format!(
                "{POLICY} tools.lint must be an object; using defaults"
            ));
            Map::new()
        }
    };
    let mut lint = configured_lint.clone();
    let areas_valid = configured_lint.get("areas").is_none_or(|value| {
        matches!(value, Value::Array(areas) if areas.iter().all(|area| area.as_str().is_some_and(valid_area)))
    });
    let areas = match configured_lint.get("areas") {
        None => Value::Array(vec![]),
        Some(value) if areas_valid => value.clone(),
        Some(_) => {
            warnings.push(format!(
                "{POLICY} tools.lint.areas must be a list of lowercase words; allowing any area"
            ));
            Value::Array(vec![])
        }
    };
    lint.insert("areas".into(), areas);
    for key in ["forbid_attribution", "forbid_closing_keywords"] {
        let value = match configured_lint.get(key) {
            None => Value::Bool(true),
            Some(Value::Bool(value)) => Value::Bool(*value),
            Some(_) => {
                warnings.push(format!(
                    "{POLICY} tools.lint.{key} must be true or false; using true"
                ));
                Value::Bool(true)
            }
        };
        lint.insert(key.into(), value);
    }
    tools.insert("lint".into(), Value::Object(lint));
    (tools, warnings)
}

fn valid_area(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z'))
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_policy(root: &Path, content: &str) {
        fs::create_dir_all(root.join(".agents")).unwrap();
        fs::write(root.join(POLICY), content).unwrap();
    }

    #[test]
    fn defaults_unknown_fields_and_wrong_types_match_policy() {
        let root = tempfile::tempdir().unwrap();
        let (tools, warnings) = tools_policy(root.path());
        assert!(warnings.is_empty());
        assert_eq!(tools["default_branch"], "main");
        assert_eq!(tools["lint"]["areas"], Value::Array(vec![]));
        write_policy(
            root.path(),
            r#"{"tools":{"extra":1,"lint":{"areas":["api"],"x":2}}}"#,
        );
        let (tools, warnings) = tools_policy(root.path());
        assert_eq!(tools["extra"], 1);
        assert_eq!(tools["lint"]["areas"], serde_json::json!(["api"]));
        assert_eq!(tools["lint"]["x"], 2);
        assert!(warnings.is_empty());
        write_policy(
            root.path(),
            r#"{"tools":{"default_branch":null,"lint":{"areas":"api","forbid_attribution":0}}}"#,
        );
        let (_, warnings) = tools_policy(root.path());
        assert_eq!(warnings, [
            format!("{POLICY} tools.default_branch must be a non-empty string; using main"),
            format!("{POLICY} tools.lint.areas must be a list of lowercase words; allowing any area"),
            format!("{POLICY} tools.lint.forbid_attribution must be true or false; using true"),
        ]);
    }

    #[test]
    fn instruction_and_exclusion_policy_validate_values_and_containment() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("allowed")).unwrap();
        write_policy(
            root.path(),
            r#"{"instruction_changes":"deny","secret_scan_exclude":["allowed","../outside",null,"/absolute"]}"#,
        );
        assert_eq!(instruction_changes(root.path()), ("deny".into(), vec![]));
        let (paths, warnings) = secret_scan_exclude(root.path());
        assert_eq!(
            paths,
            [fs::canonicalize(root.path()).unwrap().join("allowed")]
        );
        assert_eq!(warnings.len(), 3);
        write_policy(root.path(), r#"{"instruction_changes":null}"#);
        assert_eq!(instruction_changes(root.path()), ("quarantine".into(), vec![format!("{POLICY} instruction_changes must be quarantine, allow or deny; using quarantine")]));
    }

    #[test]
    fn malformed_and_non_object_files_fall_back_with_source_warnings() {
        let root = tempfile::tempdir().unwrap();
        write_policy(root.path(), "[]");
        assert_eq!(
            instruction_changes(root.path()).1,
            [format!("{POLICY} is not an object; using defaults")]
        );
        write_policy(root.path(), "{");
        assert_eq!(
            instruction_changes(root.path()).1,
            [format!("{POLICY} is unreadable; using defaults")]
        );
    }

    #[test]
    fn python_differential_covers_policy_defaults_and_exclusion_warnings() {
        use std::process::Command;

        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("allowed")).unwrap();
        write_policy(
            root.path(),
            r#"{"instruction_changes":"deny","secret_scan_exclude":["allowed","../outside",null],"tools":{"default_branch":NaN,"extra":1,"lint":{"areas":["api"],"x":2}}}"#,
        );
        let python = std::env::var_os("PROVENANT_PORT_PYTHON").unwrap_or_else(|| "python3".into());
        let source = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../skills/orchestrate/scripts/fabric_policy.py"
        );
        let script = r#"import importlib.util, json, pathlib, sys
spec = importlib.util.spec_from_file_location('fabric_policy', sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
root = pathlib.Path(sys.argv[2])
instruction = module.instruction_changes(root)
excluded = module.secret_scan_exclude(root)
tools = module.tools_policy(root)
print(json.dumps({'instruction': instruction, 'excluded': [[str(p) for p in excluded[0]], excluded[1]], 'tools': tools}))"#;
        let result = Command::new(python)
            .args(["-I", "-c", script, source, root.path().to_str().unwrap()])
            .env_clear()
            .output();
        let Ok(result) = result else {
            if std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref() == Ok("1") {
                panic!("PROVENANT_PORT_PYTHON is required but unavailable");
            }
            eprintln!("skip: Python differential interpreter unavailable");
            return;
        };
        if !result.status.success() {
            if std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref() == Ok("1") {
                panic!(
                    "Python differential failed: {}",
                    String::from_utf8_lossy(&result.stderr)
                );
            }
            eprintln!("skip: Python policy module could not be loaded");
            return;
        }
        let actual: Value = serde_json::from_slice(&result.stdout).unwrap();
        let (paths, path_warnings) = secret_scan_exclude(root.path());
        let expected = serde_json::json!({
            "instruction": instruction_changes(root.path()),
            "excluded": [paths.iter().map(|path| path.to_string_lossy().into_owned()).collect::<Vec<_>>(), path_warnings],
            "tools": tools_policy(root.path()),
        });
        assert_eq!(actual, expected);
    }
    #[test]
    fn unread_python_values_cannot_change_instruction_policy() {
        let root = tempfile::tempdir().unwrap();
        let source = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../skills/orchestrate/scripts/fabric_policy.py"
        );
        for opaque in [
            "18446744073709551616",
            "1e999",
            r#""\ud800""#,
            "NaN",
            r#"{"nested":[1e999,"\udfff"]}"#,
        ] {
            write_policy(
                root.path(),
                &format!(r#"{{"instruction_changes":"deny","unused":{opaque}}}"#),
            );
            assert_eq!(instruction_changes(root.path()), ("deny".into(), vec![]));
            let Some(expected) = crate::test_support::python(
                r#"import importlib.util,json,pathlib,sys
spec=importlib.util.spec_from_file_location('fabric_policy',sys.argv[1]); m=importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
print(json.dumps(m.instruction_changes(pathlib.Path(sys.argv[2]))))"#,
                &[source.into(), root.path().to_str().unwrap().into()],
            ) else {
                return;
            };
            assert_eq!(
                serde_json::json!(instruction_changes(root.path())),
                expected
            );
        }
    }
    #[test]
    fn shared_repr_covers_quotes_controls_nonprintable_and_exponents() {
        let values = serde_json::json!([
            "../it's",
            "\u{0000}\u{0008}\u{001c}\u{200b}",
            1e20,
            "a'b\"c"
        ]);
        let Some(expected) = crate::test_support::python(
            "import json,sys; print(json.dumps([repr(v) for v in json.loads(sys.argv[1])]))",
            &[serde_json::to_string(&values).unwrap()],
        ) else {
            return;
        };
        let actual: Vec<_> = values
            .as_array()
            .unwrap()
            .iter()
            .map(provenant_core::py::python_value_repr)
            .collect();
        assert_eq!(serde_json::json!(actual), expected);
    }
}
