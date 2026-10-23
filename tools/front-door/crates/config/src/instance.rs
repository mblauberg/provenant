//! Contract for the cyclic instance-installation owner.
//!
//! Desired state is path-free and committed; pointers and installation receipts
//! are machine-local. Routing reconciliation and seeded-file publication remain
//! with the later implementation/integration units, never in the pointer owner.
use crate::{invalid, io, pending, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub const DESIRED_STATE_RELATIVE: &str = "config/installation.json";
pub const DESIRED_STATE_CONTRACT: &str = "installation-desired-state";
pub const SEEDED_FILES: [&str; 3] = [
    "AGENTS.md",
    "config/model-preferences.json",
    "config/model-routing.json",
];
pub const ROUTING_BASE_NAME: &str = ".model-routing.base.json";
pub const CUSTOM_SKILLS_DIRECTORY: &str = "custom-skills";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallMode {
    Fused,
    Split,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Product {
    pub name: String,
    pub version: String,
    // Retains the difference between absent ref and explicit null.
    #[serde(flatten)]
    reference: BTreeMap<String, Value>,
}

impl Product {
    /// None means absent; Some(Value::Null) retains an explicit null reference.
    pub fn reference(&self) -> Option<&Value> {
        self.reference.get("ref")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesiredState {
    pub schema_version: Value,
    pub contract: String,
    pub product: Product,
    pub mode: InstallMode,
}

fn path_free(value: &Value, pointer: &str) -> Result<()> {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                path_free(value, &format!("{pointer}/{key}"))?;
            }
        }
        Value::Array(array) => {
            for (index, value) in array.iter().enumerate() {
                path_free(value, &format!("{pointer}/{index}"))?;
            }
        }
        Value::String(s) => {
            let chars: Vec<char> = s.chars().collect();
            let failure = if s.starts_with('~') {
                Some("home-relative path")
            } else if s.starts_with(['/', '\\'])
                || (chars.len() >= 3 && chars[1] == ':' && matches!(chars[2], '/' | '\\'))
            {
                Some("absolute path")
            } else if s.contains("${") {
                Some("path token")
            } else {
                None
            };
            if let Some(failure) = failure {
                return Err(invalid(format!(
                    "desired state must stay path-free: {} is a {failure}",
                    if pointer.is_empty() { "/" } else { pointer }
                )));
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn validate_desired_state(value: Value) -> Result<DesiredState> {
    let map = value
        .as_object()
        .ok_or_else(|| invalid("desired state must be an object"))?;
    if !pending::schema_one(&value["schema_version"]) {
        return Err(invalid("desired state schema_version is unsupported"));
    }
    if value["contract"] != DESIRED_STATE_CONTRACT {
        return Err(invalid(
            "desired state contract is not installation-desired-state",
        ));
    }
    if map.len() != 4 || !map.contains_key("mode") || !map.contains_key("product") {
        return Err(invalid("desired state has unexpected or missing fields"));
    }
    if !matches!(value["mode"].as_str(), Some("fused" | "split")) {
        return Err(invalid("desired state mode must be fused or split"));
    }
    let product = value["product"]
        .as_object()
        .ok_or_else(|| invalid("desired state product has unexpected or missing fields"))?;
    if product
        .keys()
        .any(|k| !matches!(k.as_str(), "name" | "version" | "ref"))
        || !product.contains_key("name")
        || !product.contains_key("version")
    {
        return Err(invalid(
            "desired state product has unexpected or missing fields",
        ));
    }
    for field in ["name", "version", "ref"] {
        let item = product.get(field);
        if field == "ref" && item.is_none_or(Value::is_null) {
            continue;
        }
        if item.and_then(Value::as_str).is_none_or(str::is_empty) {
            return Err(invalid(format!("desired state product {field} is invalid")));
        }
    }
    path_free(&value, "")?;
    serde_json::from_value(value).map_err(|e| invalid(e.to_string()))
}

pub fn install_mode(product_root: &Path, instance_root: &Path) -> Result<InstallMode> {
    Ok(
        if crate::resolve(product_root)? == crate::resolve(instance_root)? {
            InstallMode::Fused
        } else {
            InstallMode::Split
        },
    )
}

pub fn build_desired_state(version: &str, mode: InstallMode) -> Result<DesiredState> {
    validate_desired_state(
        serde_json::json!({"schema_version":1,"contract":DESIRED_STATE_CONTRACT,"product":{"name":"provenant","version":version},"mode":mode}),
    )
}

pub fn load_desired_state(instance_root: &Path) -> Result<Option<DesiredState>> {
    let path = instance_root.join(DESIRED_STATE_RELATIVE);
    if !path.exists() {
        return Ok(None);
    }
    let value = pending::read_json(&path, "desired state is unreadable")?;
    validate_desired_state(value).map(Some)
}

pub fn custom_skill_names(instance_root: &Path) -> Result<Vec<std::ffi::OsString>> {
    let directory = instance_root.join(CUSTOM_SKILLS_DIRECTORY);
    if !directory.is_dir() {
        return Ok(vec![]);
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(&directory).map_err(|e| io(&directory, e))? {
        let entry = entry.map_err(|e| io(&directory, e))?;
        if entry.path().join("SKILL.md").is_file() {
            names.push(entry.file_name());
        }
    }
    names.sort_by_cached_key(|name| pending::path_sort_key(name));
    Ok(names)
}

/// Environment values and home are explicit: AGENTS_HOME is never a fallback.
pub fn default_instance_root(
    environment: &BTreeMap<String, String>,
    home: &Path,
) -> Result<PathBuf> {
    if let Some(value) = environment
        .get("AGENT_FABRIC_INSTANCE_ROOT")
        .filter(|s| !s.is_empty())
    {
        let root = if value == "~" {
            home.to_path_buf()
        } else if let Some(rest) = value.strip_prefix("~/") {
            home.join(rest)
        } else {
            PathBuf::from(value)
        };
        if !root.is_absolute() {
            return Err(invalid(format!(
                "AGENT_FABRIC_INSTANCE_ROOT must be an absolute path, not {value:?}"
            )));
        }
        return Ok(root);
    }
    Ok(home.join(".agents"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Seed,
    Show,
    Validate,
    RefreshRouting,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductPointer {
    pub schema_version: u32,
    pub product_root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeededFile {
    pub path: String,
    pub state: SeedState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RoutingState {
    Existing,
    Updated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SeedState {
    Created,
    Existing,
    Missing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRefresh {
    pub state: RoutingState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<PathBuf>,
    pub conflicts: Vec<String>,
    pub schema_changed: bool,
    pub updated_from_product: Vec<String>,
    pub retained: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub migrated: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desired_state_is_path_free_and_keeps_null_reference() {
        let state = build_desired_state("1.2.3", InstallMode::Split).unwrap();
        let mut value = serde_json::to_value(state).unwrap();
        for reference in ["/opt/product", "~/.agents", "C:\\product", "${ROOT}"] {
            value["product"]["ref"] = reference.into();
            assert!(validate_desired_state(value.clone())
                .unwrap_err()
                .to_string()
                .contains("path-free"));
        }
        value["product"]["ref"] = Value::Null;
        assert_eq!(
            validate_desired_state(value.clone())
                .unwrap()
                .product
                .reference(),
            Some(&Value::Null)
        );
        assert_eq!(
            serde_json::to_value(validate_desired_state(value.clone()).unwrap()).unwrap(),
            value
        );
    }
    #[test]
    fn refuses_unknown_fields_and_bad_headers() {
        for (field, bad) in [
            ("schema_version", Value::from(2)),
            ("contract", Value::from("receipt")),
            ("mode", Value::from("linked")),
            ("unknown", Value::Null),
        ] {
            let mut value =
                serde_json::to_value(build_desired_state("1", InstallMode::Fused).unwrap())
                    .unwrap();
            value[field] = bad;
            assert!(validate_desired_state(value).is_err());
        }
    }
    #[test]
    fn environment_precedence_never_uses_product_root() {
        let home = Path::new("/example");
        let mut env = BTreeMap::from([("AGENTS_HOME".into(), "/product".into())]);
        assert_eq!(
            default_instance_root(&env, home).unwrap(),
            home.join(".agents")
        );
        env.insert("AGENT_FABRIC_INSTANCE_ROOT".into(), "~/instance".into());
        assert_eq!(
            default_instance_root(&env, home).unwrap(),
            home.join("instance")
        );
        env.insert("AGENT_FABRIC_INSTANCE_ROOT".into(), "relative".into());
        assert!(default_instance_root(&env, home).is_err());
    }
    #[test]
    fn custom_skills_are_sorted_and_require_a_skill_file() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["z", "a", "empty"] {
            fs::create_dir_all(dir.path().join("custom-skills").join(name)).unwrap();
        }
        for name in ["z", "a"] {
            fs::write(
                dir.path().join("custom-skills").join(name).join("SKILL.md"),
                "skill",
            )
            .unwrap();
        }
        assert_eq!(custom_skill_names(dir.path()).unwrap(), ["a", "z"]);
        assert_eq!(
            install_mode(dir.path(), &dir.path().join(".")).unwrap(),
            InstallMode::Fused
        );
    }
    #[test]
    fn custom_skill_name_order_preserves_os_bytes() {
        use std::os::unix::ffi::OsStringExt;
        // Pure values: macOS filesystems need not permit invalid UTF-8 names.
        let invalid = std::ffi::OsString::from_vec(vec![0x80]);
        let mut names = vec![invalid.clone(), "é".into(), "a".into()];
        names.sort_by_cached_key(|name| pending::path_sort_key(name));
        assert_eq!(names, [std::ffi::OsString::from("a"), "é".into(), invalid]);
    }

    #[test]
    fn routing_refresh_wire_values_match_source_shapes() {
        let existing = RoutingRefresh {
            state: RoutingState::Existing,
            backup: None,
            conflicts: vec![],
            schema_changed: false,
            updated_from_product: vec![],
            retained: vec![],
            migrated: None,
        };
        assert_eq!(
            serde_json::to_value(existing).unwrap(),
            serde_json::json!({
                "state":"existing",
                "conflicts":[],
                "schema_changed":false,
                "updated_from_product":[],
                "retained":[]
            })
        );
        let updated = RoutingRefresh {
            state: RoutingState::Updated,
            backup: Some("/instance/config/model-routing.json.bak-2026-10-23".into()),
            conflicts: vec!["models.foo".into()],
            schema_changed: true,
            updated_from_product: vec![],
            retained: vec![],
            migrated: Some(vec!["models.alias".into()]),
        };
        assert_eq!(
            serde_json::to_value(updated).unwrap(),
            serde_json::json!({
                "state":"updated",
                "backup":"/instance/config/model-routing.json.bak-2026-10-23",
                "conflicts":["models.foo"],
                "schema_changed":true,
                "updated_from_product":[],
                "retained":[],
                "migrated":["models.alias"]
            })
        );
    }

    #[test]
    fn python_desired_state_contract_differential() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("desired.json");
        for reference in [
            Value::Null,
            Value::from("v1"),
            Value::from("/machine"),
            Value::from(3),
        ] {
            let mut value =
                serde_json::to_value(build_desired_state("1.2.3", InstallMode::Split).unwrap())
                    .unwrap();
            value["product"]["ref"] = reference;
            fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            let Some(output) = crate::oracle::python("scripts/instance_installation.py", "import json\ntry: print(json.dumps(m.validate_desired_state(json.loads(p.read_text()))))\nexcept m.InstallError as e: print(json.dumps(str(e)))", &path) else { return; };
            let old: Value = serde_json::from_slice(&output).unwrap();
            match validate_desired_state(value) {
                Ok(v) => assert_eq!(old, serde_json::to_value(v).unwrap()),
                Err(e) => assert_eq!(old, e.to_string()),
            }
        }
    }
}
