//! Bounded transcript driver for the future MCP edge. Tool fixtures supply the
//! protocol version and payloads; this rig invents no provider/harness protocol.
use super::{Result, TestRig};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
#[derive(Default)]
pub struct McpClient {
    input: Vec<u8>,
    next_id: u64,
    outstanding: BTreeSet<u64>,
}
#[derive(Serialize)]
struct Request<'a> {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<u64>,
    method: &'a str,
    params: Value,
}
impl McpClient {
    fn frame(&mut self, method: &str, id: Option<u64>, params: Value) -> Result {
        let mut bytes = serde_json::to_vec(&Request {
            jsonrpc: "2.0",
            id,
            method,
            params,
        })?;
        bytes.push(b'\n');
        if self.input.len().saturating_add(bytes.len()) > 1024 * 1024 {
            return Err("MCP fixture transcript exceeds 1 MiB".into());
        }
        self.input.extend(bytes);
        Ok(())
    }
    pub fn request(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self
            .next_id
            .checked_add(1)
            .ok_or("fixture request ID overflow")?;
        self.frame(method, Some(id), params)?;
        self.next_id = id;
        self.outstanding.insert(id);
        Ok(id)
    }
    pub fn initialise(&mut self, version: &str) -> Result<u64> {
        self.request("initialize",serde_json::json!({"protocolVersion":version,"capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}))
    }
    pub fn initialised(&mut self) -> Result {
        self.frame("notifications/initialized", None, serde_json::json!({}))
    }
    pub fn list(&mut self) -> Result<u64> {
        self.request("tools/list", serde_json::json!({}))
    }
    pub fn call(&mut self, name: &str, arguments: Value) -> Result<u64> {
        self.request(
            "tools/call",
            serde_json::json!({"name":name,"arguments":arguments}),
        )
    }
    pub fn cancel(&mut self, id: u64) -> Result {
        self.frame(
            "notifications/cancelled",
            None,
            serde_json::json!({"requestId":id,"reason":"fixture cancellation"}),
        )
    }
    /// SystemExecutor closes stdin after this bounded transcript, drains both
    /// streams concurrently, and observes/reaps the owned server before return.
    pub fn eof(
        self,
        rig: &TestRig,
        executable: &Path,
        args: &[&str],
    ) -> Result<BTreeMap<u64, Value>> {
        let outcome = rig.execute(executable, args, Some(&self.input))?;
        if super::code(&outcome) != 0 || outcome.stdout.truncated || outcome.stderr.truncated {
            return Err("MCP fixture failed or overflowed".into());
        }
        let mut outstanding = self.outstanding;
        let mut replies = BTreeMap::new();
        for line in outcome
            .stdout
            .bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let message: Value = serde_json::from_slice(line)?;
            if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
                return Err("non-protocol MCP stdout".into());
            }
            if let Some(id) = message.get("id").and_then(Value::as_u64) {
                if !outstanding.remove(&id)
                    || (message.get("result").is_some() == message.get("error").is_some())
                {
                    return Err("uncorrelated or malformed MCP reply".into());
                }
                replies.insert(id, message);
            } else if !message.get("method").is_some_and(Value::is_string) {
                return Err("invalid MCP notification".into());
            }
        }
        if !outstanding.is_empty() {
            return Err("MCP EOF before all requests replied".into());
        }
        Ok(replies)
    }
}
