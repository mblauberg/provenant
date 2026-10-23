use super::Plan;
use super::{AdapterProfile, BuildError, Mode, profile};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("CF_DISPATCH_CODEX_CONFIG must be clean or inherited")]
    InvalidCodexConfig,
    #[error("Codex MCP transport is not an object")]
    MalformedTransport,
    #[error("Codex MCP transport type is not hashable")]
    MalformedTransportType,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("invalid permission profile key")]
    InvalidProfile,
}

pub type ConfigDecision = Decision;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Inputs {
    pub intent_assurance: bool,
    pub capabilities: bool,
    pub choice: String,
}

/// JSON scalars are valid TOML scalars; serde's compact string encoder also
/// leaves non-BMP characters as Unicode instead of producing surrogate escapes.
pub fn toml_scalar(value: &Value) -> Result<String, serde_json::Error> {
    serde_json::to_string(value)
}

/// Whether an MCP server transport starts a local process or enables command
/// execution. The set of data-only HTTP fields mirrors Codex's input contract.
pub fn transport_executes(transport: &Value) -> Result<bool, AdapterError> {
    const INERT: &[&str] = &[
        "type",
        "url",
        "bearer_token_env_var",
        "http_headers",
        "env_http_headers",
    ];
    let fields = transport
        .as_object()
        .ok_or(AdapterError::MalformedTransport)?;
    let kind = fields.get("type");
    if kind.is_some_and(|value| value.is_array() || value.is_object()) {
        return Err(AdapterError::MalformedTransportType);
    }
    Ok(match kind.and_then(Value::as_str) {
        Some("stdio") => true,
        Some("streamable_http") => fields.iter().any(|(key, value)| {
            !INERT.contains(&key.as_str()) && provenant_core::py::python_truth(value)
        }),
        _ => true,
    })
}

/// Encode an insertion-ordered list of MCP server tables as Codex inline TOML.
pub fn toml_table(entries: &[(String, Vec<(String, Value)>)]) -> Result<String, serde_json::Error> {
    let mut result = String::from("{");
    for (entry_index, (name, fields)) in entries.iter().enumerate() {
        if entry_index != 0 {
            result.push_str(", ");
        }
        result.push_str(&toml_scalar(&Value::String(name.clone()))?);
        result.push_str(" = {");
        for (field_index, (key, value)) in fields.iter().enumerate() {
            if field_index != 0 {
                result.push_str(", ");
            }
            result.push_str(key);
            result.push_str(" = ");
            result.push_str(&toml_scalar(value)?);
        }
        result.push('}');
    }
    result.push('}');
    Ok(result)
}

/// Implements the clean/inherited decision without launching Codex discovery.
pub fn config_decision(
    plan: &Plan,
    inputs: &Inputs,
    discovery_succeeded: bool,
) -> Result<ConfigDecision, AdapterError> {
    decide(plan, inputs, discovery_succeeded.then_some(&[]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::agy::tests::plan;

    #[test]
    fn codex_config_fails_closed_for_failed_reader_discovery() {
        let p = plan(Mode::ReadOnly);
        let decision = config_decision(&p, &Inputs::default(), false).unwrap();
        assert_eq!(decision.config, "clean");
        assert_eq!(
            decision.applied.config_note.as_deref(),
            Some("MCP server discovery failed; a non-writer lane cannot inherit blind")
        );
        assert!(matches!(
            config_decision(
                &p,
                &Inputs {
                    choice: "typo".into(),
                    ..Default::default()
                },
                true
            ),
            Err(AdapterError::InvalidCodexConfig)
        ));
    }

    #[test]
    fn codex_transport_and_toml_helpers_match_python() {
        let input = serde_json::json!({
            "transports": [
                {}, {"type":"stdio"}, {"type":"streamable_http","url":"https://example.invalid"},
                {"type":"streamable_http","http_headers":{"X-Key":"v"}},
                {"type":"streamable_http","command":"exec"},
                {"type":"http","command":null}
            ],
            "entries": [["mcp.one", [["enabled", false], ["default_tools_approval_mode", "approve"]]], ["工具", [["mode", "🦀"]]]]
        });
        let actual = serde_json::json!({
            "executes": input["transports"].as_array().unwrap().iter().map(|transport| transport_executes(transport).unwrap()).collect::<Vec<_>>(),
            "toml": toml_table(&input["entries"].as_array().unwrap().iter().map(|entry| (
                entry[0].as_str().unwrap().to_owned(),
                entry[1].as_array().unwrap().iter().map(|pair| (pair[0].as_str().unwrap().to_owned(), pair[1].clone())).collect()
            )).collect::<Vec<_>>()).unwrap()
        });
        let Some(expected): Option<Value> = crate::oracle::python(
            r#"import importlib,json,sys; m=importlib.import_module('skills.orchestrate.scripts.adapters.codex'); x=json.load(sys.stdin); print(json.dumps({'executes':[m._executes(t) for t in x['transports']], 'toml':m._toml_table({name:dict(fields) for name,fields in x['entries']})}))"#,
            &input,
            None,
        ) else {
            return;
        };
        assert_eq!(actual, expected);
    }

    #[test]
    fn codex_inherits_only_when_safe_and_respects_assurance_and_capability_lanes() {
        let reader = plan(Mode::ReadOnly);
        let inherited = config_decision(&reader, &Inputs::default(), true).unwrap();
        assert_eq!(inherited.config, "inherited");
        assert!(inherited.args.contains(&"features.hooks=false".into()));

        let mut writer = plan(Mode::WorktreeWrite);
        let writer_policy = config_decision(&writer, &Inputs::default(), true).unwrap();
        assert_eq!(writer_policy.config, "inherited");
        assert!(!writer_policy.args.contains(&"features.hooks=false".into()));

        let assurance = config_decision(
            &writer,
            &Inputs {
                intent_assurance: true,
                ..Default::default()
            },
            true,
        )
        .unwrap();
        assert_eq!(assurance.config, "clean");
        writer.mode = Mode::ReadOnly;
        let capability = config_decision(
            &writer,
            &Inputs {
                capabilities: true,
                ..Default::default()
            },
            true,
        )
        .unwrap();
        assert_eq!(capability.config, "clean");
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct McpServer {
    pub name: String,
    #[serde(rename = "exec")]
    pub executes: bool,
    pub plugin: bool,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct AppliedConfig {
    pub config: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_unapproved: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_unconfined: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_disabled: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooks_unconfined: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooks_disabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugins_disabled: Option<bool>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Decision {
    pub config: String,
    pub args: Vec<String>,
    pub applied: AppliedConfig,
}

pub fn decide(
    plan: &Plan,
    inputs: &Inputs,
    inventory: Option<&[McpServer]>,
) -> Result<Decision, AdapterError> {
    if !matches!(inputs.choice.as_str(), "" | "clean" | "inherited") {
        return Err(AdapterError::InvalidCodexConfig);
    }
    let mut clean = Decision {
        config: "clean".into(),
        args: vec!["--ignore-user-config".into(), "--ignore-rules".into()],
        applied: AppliedConfig {
            config: "clean".into(),
            ..Default::default()
        },
    };
    if inputs.intent_assurance || inputs.capabilities || inputs.choice == "clean" {
        return Ok(clean);
    }
    let writer = plan.mode == Mode::WorktreeWrite;
    let mut applied = AppliedConfig {
        config: "inherited".into(),
        ..Default::default()
    };
    if inventory.is_none() {
        if !writer {
            clean.applied.config_note =
                Some("MCP server discovery failed; a non-writer lane cannot inherit blind".into());
            return Ok(clean);
        }
        applied.config_note = Some("MCP server discovery failed; no server approvals set".into());
    }
    let mut args = vec![
        "-c".into(),
        "notify=[]".into(),
        "-c".into(),
        "web_search=\"live\"".into(),
    ];
    let (mut entries, mut plugins, mut disabled, mut unconfined) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut servers = inventory.unwrap_or_default().iter().collect::<Vec<_>>();
    servers.sort_by(|a, b| a.name.cmp(&b.name));
    for server in servers {
        if !crate::pending::python_isprintable(&server.name) {
            if writer {
                applied
                    .mcp_unapproved
                    .get_or_insert_with(Vec::new)
                    .push(server.name.clone());
                continue;
            }
            clean.applied.config_note = Some(format!(
                "MCP server name cannot be overridden: {}",
                crate::ordered_json::json_string(&server.name, true)
            ));
            return Ok(clean);
        }
        if server.plugin {
            plugins.push(server);
        } else if server.executes && !writer {
            entries.push((
                server.name.clone(),
                vec![("enabled".into(), Value::Bool(false))],
            ));
            disabled.push(server.name.clone());
        } else {
            entries.push((
                server.name.clone(),
                vec![(
                    "default_tools_approval_mode".into(),
                    Value::String("approve".into()),
                )],
            ));
        }
        if writer && server.executes {
            unconfined.push(server.name.clone());
        }
    }
    if !entries.is_empty() {
        args.extend([
            "-c".into(),
            format!("mcp_servers={}", toml_table(&entries)?),
        ]);
    }
    if writer {
        applied.hooks_unconfined = Some(true);
        if !unconfined.is_empty() {
            applied.mcp_unconfined = Some(unconfined);
        }
        if !plugins.is_empty() {
            applied
                .mcp_unapproved
                .get_or_insert_with(Vec::new)
                .extend(plugins.iter().map(|s| s.name.clone()));
        }
    } else {
        args.extend(["-c".into(), "features.hooks=false".into()]);
        applied.hooks_disabled = Some(true);
        if plugins.iter().any(|s| s.executes) {
            args.extend(["-c".into(), "features.plugins=false".into()]);
            applied.plugins_disabled = Some(true);
            disabled.extend(
                plugins
                    .iter()
                    .filter(|s| s.executes)
                    .map(|s| s.name.clone()),
            );
        }
    }
    if !disabled.is_empty() {
        disabled.sort();
        applied.mcp_disabled = Some(disabled);
    }
    Ok(Decision {
        config: "inherited".into(),
        args,
        applied,
    })
}

pub const PERMISSION_PATTERN_CHARACTERS: &str = "*?[]{}";
pub const DISCOVERY_SECONDS: u64 = 8;

/// Profile ids come from the execution owner (persisted on write_boundary,
/// otherwise a fresh `provenant-<uuid.hex>`). Never choose a shared fixed id.
pub fn permissions_profile(
    name: &str,
    extends: &str,
    network: bool,
    filesystem: Option<&[(String, Value)]>,
) -> Result<Vec<String>, AdapterError> {
    let mut command = vec![
        "-c".into(),
        format!(
            "default_permissions={}",
            toml_scalar(&Value::String(name.into()))?
        ),
        "-c".into(),
        format!(
            "permissions.{name}.extends={}",
            toml_scalar(&Value::String(extends.into()))?
        ),
        "-c".into(),
        format!("permissions.{name}.network.enabled={network}"),
    ];
    if let Some(fs) = filesystem {
        let entries = fs
            .iter()
            .map(|(p, a)| {
                Ok(format!(
                    "{} = {}",
                    toml_scalar(&Value::String(p.clone()))?,
                    toml_scalar(a)?
                ))
            })
            .collect::<Result<Vec<_>, serde_json::Error>>()?;
        command.extend([
            "-c".into(),
            format!("permissions.{name}.filesystem={{{}}}", entries.join(", ")),
        ]);
    }
    Ok(command)
}

pub fn read_only_writable_dirs<'a>(cwd: &str, dirs: &'a [String]) -> Vec<&'a str> {
    let cwd = std::path::Path::new(cwd);
    dirs.iter()
        .filter(|p| {
            !cwd.starts_with(p) && !p.chars().any(|c| PERMISSION_PATTERN_CHARACTERS.contains(c))
        })
        .map(String::as_str)
        .collect()
}

/// Presence matters: a missing profile defaults to endpoint, a present null
/// or empty value suppresses the name override.
#[derive(Clone, Debug, Default)]
pub enum EndpointProfile {
    #[default]
    Missing,
    Present(Value),
}
impl EndpointProfile {
    fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }
    fn value(&self) -> Value {
        match self {
            Self::Missing => Value::String("endpoint".into()),
            Self::Present(v) => v.clone(),
        }
    }
}
impl Serialize for EndpointProfile {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Missing => s.serialize_unit(),
            Self::Present(v) => v.serialize(s),
        }
    }
}
impl<'de> Deserialize<'de> for EndpointProfile {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::Present(Value::deserialize(d)?))
    }
}

/// Execution facts that require OS custody remain outside this pure builder.
/// The host supplies a cached decision, validated state grant and unique profile.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BuildInputs {
    pub decision: Decision,
    pub sandbox: String,
    pub network: bool,
    pub capabilities: bool,
    pub darwin: bool,
    pub profile_name: String,
    pub state_dir: Option<String>,
    pub filesystem: Vec<(String, Value)>,
    pub worktree: Option<String>,
    pub endpoint_token_env: Option<String>,
    #[serde(default, skip_serializing_if = "EndpointProfile::is_missing")]
    pub endpoint_profile: EndpointProfile,
    pub endpoint_wire_api: Option<String>,
}

pub fn codex(plan: &Plan, inputs: &BuildInputs) -> Result<AdapterProfile, BuildError> {
    fn scalar(value: &Value) -> Result<String, BuildError> {
        toml_scalar(value).map_err(|e| BuildError::Codex(AdapterError::Json(e)))
    }
    let mut command = vec!["exec".into()];
    let resume = plan.resume_session.as_deref().filter(|s| !s.is_empty());
    if let Some(session) = resume {
        command.extend(["resume".into(), session.into()]);
    }
    command.extend(["--json".into(), "--skip-git-repo-check".into()]);
    command.extend(inputs.decision.args.clone());
    command.extend([
        "-c".into(),
        "service_tier=\"default\"".into(),
        "-c".into(),
        "approval_policy=\"never\"".into(),
    ]);
    if inputs.darwin && inputs.sandbox != "full" && inputs.decision.config != "inherited" {
        command.extend(["-c".into(), "allow_login_shell=false".into()]);
    }
    if inputs.capabilities {
        if resume.is_some() {
            command.extend(["-c".into(), "sandbox_mode=\"danger-full-access\"".into()]);
        } else {
            command.extend(["-s".into(), "danger-full-access".into()]);
        }
    } else if matches!(inputs.sandbox.as_str(), "read-only" | "workspace-write") {
        let read_only = inputs.sandbox == "read-only";
        let mut filesystem = if read_only {
            vec![(":tmpdir".into(), Value::String("write".into()))]
        } else {
            inputs.filesystem.clone()
        };
        if read_only {
            filesystem.extend(
                read_only_writable_dirs(&plan.cwd, &plan.add_dirs)
                    .into_iter()
                    .map(|p| (p.into(), Value::String("write".into()))),
            );
        }
        if let Some(state) = &inputs.state_dir {
            if let Some((_, access)) = filesystem.iter_mut().find(|(p, _)| p == state) {
                *access = Value::String("write".into());
            } else {
                filesystem.push((state.clone(), Value::String("write".into())));
            }
        }
        command.extend(
            permissions_profile(
                &inputs.profile_name,
                if read_only {
                    ":read-only"
                } else {
                    ":workspace"
                },
                inputs.network,
                Some(&filesystem),
            )
            .map_err(BuildError::Codex)?,
        );
    } else {
        let sandbox = if inputs.sandbox == "full" {
            "danger-full-access"
        } else {
            &inputs.sandbox
        };
        if resume.is_some() {
            command.extend([
                "-c".into(),
                format!("sandbox_mode={}", scalar(&Value::String(sandbox.into()))?),
            ]);
        } else {
            command.extend(["-s".into(), sandbox.into()]);
        }
    }
    if resume.is_none() {
        for dir in &plan.add_dirs {
            command.extend(["--add-dir".into(), dir.clone()]);
        }
        if let Some(worktree) = inputs.worktree.as_deref().filter(|s| !s.is_empty()) {
            command.extend(["--cd".into(), worktree.into()]);
        }
    }
    let endpoint = plan
        .endpoint_base_url
        .as_ref()
        .filter(|v| provenant_core::py::python_truth(v));
    let token = inputs
        .endpoint_token_env
        .as_deref()
        .filter(|s| !s.is_empty());
    if let (Some(endpoint), Some(token)) = (endpoint, token) {
        for (key, value) in [
            ("name", inputs.endpoint_profile.value()),
            ("base_url", endpoint.clone()),
            ("env_key", Value::String(token.into())),
            (
                "wire_api",
                inputs
                    .endpoint_wire_api
                    .clone()
                    .map(Value::String)
                    .unwrap_or_default(),
            ),
        ] {
            if provenant_core::py::python_truth(&value) {
                command.extend([
                    "-c".into(),
                    format!(
                        "model_providers.provenant_endpoint.{key}={}",
                        scalar(&value)?
                    ),
                ]);
            }
        }
        command.extend(["-c".into(), "model_provider=\"provenant_endpoint\"".into()]);
    } else {
        command.extend([
            "-c".into(),
            "model_provider=\"openai\"".into(),
            "-c".into(),
            "openai_base_url=\"\"".into(),
        ]);
    }
    if let Some(model) = plan.model.as_deref().filter(|s| !s.is_empty()) {
        command.extend(["-m".into(), model.into()]);
    }
    if let Some(effort) = plan.effort.as_deref().filter(|s| !s.is_empty()) {
        command.extend([
            "-c".into(),
            format!(
                "model_reasoning_effort={}",
                scalar(&Value::String(effort.into()))?
            ),
        ]);
    }
    if let Some(ceiling) = plan.context_ceiling.as_ref().filter(|v| v.truth()) {
        command.extend([
            "-c".into(),
            format!(
                "model_auto_compact_token_limit={}",
                crate::pending::integer_string(&ceiling.value())?
            ),
        ]);
    }
    command.push("-".into());
    profile("codex", command)
}

#[cfg(test)]
mod policy_oracles {
    use super::*;
    use crate::adapters::agy::tests::plan;
    #[test]
    fn cached_configuration_policy_matches_source() {
        let Some(cases): Option<Vec<(Value, Value)>> = crate::oracle::python(
            r#"
import json,sys
from skills.orchestrate.scripts.adapters import codex as m
cases=[]
inventories=[None,{}, {'a.b':{'exec':True,'plugin':False},'remote':{'exec':False,'plugin':False},'plug':{'exec':True,'plugin':True}}, {'bad\nname':{'exec':True,'plugin':False},'plug':{'exec':False,'plugin':True}}, {'unicode🦀':{'exec':False,'plugin':False}}]
for writer in [False,True]:
 for intent in ['ordinary','assurance']:
  for capabilities in [[],['browser']]:
   for inventory in inventories:
    p={'mode':'worktree_write' if writer else 'read_only','intent':intent,'applied':{'capabilities':capabilities},'_mcp_inventory':inventory}
    cases.append((p,m.config_decision(p)))
print(json.dumps(cases))
"#,
            &Value::Null,
            None,
        ) else {
            return;
        };
        for (p, expected) in cases {
            let common = plan(if p["mode"] == "worktree_write" {
                Mode::WorktreeWrite
            } else {
                Mode::ReadOnly
            });
            let inventory = p["_mcp_inventory"].as_object().map(|i| {
                i.iter()
                    .map(|(name, v)| McpServer {
                        name: name.clone(),
                        executes: v["exec"].as_bool().unwrap(),
                        plugin: v["plugin"].as_bool().unwrap(),
                    })
                    .collect::<Vec<_>>()
            });
            let inputs = Inputs {
                intent_assurance: p["intent"] == "assurance",
                capabilities: !p["applied"]["capabilities"].as_array().unwrap().is_empty(),
                choice: String::new(),
            };
            assert_eq!(
                serde_json::to_value(decide(&common, &inputs, inventory.as_deref()).unwrap())
                    .unwrap(),
                expected,
                "{p}"
            );
        }
    }
    #[test]
    fn permissions_and_read_only_grants_match_source() {
        let mut fixtures = Vec::new();
        for dirs in [
            vec![
                "/repo".to_string(),
                "/repo/sub".into(),
                "/safe".into(),
                "/wild*".into(),
            ],
            vec![],
        ] {
            for filesystem in [
                None,
                Some(vec![
                    (":tmpdir".to_string(), Value::String("write".into())),
                    ("/路径🦀".into(), Value::String("read".into())),
                ]),
            ] {
                fixtures.push(serde_json::json!({"cwd":"/repo","applied":{"add_dirs":dirs,"write_boundary":{"profile":"provenant-fixture"}},"fs":filesystem}));
            }
        }
        let actual = fixtures
            .iter()
            .map(|p| {
                let fs: Option<Vec<(String, Value)>> =
                    serde_json::from_value(p["fs"].clone()).unwrap();
                let dirs: Vec<String> =
                    serde_json::from_value(p["applied"]["add_dirs"].clone()).unwrap();
                serde_json::json!([
                    permissions_profile("provenant-fixture", ":read-only", false, fs.as_deref())
                        .unwrap(),
                    read_only_writable_dirs("/repo", &dirs)
                ])
            })
            .collect::<Vec<_>>();
        let Some(expected): Option<Vec<Value>> = crate::oracle::python(
            r#"
import json,sys
from skills.orchestrate.scripts.adapters import codex as m
out=[]
for p in json.load(sys.stdin):
 fs=p.pop('fs'); fs=None if fs is None else dict(fs)
 out.append([m.permissions_profile(p,':read-only',False,fs),m.read_only_writable_dirs(p)])
print(json.dumps(out))
"#,
            &Value::Array(fixtures),
            None,
        ) else {
            return;
        };
        assert_eq!(actual, expected);
    }
}

#[cfg(test)]
mod argv_oracle {
    use super::*;
    #[test]
    fn argv_assembly_matches_source_with_cached_policy_and_fixed_profile() {
        let Some(cases): Option<Vec<Value>> = crate::oracle::python(
            r#"
import json,sys
from skills.orchestrate.scripts.adapters import codex as m
cases=[]
for sandbox in ['read-only','workspace-write','full','other']:
 for resume in [None,'resume-id']:
  for capabilities in [[],['browser']]:
   for endpoint in [False,True]:
    for context in [None,-3.5,'12']:
     p=dict(mode='worktree_write' if sandbox=='workspace-write' else 'read_only',prompt='do it',boundary_prompt='Read only',timeout_seconds=30,agy_sandbox=False,resume_session=resume,model='model-x',effort='high',session_id='session-1',cwd='/workspace/fixture',workspace_root='/workspace/fixture',worktree='/workspace/fixture' if sandbox=='workspace-write' else None,protected_paths=[],context_ceiling=context,route={'endpoint_base_url':'https://example.invalid','endpoint_token_env':'ENDPOINT_TOKEN','endpoint_wire_api':'responses'} if endpoint else {},applied=dict(sandbox=sandbox,network=False,capabilities=capabilities,add_dirs=['/shared'],write_boundary=dict(profile='provenant-fixture',filesystem={'/shared':'read',':tmpdir':'write'})),_mcp_inventory={})
     if endpoint and resume is not None: p['route']['endpoint_profile']={None:None,-3.5:'','12':'custom'}[context]
     decision=m.config_decision(p)
     state,note=m.provenant_state_dir(p)
     argv=m.argv(p)
     cases.append(dict(p=p,decision=decision,state=state,argv=argv,darwin=sys.platform=='darwin'))
print(json.dumps(cases))
"#,
            &Value::Null,
            None,
        ) else {
            return;
        };
        for case in cases {
            let p = &case["p"];
            let mut common = crate::adapters::agy::tests::plan(if p["mode"] == "worktree_write" {
                Mode::WorktreeWrite
            } else {
                Mode::ReadOnly
            });
            common.cwd = p["cwd"].as_str().unwrap().into();
            common.resume_session = p["resume_session"].as_str().map(str::to_owned);
            common.context_ceiling = p.get("context_ceiling").cloned().map(Into::into);
            common.endpoint_base_url = p["route"].get("endpoint_base_url").cloned();
            let inputs = BuildInputs {
                decision: serde_json::from_value(case["decision"].clone()).unwrap(),
                sandbox: p["applied"]["sandbox"].as_str().unwrap().into(),
                network: false,
                capabilities: !p["applied"]["capabilities"].as_array().unwrap().is_empty(),
                darwin: case["darwin"].as_bool().unwrap(),
                profile_name: "provenant-fixture".into(),
                state_dir: case["state"].as_str().map(str::to_owned),
                filesystem: vec![
                    ("/shared".into(), Value::String("read".into())),
                    (":tmpdir".into(), Value::String("write".into())),
                ],
                worktree: p["worktree"].as_str().map(str::to_owned),
                endpoint_token_env: p["route"]
                    .get("endpoint_token_env")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                endpoint_profile: p["route"]
                    .get("endpoint_profile")
                    .cloned()
                    .map(EndpointProfile::Present)
                    .unwrap_or_default(),
                endpoint_wire_api: p["route"]
                    .get("endpoint_wire_api")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            };
            assert_eq!(
                serde_json::to_value(codex(&common, &inputs).unwrap().argv).unwrap(),
                case["argv"],
                "{p}"
            );
        }
    }
}
