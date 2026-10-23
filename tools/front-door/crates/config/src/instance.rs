//! Contract for the cyclic instance-installation owner.
//!
//! Desired state is path-free and committed; pointers and installation receipts
//! are machine-local. Routing reconciliation and seeded-file publication remain
//! with the later implementation/integration units, never in the pointer owner.
use crate::{Result, invalid, pending};
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
#[serde(deny_unknown_fields)]
pub struct Product {
    pub name: String,
    pub version: String,
    // Retains the difference between absent ref and explicit null.
    #[serde(
        rename = "ref",
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_reference"
    )]
    reference: Option<Value>,
}

impl Product {
    /// None means absent; Some(Value::Null) retains an explicit null reference.
    pub fn reference(&self) -> Option<&Value> {
        self.reference.as_ref()
    }
}

// Unlike Option's default deserializer, preserve an explicit JSON null.
fn present_reference<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
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

fn validate_desired_state_value(value: Value) -> Result<DesiredState> {
    validate_desired_state_inner(value, true)
}

fn validate_desired_state_inner(value: Value, check_paths: bool) -> Result<DesiredState> {
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
    if check_paths {
        path_free(&value, "")?;
    }
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
    validate_desired_state_value(
        serde_json::json!({"schema_version":1,"contract":DESIRED_STATE_CONTRACT,"product":{"name":"provenant","version":version},"mode":mode}),
    )
}

pub fn load_desired_state(instance_root: &Path) -> Result<Option<DesiredState>> {
    let path = instance_root.join(DESIRED_STATE_RELATIVE);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path).map_err(|source| crate::Error::Read {
        context: "desired state is unreadable",
        path: path.clone(),
        source,
    })?;
    validate_desired_state_bytes(&bytes).map(Some)
}

pub fn custom_skill_names(instance_root: &Path) -> Result<Vec<std::ffi::OsString>> {
    let directory = instance_root.join(CUSTOM_SKILLS_DIRECTORY);
    if !directory.is_dir() {
        return Ok(vec![]);
    }
    let mut names = Vec::new();
    // pathlib's glob ignores directory scan errors, including permission denial.
    let Ok(entries) = fs::read_dir(&directory) else {
        return Ok(names);
    };
    for entry in entries.flatten() {
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
    default_instance_root_with_homes(
        environment,
        &provenant_core::paths::Homes {
            current: Some(home.into()),
            ..Default::default()
        },
    )
}

pub fn default_instance_root_with_homes(
    environment: &BTreeMap<String, String>,
    homes: &provenant_core::paths::Homes,
) -> Result<PathBuf> {
    if let Some(value) = environment
        .get("AGENT_FABRIC_INSTANCE_ROOT")
        .filter(|s| !s.is_empty())
    {
        let root = provenant_core::paths::expand_user(Path::new(value), homes)?;
        if !root.is_absolute() {
            return Err(invalid(format!(
                "AGENT_FABRIC_INSTANCE_ROOT must be an absolute path, not {}",
                provenant_core::py::python_repr(value)
            )));
        }
        return Ok(root);
    }
    Ok(homes
        .current
        .as_ref()
        .ok_or(provenant_core::paths::PathError::Home)?
        .join(".agents"))
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
    pub schema_version: Value,
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
            assert!(
                validate_desired_state_value(value.clone())
                    .unwrap_err()
                    .to_string()
                    .contains("path-free")
            );
        }
        value["product"]["ref"] = Value::Null;
        assert_eq!(
            validate_desired_state_value(value.clone())
                .unwrap()
                .product
                .reference(),
            Some(&Value::Null)
        );
        assert_eq!(
            serde_json::to_value(validate_desired_state_value(value.clone()).unwrap()).unwrap(),
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
            assert!(validate_desired_state_value(value).is_err());
        }
    }
    #[test]
    fn every_product_decode_rejects_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let mut value =
            serde_json::to_value(build_desired_state("1", InstallMode::Split).unwrap()).unwrap();
        value["product"]["unexpected"] = Value::from("extra");
        let bytes = serde_json::to_vec(&value).unwrap();
        assert!(serde_json::from_value::<Product>(value["product"].clone()).is_err());
        assert!(serde_json::from_value::<DesiredState>(value.clone()).is_err());
        assert!(serde_json::from_slice::<DesiredState>(&bytes).is_err());
        assert!(validate_desired_state(&bytes).is_err());
        let path = dir.path().join(DESIRED_STATE_RELATIVE);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &bytes).unwrap();
        assert!(load_desired_state(dir.path()).is_err());
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
    fn unreadable_custom_skill_directory_is_empty_like_python_glob() {
        use std::os::unix::fs::PermissionsExt;
        let user = provenant_supervisor::sys::account(provenant_supervisor::sys::Account::Current)
            .unwrap()
            .unwrap();
        let root =
            provenant_supervisor::sys::account(provenant_supervisor::sys::Account::Uid(0)).unwrap();
        if root.as_ref().is_some_and(|root| root.name == user.name) {
            eprintln!("skip: root can read chmod 000 directories");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let custom = dir.path().join(CUSTOM_SKILLS_DIRECTORY);
        fs::create_dir_all(custom.join("skill")).unwrap();
        fs::write(custom.join("skill/SKILL.md"), "skill").unwrap();
        let permissions = fs::metadata(&custom).unwrap().permissions();
        fs::set_permissions(&custom, fs::Permissions::from_mode(0o000)).unwrap();
        let actual = custom_skill_names(dir.path());
        let expected = crate::oracle::python(
            "scripts/instance_installation.py",
            "import json\nprint(json.dumps(m.custom_skill_names(p)))",
            dir.path(),
        );
        fs::set_permissions(&custom, permissions).unwrap();
        assert!(actual.unwrap().is_empty());
        if let Some(expected) = expected {
            assert_eq!(
                serde_json::from_slice::<Value>(&expected).unwrap(),
                serde_json::json!([])
            );
        }
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
            let Some(output) = crate::oracle::python(
                "scripts/instance_installation.py",
                "import json\ntry: print(json.dumps(m.validate_desired_state(json.loads(p.read_text()))))\nexcept m.InstallError as e: print(json.dumps(str(e)))",
                &path,
            ) else {
                return;
            };
            let old: Value = serde_json::from_slice(&output).unwrap();
            match validate_desired_state_value(value) {
                Ok(v) => assert_eq!(old, serde_json::to_value(v).unwrap()),
                Err(e) => assert_eq!(old, e.to_string()),
            }
        }
    }
}

/// Decode before validating so diagnostic traversal retains Python object order.
pub fn validate_desired_state_bytes(bytes: &[u8]) -> Result<DesiredState> {
    let document = crate::ordered::decode(
        bytes,
        "desired state is unreadable",
        Path::new(DESIRED_STATE_RELATIVE),
    )?;
    let result = validate_desired_state_inner(document.value(), false)?;
    fn walk(value: &crate::ordered::Json, pointer: &str) -> Result<()> {
        match value {
            crate::ordered::Json::Object(items) => {
                for (k, v) in items {
                    walk(v, &format!("{pointer}/{k}"))?;
                }
            }
            crate::ordered::Json::Array(items) => {
                for (i, v) in items.iter().enumerate() {
                    walk(v, &format!("{pointer}/{i}"))?;
                }
            }
            _ => path_free(&value.value(), pointer)?,
        }
        Ok(())
    }
    walk(&document, "")?;
    Ok(result)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallationReport {
    pub schema_version: Value,
    pub action: Action,
    pub mode: InstallMode,
    pub desired_state: Option<DesiredState>,
    pub desired_state_state: SeedState,
    pub product_pointer: Option<ProductPointer>,
    pub seeded: Vec<SeededFile>,
    pub custom_skills: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing_drift: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed: Option<Vec<String>>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingReport {
    pub schema_version: Value,
    pub action: Action,
    pub routing: RoutingRefresh,
    pub routing_drift: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InstallationResult {
    State(InstallationReport),
    Routing(RoutingReport),
}

/// Cycle boundary: instance seeding owns intent/templates; the managed installer
/// only consumes these results and never writes the instance's desired state.
pub struct InstallationRequest<'a> {
    pub action: Action,
    pub product_root: &'a Path,
    pub instance_root: &'a Path,
    pub write_product_pointer: bool,
}
pub trait InstanceOwner {
    fn execute(&mut self, request: InstallationRequest<'_>) -> Result<InstallationResult>;
}

#[cfg(test)]
mod contract_tests {
    use super::*;
    #[test]
    fn byte_validation_reports_first_inserted_path_and_named_homes_expand() {
        let raw=br#"{"schema_version":1,"contract":"installation-desired-state","mode":"split","product":{"version":"/version","name":"/name"}}"#;
        assert_eq!(
            validate_desired_state_bytes(raw).unwrap_err().to_string(),
            "desired state must stay path-free: /product/version is a absolute path"
        );
        let homes = provenant_core::paths::Homes {
            current: Some("/home/me".into()),
            named: BTreeMap::from([("other".into(), "/home/other".into())]),
        };
        let env = BTreeMap::from([(
            "AGENT_FABRIC_INSTANCE_ROOT".into(),
            "~other/instance".into(),
        )]);
        assert_eq!(
            default_instance_root_with_homes(&env, &homes).unwrap(),
            Path::new("/home/other/instance")
        );
    }
    #[test]
    fn all_real_execute_result_shapes_roundtrip_without_wrapper_fields() {
        let dir = tempfile::tempdir().unwrap();
        let product = dir.path().join("product");
        let instance = dir.path().join("instance");
        fs::create_dir_all(product.join("config")).unwrap();
        fs::create_dir(&instance).unwrap();
        fs::write(product.join("package.json"), r#"{"version":"1"}"#).unwrap();
        for relative in SEEDED_FILES {
            fs::write(
                product.join(relative),
                if relative.ends_with(".json") {
                    "{\"schema_version\":1}"
                } else {
                    "agents"
                },
            )
            .unwrap();
        }
        let body = "import json\nroot=p/'product'; instance=p/'instance'\nout=[]\nfor action in ['show','validate','seed','show','validate','refresh-routing']:\n out.append(m.execute(action,root,instance,write_product_pointer=False))\nprint(json.dumps(out))";
        let Some(output) =
            crate::oracle::python("scripts/instance_installation.py", body, dir.path())
        else {
            return;
        };
        for old in serde_json::from_slice::<Value>(&output)
            .unwrap()
            .as_array()
            .unwrap()
        {
            let contract: InstallationResult = serde_json::from_value(old.clone()).unwrap();
            assert_eq!(&serde_json::to_value(contract).unwrap(), old);
        }
    }
    #[test]
    fn custom_discovery_matches_hidden_symlinked_and_non_file_markers() {
        let dir = tempfile::tempdir().unwrap();
        let custom = dir.path().join(CUSTOM_SKILLS_DIRECTORY);
        for name in [".hidden", "zeta", "directory-marker"] {
            fs::create_dir_all(custom.join(name)).unwrap();
        }
        fs::write(custom.join(".hidden/SKILL.md"), "hidden").unwrap();
        fs::write(custom.join("zeta/SKILL.md"), "z").unwrap();
        fs::create_dir(custom.join("directory-marker/SKILL.md")).unwrap();
        std::os::unix::fs::symlink(custom.join("zeta"), custom.join("alpha")).unwrap();
        let Some(output) = crate::oracle::python(
            "scripts/instance_installation.py",
            "import json\nprint(json.dumps(m.custom_skill_names(p)))",
            dir.path(),
        ) else {
            return;
        };
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap(),
            serde_json::json!(
                custom_skill_names(dir.path())
                    .unwrap()
                    .iter()
                    .map(|s| s.to_str().unwrap())
                    .collect::<Vec<_>>()
            )
        );
    }
}

/// Ordered JSON input retains the source validator's first-error policy.
pub fn validate_desired_state(bytes: &[u8]) -> Result<DesiredState> {
    validate_desired_state_bytes(bytes)
}
