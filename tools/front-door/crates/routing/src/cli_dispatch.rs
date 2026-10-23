//! The completed prefix of model_route_cli._main. Resolution and process probes
//! remain explicit continuations; no executable front door calls this module.
use crate::{
    cli::{self, ParsedCommand, RouteCommand, SelectArgs},
    ordered_json::OrderedValue,
    selection::SelectionOutput,
};
use provenant_core::py::python_truth;
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("{0}")]
    Value(String),
    #[error("malformed routing command data: {0}")]
    Malformed(&'static str),
    #[error("routing {operation} failed: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    NumberOutOfRange(#[from] provenant_records::NumberOutOfRange),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}
#[derive(Debug)]
pub enum Dispatch {
    Complete(CommandOutput),
    Continue(RouteCommand),
}

pub use crate::cli::CliDefaults;
/// One boundary for the current YAML / future JSON document. Parsing belongs to
/// config; no second YAML subset parser is introduced by routing.
pub trait CompatibilityDocumentLoader {
    fn load_adapter_compatibility(&self, path: &Path) -> Result<Value, CommandError>;
}

pub trait RoutingCommandHost {
    fn load_catalog(&mut self, path: Option<&Path>) -> Result<Value, CommandError>;
    fn catalogue_snapshot(&mut self) -> Result<Value, CommandError>;
    fn route_health(&mut self) -> Result<Value, CommandError>;
    fn describe_routes(&mut self, catalog: &Value) -> Result<OrderedValue, CommandError>;
    fn quota_states(&mut self) -> Result<Value, CommandError>;
    fn read_cooldowns(&mut self) -> Result<Value, CommandError>;
    fn clear_cooldowns(
        &mut self,
        selector: Option<&str>,
        account: Option<&str>,
    ) -> Result<Vec<String>, CommandError>;
    fn route_pick(&mut self, requests: Value, seed: Option<i64>) -> Result<Value, CommandError>;
    fn select(
        &mut self,
        args: &SelectArgs,
        policy: &BTreeMap<String, Value>,
    ) -> Result<SelectionOutput, CommandError>;
    fn retired_max(&mut self) -> Result<CommandOutput, CommandError>;
}
fn output(value: Value, pretty: bool, code: i32) -> CommandOutput {
    CommandOutput {
        code,
        stdout: format!(
            "{}\n",
            crate::pending::json_dumps(&value.into(), true, pretty.then_some(2))
        ),
        stderr: String::new(),
    }
}
fn health(catalog: &Value, host: &mut impl RoutingCommandHost) -> Value {
    cli::health_record(catalog, host.route_health().ok())
}

pub fn dispatch_prefix(
    parsed: ParsedCommand,
    stdin: &str,
    defaults: &CliDefaults,
    host: &mut impl RoutingCommandHost,
) -> Result<Dispatch, CommandError> {
    let mut command = match parsed {
        ParsedCommand::LegacyPick(route) => {
            let mut result = host.route_pick(json!([{"route":route}]), None)?;

            if result.get("results").is_none() {
                return Ok(Dispatch::Complete(output(result, false, 2)));
            }
            let rows = result
                .get_mut("results")
                .and_then(Value::as_array_mut)
                .ok_or(CommandError::Malformed("pick results"))?;
            let picked = rows
                .first_mut()
                .ok_or(CommandError::Malformed("empty pick results"))?;
            let ok = picked.get("status").and_then(Value::as_str) == Some("ok");
            if ok {
                let quota = host.quota_states()?;
                let mut warnings = vec![];
                if let Some(picks) = picked.get_mut("picks").and_then(Value::as_array_mut) {
                    for pick in picks {
                        let adapter = pick
                            .get("adapter")
                            .and_then(Value::as_str)
                            .ok_or(CommandError::Malformed("pick adapter"))?;
                        if let Some(state) = quota.get(adapter).filter(|v| python_truth(v)) {
                            let mut observation = serde_json::Map::new();
                            for key in ["account", "state", "summary", "observed_at"] {
                                if let Some(value) = state.get(key) {
                                    observation.insert(key.into(), value.clone());
                                }
                            }
                            if state.get("state").and_then(Value::as_str).is_some_and(|s| {
                                ["ahead_of_pace", "deficit", "credit_only"].contains(&s)
                            }) {
                                let summary = state
                                    .get("summary")
                                    .ok_or(CommandError::Malformed("quota summary"))?;
                                warnings.push(Value::String(format!(
                                    "{adapter} quota: {}",
                                    provenant_core::py::python_str(summary)
                                )));
                            }
                            pick.as_object_mut()
                                .ok_or(CommandError::Malformed("pick object"))?
                                .insert("quota".into(), Value::Object(observation));
                        }
                    }
                }
                if !warnings.is_empty() {
                    let map = picked
                        .as_object_mut()
                        .ok_or(CommandError::Malformed("pick result"))?;
                    let existing = map.entry("warnings").or_insert_with(|| json!([]));
                    existing
                        .as_array_mut()
                        .ok_or(CommandError::Malformed("pick warnings"))?
                        .extend(warnings);
                }
            }
            return Ok(Dispatch::Complete(output(
                result,
                false,
                if ok { 0 } else { 2 },
            )));
        }
        ParsedCommand::Command(c) => *c,
    };
    defaults.apply(&mut command);
    if let RouteCommand::Resolve(args) = &mut command {
        if let Some(alias) = &args.alias {
            // The alias preflight deliberately case-folds without stripping.
            let folded = crate::casefold::fold(alias);
            if let Some((_, target)) = crate::pool_migration::RENAMED_TIERS
                .iter()
                .find(|(old, _)| *old == folded)
            {
                return Ok(Dispatch::Complete(CommandOutput{code:2,stdout:String::new(),stderr:format!("model-route: alias '{alias}' was renamed to '{target}'; fix: pass {target}\n")}));
            }
            if crate::catalog::ALIASES.contains(&folded.as_str()) {
                args.alias = Some(folded);
            }
        }
        if args
            .effort
            .as_ref()
            .is_some_and(|s| crate::casefold::fold(s) == "max")
        {
            return host.retired_max().map(Dispatch::Complete);
        }
    }
    let result = match command {
        RouteCommand::Select(args) => {
            let catalog = host.load_catalog(
                args.catalog
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .map(Path::new),
            )?;
            let mut policies = BTreeMap::new();
            if let Some(routes) = catalog.get("task_class_routes").and_then(Value::as_object) {
                for (name, route) in routes.iter().filter(|(_, r)| r.is_object()) {
                    policies.insert(name.clone(),json!({"minimum_alias":route.get("alias"),"minimum_effort":route.get("minimum_effort").or_else(||route.get("effort")),"role":if args.role.is_empty(){"worker"}else{&args.role}}));
                }
            }
            let selection = host.select(&args, &policies)?;
            CommandOutput {
                code: selection.code,
                stdout: selection.stdout()?,
                stderr: String::new(),
            }
        }
        RouteCommand::Snapshot(_) => {
            let mut record = host.catalogue_snapshot()?;
            record
                .as_object_mut()
                .ok_or(CommandError::Malformed("catalogue snapshot"))?
                .remove("catalogue");
            output(record, false, 0)
        }
        RouteCommand::Health(args) => {
            let catalog = host.load_catalog(None)?;
            output(health(&catalog, host), !args.json, 0)
        }
        RouteCommand::Routes(args) => {
            let catalog = host.load_catalog(None)?;
            let mut document = host.describe_routes(&catalog)?;
            if args.health {
                let value = health(&catalog, host);
                match &mut document {
                    OrderedValue::Object(map) => map.insert("health".into(), value.into()),
                    _ => return Err(CommandError::Malformed("routes document")),
                }
            }
            if args.json {
                output(document.to_value(), false, 0)
            } else {
                let mut text = crate::render::render(&document, args.all)
                    .map_err(|e| CommandError::Value(e.to_string()))?;
                text.push('\n');
                if args.health {
                    text.push_str("\ntask-class routes and recent health (route health --json):\n");
                    text.push_str(&crate::pending::json_dumps(
                        document
                            .get("health")
                            .ok_or(CommandError::Malformed("route health"))?,
                        true,
                        Some(2),
                    ));
                    text.push('\n');
                }
                CommandOutput {
                    code: 0,
                    stdout: text,
                    stderr: String::new(),
                }
            }
        }
        RouteCommand::Cooldowns(args) => {
            let rows = host.read_cooldowns()?;
            output(
                json!({"schema":"fabric.cooldowns.active.v1","cooldowns":rows}),
                !args.json,
                0,
            )
        }
        RouteCommand::Reset(args) => {
            match host.clear_cooldowns(args.selector.as_deref(), args.account.as_deref()) {
                Ok(removed) => {
                    if args.json {
                        output(
                            json!({"schema":"fabric.cooldown-reset.v1","removed":removed}),
                            false,
                            0,
                        )
                    } else {
                        CommandOutput {
                            code: 0,
                            stdout: format!(
                                "{}\n",
                                if removed.is_empty() {
                                    "no cooldowns matched".into()
                                } else {
                                    removed.join("\n")
                                }
                            ),
                            stderr: String::new(),
                        }
                    }
                }
                Err(CommandError::Value(error)) => CommandOutput {
                    code: 2,
                    stdout: String::new(),
                    stderr: format!("model-route reset: {error}\n"),
                },
                Err(e) => return Err(e),
            }
        }
        RouteCommand::Pick(args) => {
            let raw = if stdin.is_empty() { "{}" } else { stdin };
            let requests = match provenant_records::checked_json::from_str(raw) {
                Ok(value) => value.get("requests").cloned().unwrap_or(Value::Null),
                Err(provenant_records::DecodeError::NumberOutOfRange(e)) => {
                    return Err(CommandError::NumberOutOfRange(e))
                }
                Err(_) => Value::Null,
            };
            output(host.route_pick(requests, args.seed)?, false, 0)
        }
        remaining => return Ok(Dispatch::Continue(remaining)),
    };
    Ok(Dispatch::Complete(result))
}

/// The probe branch at the start of CLI unit-02 shares an explicit monotonic
/// clock and passes listing-only's exact 30-second deadline to its process owner.
pub trait RoutingProbeHost {
    fn probe(
        &mut self,
        adapter: &str,
        executable: &str,
        deadline: Option<f64>,
        listing_only: bool,
    ) -> Result<(Value, i32), CommandError>;
}
pub fn dispatch_probe(
    args: &cli::ProbeArgs,
    now_monotonic: f64,
    host: &mut impl RoutingProbeHost,
) -> Result<CommandOutput, CommandError> {
    let (record, code) = host.probe(
        &args.adapter,
        &args.executable,
        args.listing_only.then_some(now_monotonic + 30.0),
        args.listing_only,
    )?;
    Ok(output(record, false, code))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Host {
        health_fails: bool,
    }
    fn catalogue() -> Value {
        json!({"task_class_routes":{"work":{"alias":"senior","effort":"high"}}})
    }
    impl RoutingCommandHost for Host {
        fn load_catalog(&mut self, _: Option<&Path>) -> Result<Value, CommandError> {
            Ok(catalogue())
        }
        fn catalogue_snapshot(&mut self) -> Result<Value, CommandError> {
            Ok(json!({"schema":"fabric.catalogue.v1","catalogue":catalogue(),"date":"é"}))
        }
        fn route_health(&mut self) -> Result<Value, CommandError> {
            if self.health_fails {
                Err(CommandError::Value("unreadable".into()))
            } else {
                Ok(json!({"x":{"status":"ok"}}))
            }
        }
        fn describe_routes(&mut self, _: &Value) -> Result<OrderedValue, CommandError> {
            Ok(json!({"schema":"fabric.routes.v1","routes":{"senior":[{"model":"codex/x","weight":"high","family":"openai","availability":"available"}]},"synonyms":{"build":"senior"}}).into())
        }
        fn quota_states(&mut self) -> Result<Value, CommandError> {
            Ok(
                json!({"codex":{"account":"main","state":"deficit","summary":"behind","observed_at":"clock"}}),
            )
        }
        fn read_cooldowns(&mut self) -> Result<Value, CommandError> {
            Ok(json!({"codex/*":{"cooling_until":"tomorrow"}}))
        }
        fn clear_cooldowns(
            &mut self,
            selector: Option<&str>,
            _: Option<&str>,
        ) -> Result<Vec<String>, CommandError> {
            match selector {
                Some("bad") => Err(CommandError::Value("invalid selector".into())),
                Some("none") => Ok(vec![]),
                _ => Ok(vec!["codex/x".into()]),
            }
        }
        fn route_pick(
            &mut self,
            requests: Value,
            seed: Option<i64>,
        ) -> Result<Value, CommandError> {
            Ok(
                json!({"requests":requests,"seed":seed,"results":[{"status":"ok","picks":[{"adapter":"codex","model":"x"}],"warnings":[]}]}),
            )
        }
        fn select(
            &mut self,
            _: &SelectArgs,
            _: &BTreeMap<String, Value>,
        ) -> Result<SelectionOutput, CommandError> {
            Err(CommandError::Malformed(
                "selection not used by this fixture",
            ))
        }
        fn retired_max(&mut self) -> Result<CommandOutput, CommandError> {
            Ok(output(
                json!({"status":"invalid_effort","message":"max retired"}),
                false,
                2,
            ))
        }
    }
    #[test]
    fn dispatcher_matches_python_bytes_and_exit_codes_for_read_commands_and_reset() {
        let cases = vec![
            vec!["snapshot"],
            vec!["health"],
            vec!["health", "--json"],
            vec!["routes"],
            vec!["routes", "--health"],
            vec!["routes", "--health", "--json"],
            vec!["cooldowns"],
            vec!["cooldowns", "--json"],
            vec!["reset"],
            vec!["reset", "--json"],
            vec!["reset", "none"],
            vec!["reset", "bad"],
            vec!["pick", "--seed", "9"],
            vec!["senior"],
            vec!["resolve", "--alias", "SCOUT"],
            vec!["resolve", "--alias", "senior", "--effort", "MAX"],
        ];
        let Some(expected): Option<Value> = crate::oracle::python(
            r#"
import pathlib,sys,json,types,contextlib,io
sys.path.insert(0,str(pathlib.Path('scripts').resolve()))
import model_route_cli as m,model_route_pools as pools,model_route_preferences as preferences
def catalog():return {'task_class_routes':{'work':{'alias':'senior','effort':'high'}}}
def clear(selector,account=None):
 if selector=='bad':raise ValueError('invalid selector')
 return [] if selector=='none' else ['codex/x']
def pick(requests,seed=None):return {'requests':requests,'seed':seed,'results':[{'status':'ok','picks':[{'adapter':'codex','model':'x'}],'warnings':[]}]}
def emit(record,code):print(json.dumps(record,sort_keys=True));return code
router=types.SimpleNamespace(COMPATIBILITY_PATH=pathlib.Path('/compat.yaml'),INSTANCE_ROOT=pathlib.Path('/instance'),ALIAS_ORDER={'junior':0,'senior':1,'principal':2},EFFORT_ORDER={'low':0,'high':1},_preferences=preferences,load_catalog=lambda *a:catalog(),catalogue_snapshot=lambda:{'schema':'fabric.catalogue.v1','catalogue':catalog(),'date':'é'},_router=lambda:None,route_pick=pick,emit=emit,_max_retired=lambda:{'status':'invalid_effort','message':'max retired'})
router._fabric_records=lambda:types.SimpleNamespace(read_route_health=lambda:{'x':{'status':'ok'}},read_cooldowns=lambda:{'codex/*':{'cooling_until':'tomorrow'}},clear_cooldowns=clear)
router.model_route_live=types.SimpleNamespace(_quota_states=lambda:{'codex':{'account':'main','state':'deficit','summary':'behind','observed_at':'clock'}})
router._pools=types.SimpleNamespace(RENAMED_TIERS=pools.RENAMED_TIERS,Availability=lambda *a:None,render=pools.render,describe=lambda *a:{'schema':'fabric.routes.v1','routes':{'senior':[{'model':'codex/x','weight':'high','family':'openai','availability':'available'}]},'synonyms':{'build':'senior'}})
out=[]
for args in json.load(sys.stdin):
 stdout=io.StringIO();stderr=io.StringIO();sys.stdin=io.StringIO('{"requests":[]}')
 with contextlib.redirect_stdout(stdout),contextlib.redirect_stderr(stderr):code=m._main(args,router)
 out.append([code,stdout.getvalue(),stderr.getvalue()])
print(json.dumps(out))
"#,
            &json!(cases),
        ) else {
            return;
        };
        let defaults = CliDefaults {
            instance_root: "/instance".into(),
            compatibility_path: "/compat.yaml".into(),
        };
        let actual: Vec<_> = cases
            .iter()
            .map(|args| {
                let mut host = Host {
                    health_fails: false,
                };
                let Dispatch::Complete(result) = dispatch_prefix(
                    cli::parse_command(args.iter().copied()).unwrap(),
                    r#"{"requests":[]}"#,
                    &defaults,
                    &mut host,
                )
                .unwrap() else {
                    panic!("early command escaped")
                };
                json!([result.code, result.stdout, result.stderr])
            })
            .collect();
        assert_eq!(json!(actual), expected);
    }
    #[test]
    fn defaults_are_injected_and_unfinished_resolution_remains_a_continuation() {
        let defaults = CliDefaults {
            instance_root: "/instance".into(),
            compatibility_path: "/current-config.json".into(),
        };
        let mut parsed = cli::parse_command([
            "select",
            "--task-class",
            "work",
            "--role",
            "worker",
            "--candidates-file",
            "c",
            "--spread-state-file",
            "s",
        ])
        .unwrap();
        if let ParsedCommand::Command(command) = &mut parsed {
            defaults.apply(command);
            let RouteCommand::Select(args) = command.as_ref() else {
                panic!()
            };
            assert_eq!(
                args.preferences_file.as_deref(),
                Some("/instance/config/model-preferences.json")
            );
        }
        let Dispatch::Continue(RouteCommand::Resolve(args)) = dispatch_prefix(
            cli::parse_command(["resolve", "--alias", "SENIOR"]).unwrap(),
            "",
            &defaults,
            &mut Host {
                health_fails: false,
            },
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(args.alias.as_deref(), Some("senior"));
        assert_eq!(
            args.adapter_compatibility.as_deref(),
            Some("/current-config.json")
        );
        let Dispatch::Complete(result) = dispatch_prefix(
            cli::parse_command(["health", "--json"]).unwrap(),
            "",
            &defaults,
            &mut Host { health_fails: true },
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(
            serde_json::from_str::<Value>(&result.stdout).unwrap()["routes"],
            json!({})
        );
    }
    #[test]
    fn probe_branch_passes_the_explicit_deadline_and_provider_exit_code() {
        struct ProbeHost;
        impl RoutingProbeHost for ProbeHost {
            fn probe(
                &mut self,
                adapter: &str,
                executable: &str,
                deadline: Option<f64>,
                listing_only: bool,
            ) -> Result<(Value, i32), CommandError> {
                assert_eq!(adapter, "kiro");
                assert_eq!(executable, "kiro-cli");
                assert_eq!(deadline, listing_only.then_some(42.25));
                Ok((json!({"status":"denied"}), 7))
            }
        }
        for listing_only in [false, true] {
            let out = dispatch_probe(
                &cli::ProbeArgs {
                    adapter: "kiro".into(),
                    executable: "kiro-cli".into(),
                    json: false,
                    listing_only,
                },
                12.25,
                &mut ProbeHost,
            )
            .unwrap();
            assert_eq!(
                out,
                CommandOutput {
                    code: 7,
                    stdout: "{\"status\": \"denied\"}\n".into(),
                    stderr: String::new()
                }
            );
        }
    }
}
