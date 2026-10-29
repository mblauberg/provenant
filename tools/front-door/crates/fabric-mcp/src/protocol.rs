//! Connection-local MCP lifecycle. No execution or I/O occurs here.
use crate::tools::{InvalidInput, ToolCall, tool_error};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
pub const LEGACY: &str = "2025-11-25";
pub const MODERN: &str = "2026-07-28";
pub const VERSION: &str = "io.modelcontextprotocol/protocolVersion";
pub const CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
pub const CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
pub const LOG_LEVEL: &str = "io.modelcontextprotocol/logLevel";
pub const SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";
pub const MAX_FRAME: usize = 1_048_576;
const TTL_MS: u64 = 300_000;
const CACHE_SCOPE: &str = "public";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Id {
    Text(String),
    Signed(i64),
    Unsigned(u64),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Id>,
    method: String,
    #[serde(default)]
    params: BTreeMap<String, Value>,
}
#[derive(Deserialize)]
struct Initialise {
    #[serde(rename = "protocolVersion")]
    _version: String,
    #[serde(rename = "capabilities")]
    _capabilities: BTreeMap<String, Value>,
    #[serde(rename = "clientInfo")]
    _client: Implementation,
}
#[derive(Deserialize)]
struct Implementation {
    #[serde(rename = "name")]
    _name: String,
    #[serde(rename = "version")]
    _version: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    name: String,
    arguments: Value,
}
#[derive(Deserialize)]
struct Cancel {
    #[serde(rename = "requestId")]
    id: Id,
}
#[derive(Debug, Clone, Copy)]
enum LegacyState {
    Initialised,
    Ready,
}
#[derive(Debug, Clone, Copy)]
enum Era {
    Legacy(LegacyState),
    Modern,
}
#[derive(Default)]
pub struct Protocol {
    era: Option<Era>,
    usage_tool: Option<&'static str>,
}
pub enum Action {
    Reply(Value),
    Ignore,
    Call {
        id: Id,
        modern: bool,
        call: Box<ToolCall>,
    },
    Cancel(Id),
}
pub fn error(id: Option<&Id>, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
pub fn uncorrelated_error(code: i64, message: &str, modern: bool) -> Value {
    if modern {
        json!({"jsonrpc":"2.0","error":{"code":code,"message":message}})
    } else {
        error(None, code, message)
    }
}
pub fn result(id: &Id, mut value: Value, modern: bool) -> Value {
    if modern && let Some(object) = value.as_object_mut() {
        object.insert("resultType".into(), json!("complete"));
        object.entry("_meta").or_insert_with(|| json!({}))[SERVER_INFO] = server_info();
    }
    json!({"jsonrpc":"2.0","id":id,"result":value})
}
fn server_info() -> Value {
    json!({"name":"provenant-fabric","version":env!("CARGO_PKG_VERSION")})
}
impl Protocol {
    pub(crate) fn usage_tool(&self) -> Option<&'static str> {
        self.usage_tool
    }
    pub fn modern(&self) -> bool {
        matches!(self.era, Some(Era::Modern))
    }
    pub fn receive(&mut self, bytes: &[u8]) -> Action {
        self.receive_with_limits(
            bytes,
            provenant_records::Limits {
                max_bytes: MAX_FRAME as u64,
                max_depth: 64,
                max_members: 100_000,
            },
        )
    }
    pub fn receive_with_limits(
        &mut self,
        bytes: &[u8],
        mut limits: provenant_records::Limits,
    ) -> Action {
        self.usage_tool = None;
        limits.max_bytes = limits.max_bytes.min(MAX_FRAME as u64);
        limits.max_depth = limits.max_depth.min(64);
        limits.max_members = limits.max_members.min(100_000);
        let value: Value = match provenant_records::decode(bytes, limits) {
            Ok(value) => value,
            Err(_) => {
                return Action::Reply(uncorrelated_error(
                    -32700,
                    "Invalid bounded JSON frame",
                    self.modern(),
                ));
            }
        };
        let id = value
            .get("id")
            .and_then(|v| serde_json::from_value::<Id>(v.clone()).ok());
        if value.get("id").is_some() && id.is_none() {
            return Action::Reply(uncorrelated_error(
                -32600,
                "Invalid request ID",
                self.modern(),
            ));
        }
        let envelope: Envelope = match serde_json::from_value(value) {
            Ok(envelope) => envelope,
            Err(_) => return Action::Reply(self.invalid(id.as_ref(), "Invalid JSON-RPC request")),
        };
        if envelope.jsonrpc != "2.0" {
            return Action::Reply(self.invalid(id.as_ref(), "Invalid JSON-RPC version"));
        }
        match envelope.id {
            None => self.notification(envelope.method.as_str(), envelope.params),
            Some(id) => self.request(id, envelope.method.as_str(), envelope.params),
        }
    }
    fn invalid(&self, id: Option<&Id>, message: &str) -> Value {
        match id {
            Some(id) => error(Some(id), -32600, message),
            None => uncorrelated_error(-32600, message, self.modern()),
        }
    }
    fn notification(&mut self, method: &str, params: BTreeMap<String, Value>) -> Action {
        match method {
            "notifications/initialized" => {
                if let Some(Era::Legacy(state)) = &mut self.era
                    && matches!(*state, LegacyState::Initialised)
                {
                    *state = LegacyState::Ready;
                }
                Action::Ignore
            }
            "notifications/cancelled" => serde_json::from_value::<Cancel>(json!(params))
                .map_or(Action::Ignore, |cancel| Action::Cancel(cancel.id)),
            _ => Action::Ignore,
        }
    }
    fn request(&mut self, id: Id, method: &str, mut params: BTreeMap<String, Value>) -> Action {
        let modern_meta = params.get("_meta").and_then(|m| m.get(VERSION));
        let selected = if method == "initialize" {
            Some(Era::Legacy(LegacyState::Initialised))
        } else if modern_meta.is_some() {
            Some(Era::Modern)
        } else {
            None
        };
        let refuse = |code, message| Action::Reply(error(Some(&id), code, message));
        let newly_selected = self.era.is_none();
        let Some(era) = self.era.or(selected) else {
            return refuse(-32600, "First request must select a protocol era");
        };
        let modern = matches!(era, Era::Modern);
        if (modern && method == "initialize") || (!modern && modern_meta.is_some()) {
            return refuse(-32600, "Protocol era switch refused");
        }
        if modern {
            let Some(meta) = params.remove("_meta").filter(Value::is_object) else {
                return refuse(-32602, "Required request metadata missing");
            };
            let Some(version) = meta.get(VERSION).and_then(Value::as_str) else {
                return refuse(-32602, "Protocol version must be a string");
            };
            if !meta.get(CAPABILITIES).is_some_and(Value::is_object) {
                return refuse(-32602, "Required client capabilities missing");
            }
            if let Some(client_info) = meta.get(CLIENT_INFO)
                && serde_json::from_value::<Implementation>(client_info.clone()).is_err()
            {
                return refuse(-32602, "Invalid client information metadata");
            }
            if let Some(log_level) = meta.get(LOG_LEVEL) {
                let Some(log_level) = log_level.as_str() else {
                    return refuse(-32602, "Log level metadata must be a string");
                };
                if !matches!(
                    log_level,
                    "debug"
                        | "info"
                        | "notice"
                        | "warning"
                        | "error"
                        | "critical"
                        | "alert"
                        | "emergency"
                ) {
                    return refuse(-32602, "Invalid log level metadata");
                }
            }
            if version != MODERN {
                let mut reply = error(Some(&id), -32022, "Unsupported protocol version");
                reply["error"]["data"] = json!({"supported":[MODERN,LEGACY],"requested":version});
                return Action::Reply(reply);
            }
            if matches!(method, "ping" | "notifications/initialized") {
                return refuse(-32601, "Method unavailable in this protocol era");
            }
        } else {
            // Legacy metadata such as progressToken is optional and does not select an era.
            params.remove("_meta");
        }
        if method == "initialize" {
            if !newly_selected || !matches!(era, Era::Legacy(_)) {
                return refuse(-32600, "Already initialized");
            }
            if serde_json::from_value::<Initialise>(json!(params)).is_err() {
                return refuse(-32602, "Invalid initialize parameters");
            }
            self.era = Some(Era::Legacy(LegacyState::Initialised));
            return Action::Reply(result(
                &id,
                json!({"protocolVersion":LEGACY,"capabilities":{"tools":{}},"serverInfo":server_info()}),
                false,
            ));
        }
        if !modern && method != "ping" && !matches!(era, Era::Legacy(LegacyState::Ready)) {
            return refuse(-32600, "Initialisation incomplete");
        }
        let value = match method {
            "ping" if !modern => {
                if !params.is_empty() {
                    return refuse(-32602, "Unexpected ping parameters");
                }
                json!({})
            }
            "server/discover" if modern => {
                if !params.is_empty() {
                    return refuse(-32602, "Unexpected discovery parameters");
                }
                json!({"supportedVersions":[MODERN,LEGACY],"capabilities":{"tools":{}},"ttlMs":TTL_MS,"cacheScope":CACHE_SCOPE})
            }
            "tools/list" => {
                if !params.is_empty() {
                    return refuse(-32602, "Unexpected catalogue cursor or parameters");
                }
                let tools = if modern {
                    crate::tools::catalogue()
                } else {
                    crate::tools::legacy_catalogue()
                };
                let mut list = json!({"tools":tools});
                if modern {
                    list["ttlMs"] = json!(TTL_MS);
                    list["cacheScope"] = json!(CACHE_SCOPE);
                }
                list
            }
            "tools/call" => {
                let call: Call = match serde_json::from_value(json!(params)) {
                    Ok(call) => call,
                    Err(_) => return refuse(-32602, "Invalid tool call envelope"),
                };
                self.usage_tool = Some(ToolCall::usage_name(&call.name));
                let call = match ToolCall::parse(&call.name, call.arguments.clone()) {
                    Ok(call) => call,
                    Err(InvalidInput::UnknownTool) => return refuse(-32602, "Unknown tool name"),
                    Err(InvalidInput::Arguments { path }) => {
                        let message = match crate::schema::near_argument(
                            &call.name,
                            &call.arguments,
                            &path,
                        ) {
                            Some(near) => {
                                format!("Invalid argument at {path}; did you mean '{near}'?")
                            }
                            None => format!(
                                "Invalid argument at {path}; expected {}",
                                crate::schema::expected(&call.name, &call.arguments, &path)
                            ),
                        };
                        return Action::Reply(result(
                            &id,
                            tool_error(
                                Some(&call.name),
                                "invalid_input",
                                json!({"code":"invalid_input","message":message,"path":path,"hint":"Correct the argument and retry; use fabric_discover for advanced tool schemas."}),
                            ),
                            modern,
                        ));
                    }
                };
                if newly_selected {
                    self.era = Some(Era::Modern);
                }
                return Action::Call {
                    id,
                    modern,
                    call: Box::new(call),
                };
            }
            _ => return refuse(-32601, "Method not found"),
        };
        if newly_selected {
            self.era = Some(era);
        }
        Action::Reply(result(&id, value, modern))
    }
}
#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
