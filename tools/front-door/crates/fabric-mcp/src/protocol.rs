//! Connection-local MCP lifecycle. No execution or I/O occurs here.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
pub const LEGACY: &str = "2025-11-25";
pub const MODERN: &str = "2026-07-28";
pub const VERSION: &str = "io.modelcontextprotocol/protocolVersion";
pub const CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
pub const MAX_FRAME: usize = 1_048_576;

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
    version: String,
    capabilities: BTreeMap<String, Value>,
    #[serde(rename = "clientInfo")]
    client: Implementation,
}
#[derive(Deserialize)]
struct Implementation {
    name: String,
    version: String,
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
enum Era {
    Legacy { ready: bool },
    Modern,
}
#[derive(Default)]
pub struct Protocol {
    era: Option<Era>,
    legacy_initialised: bool,
}
pub enum Action {
    Reply(Value),
    Ignore,
    Call {
        id: Id,
        modern: bool,
        name: String,
        arguments: Value,
    },
    Cancel(Id),
}
pub fn error(id: Option<&Id>, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
pub fn result(id: &Id, mut value: Value, modern: bool) -> Value {
    if modern && let Some(object) = value.as_object_mut() {
        object.insert("resultType".into(), json!("complete"));
    }
    json!({"jsonrpc":"2.0","id":id,"result":value})
}
fn server_info() -> Value {
    json!({"name":"provenant-fabric","version":env!("CARGO_PKG_VERSION")})
}
impl Protocol {
    pub fn modern(&self) -> bool {
        matches!(self.era, Some(Era::Modern))
    }
    pub fn receive(&mut self, bytes: &[u8]) -> Action {
        let value: Value = match provenant_records::decode(
            bytes,
            provenant_records::Limits {
                max_bytes: MAX_FRAME as u64,
                max_depth: 64,
                max_members: 100_000,
            },
        ) {
            Ok(value) => value,
            Err(_) => return Action::Reply(error(None, -32700, "Invalid bounded JSON frame")),
        };
        let id = value
            .get("id")
            .and_then(|v| serde_json::from_value::<Id>(v.clone()).ok());
        if value.get("id").is_some() && id.is_none() {
            return Action::Reply(error(None, -32600, "Invalid request ID"));
        }
        let envelope: Envelope = match serde_json::from_value(value) {
            Ok(envelope) => envelope,
            Err(_) => return Action::Reply(error(id.as_ref(), -32600, "Invalid JSON-RPC request")),
        };
        if envelope.jsonrpc != "2.0" {
            return Action::Reply(error(id.as_ref(), -32600, "Invalid JSON-RPC version"));
        }
        match envelope.id {
            None => self.notification(envelope.method.as_str(), envelope.params),
            Some(id) => self.request(id, envelope.method.as_str(), envelope.params),
        }
    }
    fn notification(&mut self, method: &str, params: BTreeMap<String, Value>) -> Action {
        match method {
            "notifications/initialized" => {
                if self.legacy_initialised
                    && let Some(Era::Legacy { ready }) = &mut self.era
                {
                    *ready = true;
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
            Some(Era::Legacy { ready: false })
        } else if modern_meta.is_some() {
            Some(Era::Modern)
        } else {
            None
        };
        if self.era.is_none() {
            self.era = selected;
        }
        let refuse = |code, message| Action::Reply(error(Some(&id), code, message));
        let Some(era) = self.era else {
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
            if version != MODERN {
                let mut reply = error(Some(&id), -32022, "Unsupported protocol version");
                reply["error"]["data"] = json!({"supported":[MODERN,LEGACY],"requested":version});
                return Action::Reply(reply);
            }
            if !meta.get(CAPABILITIES).is_some_and(Value::is_object) {
                return refuse(-32602, "Required client capabilities missing");
            }
            if matches!(method, "ping" | "notifications/initialized") {
                return refuse(-32601, "Method unavailable in this protocol era");
            }
        } else {
            // Legacy metadata such as progressToken is optional and does not select an era.
            params.remove("_meta");
        }
        if method == "initialize" {
            if !matches!(era, Era::Legacy { ready: false }) {
                return refuse(-32600, "Already initialized");
            }
            // A successful initialize is distinct from the initialized notification.
            if self.initialised() {
                return refuse(-32600, "Already initialized");
            }
            let init: Initialise = match serde_json::from_value(json!(params)) {
                Ok(init) => init,
                Err(_) => return refuse(-32602, "Invalid initialize parameters"),
            };
            let _ = (
                init.version,
                init.capabilities,
                init.client.name,
                init.client.version,
            );
            self.legacy_initialised = true;
            return Action::Reply(result(
                &id,
                json!({"protocolVersion":LEGACY,"capabilities":{"tools":{}},"serverInfo":server_info()}),
                false,
            ));
        }
        if !modern && method != "ping" && !matches!(era, Era::Legacy { ready: true }) {
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
                json!({"supportedVersions":[MODERN,LEGACY],"capabilities":{"tools":{}},"_meta":{"io.modelcontextprotocol/serverInfo":server_info()},"ttlMs":300000,"cacheScope":"public"})
            }
            "tools/list" => {
                if !params.is_empty() {
                    return refuse(-32602, "Unexpected catalogue cursor or parameters");
                }
                let mut list = json!({"tools":crate::tools::catalogue()});
                if modern {
                    list["ttlMs"] = json!(300000);
                    list["cacheScope"] = json!("public");
                }
                list
            }
            "tools/call" => {
                let call: Call = match serde_json::from_value(json!(params)) {
                    Ok(call) => call,
                    Err(_) => return refuse(-32602, "Invalid tool call envelope"),
                };
                return Action::Call {
                    id,
                    modern,
                    name: call.name,
                    arguments: call.arguments,
                };
            }
            _ => return refuse(-32601, "Method not found"),
        };
        Action::Reply(result(&id, value, modern))
    }
    fn initialised(&self) -> bool {
        self.legacy_initialised
    }
}
#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
