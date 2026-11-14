//! Names why a launch was refused before admission, from the lane record that
//! already holds it. Display only: the checkpoint keeps its fixed sentence.
use super::*;
use serde_json::Value;

const MAX_DETAIL_CHARS: usize = 1024;

impl Factory {
    /// Append the recorded route, account and reason to each node's
    /// `preparation_refused` message in a graph reply.
    pub(crate) fn name_refusal_causes(&self, result: &mut Value) {
        let Some(nodes) = result["checkpoint"]["nodes"].as_array_mut() else {
            return;
        };
        for node in nodes {
            if node["advance_error_code"] != "preparation_refused" {
                continue;
            }
            let Some(reference) = node["attempts"]
                .as_array()
                .and_then(|attempts| attempts.last())
                .and_then(|attempt| attempt["attempt_ref"].as_str())
            else {
                continue;
            };
            let Some(detail) = self.recorded_refusal(reference) else {
                continue;
            };
            let base = node["advance_error"].as_str().unwrap_or_default();
            if base.contains(&detail) {
                continue;
            }
            let text = format!("{base} Refused: {detail}");
            node["advance_error"] = Value::String(text.chars().take(MAX_DETAIL_CHARS).collect());
        }
    }

    fn recorded_refusal(&self, reference: &str) -> Option<String> {
        let (host, attempt) = reference.rsplit_once('/')?;
        if host != self.context.identity.host.to_string() {
            return None;
        }
        let target = provenant_fabric_runtime::coordination::facade::AttemptTarget {
            extra: Default::default(),
            attempt: attempt.parse().ok()?,
            host: None,
        };
        let owner = run_owner::BinaryRunOwner {
            factory: self,
            allow_remote: false,
            actor: None,
            chair: None,
        };
        let record = owner
            .snapshots_local(&[target], &CancelToken::new())
            .ok()?
            .into_iter()
            .next()?;
        let routes: Vec<String> = record
            .skipped_routes
            .iter()
            .map(provenant_records::run::SkippedRoute::describe)
            .collect();
        if !routes.is_empty() {
            return Some(routes.join("; "));
        }
        match record.state {
            provenant_records::run::AttemptState::Terminal { extra, reason, .. } => extra
                .get("terminal_note")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or(reason),
            _ => None,
        }
    }
}
