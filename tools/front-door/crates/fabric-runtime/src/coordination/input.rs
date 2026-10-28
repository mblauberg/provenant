use super::facade::{GraphInput, HostsInput, IdentityInput, OutputInput, RunInput, SubmitInput};
use super::{
    ArtifactRef, ClaimInput, EventsInput, LandingInput, MailInput, TaskInput, TaskState, WorkScope,
};
use provenant_core::WaitTransport;
use provenant_records::run::RequestedRoute;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputError {
    pub path: String,
}

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid input field `{}`", self.path)?;
        if self.path.starts_with("tasks[") && self.path.ends_with("].title") {
            write!(
                f,
                ": task title must be 1 to 256 bytes and contain no control characters"
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for InputError {}

impl InputError {
    pub(crate) fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }
}

pub trait ValidateInput {
    fn validate_input(&self, transport: WaitTransport) -> Result<(), InputError>;
}

macro_rules! bounded_input {
    (
        $vis:vis enum $name:ident as $wire:ident {
            $(
                $(#[$variant_attr:meta])*
                $variant:ident {
                    $( $(#[$field_attr:meta])* $field:ident : $field_type:ty ),* $(,)?
                }
            ),* $(,)?
        }
    ) => {
        #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
        #[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
        $vis enum $name {
            $( $(#[$variant_attr])* $variant { $( $(#[$field_attr])* $field: $field_type ),* } ),*
        }

        #[derive(serde::Deserialize)]
        #[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
        enum $wire {
            $(
                $(#[$variant_attr])*
                $variant {
                    $( $(#[$field_attr])* $field: $field_type ),*
                }
            ),*
        }

        impl From<$wire> for $name {
            fn from(value: $wire) -> Self {
                match value {
                    $(
                        $wire::$variant { $( $field ),* } => Self::$variant { $( $field ),* }
                    ),*
                }
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let input = Self::from(<$wire as serde::Deserialize>::deserialize(deserializer)?);
                crate::coordination::input::ValidateInput::validate_input(&input, provenant_core::WaitTransport::Cli)
                    .map_err(serde::de::Error::custom)?;
                Ok(input)
            }
        }
    };
}

pub(crate) use bounded_input;

pub const MAX_TEXT_BYTES: usize = 65_536;
/// Largest prompt a CLI peer submission may carry.
pub const MAX_CLI_PROMPT_BYTES: usize = 1_048_576;
pub const MAX_REFERENCE_BYTES: usize = 1_024;
pub const MAX_ROUTE_BYTES: usize = 128;
pub const MAX_ARTIFACT_PATH_BYTES: usize = 4_096;
/// Leaves over 250 KiB for the MCP envelope under the 1 MiB frame cap after
/// worst-case sixfold JSON string escaping; base64 encoding stays below 2x.
pub const MAX_FETCH_BYTES: u64 = 131_072;

fn field(path: &str) -> InputError {
    InputError::new(path)
}

fn bounded_text(value: &str, path: &str, max: usize) -> Result<(), InputError> {
    if value.is_empty() || value.len() > max || value.contains('\0') {
        return Err(field(path));
    }
    Ok(())
}

fn optional_text(value: Option<&str>, path: &str, max: usize) -> Result<(), InputError> {
    if let Some(value) = value {
        bounded_text(value, path, max)?;
    }
    Ok(())
}

fn page(limit: u64, path: &str) -> Result<(), InputError> {
    if !(1..=100).contains(&limit) {
        return Err(field(path));
    }
    Ok(())
}

fn lease(seconds: u64) -> Result<(), InputError> {
    if !(1..=3_600).contains(&seconds) {
        return Err(field("lease_seconds"));
    }
    Ok(())
}

fn wait(seconds: u64, transport: WaitTransport) -> Result<(), InputError> {
    if !(1..=transport.maximum_seconds()).contains(&seconds) {
        return Err(field("wait_seconds"));
    }
    Ok(())
}

fn artifact(artifact: &ArtifactRef, path: &str) -> Result<(), InputError> {
    let encoded = serde_json::to_value(&artifact.path).map_err(|_| field(path))?;
    let value = encoded["path"].as_str().ok_or_else(|| field(path))?;
    bounded_text(value, path, MAX_ARTIFACT_PATH_BYTES)
}

fn artifacts(values: &[ArtifactRef], path: &str) -> Result<(), InputError> {
    if values.len() > 100 {
        return Err(field(path));
    }
    for (index, value) in values.iter().enumerate() {
        artifact(value, &format!("{path}[{index}].path.path"))?;
    }
    Ok(())
}

fn path_text(path: &std::path::Path, field_path: &str) -> Result<(), InputError> {
    let value = path.to_str().ok_or_else(|| field(field_path))?;
    bounded_text(value, field_path, MAX_ARTIFACT_PATH_BYTES)
}

fn scope(scope: &WorkScope) -> Result<(), InputError> {
    match scope {
        WorkScope::Issue { key } => bounded_text(key, "scope.key", MAX_REFERENCE_BYTES),
        WorkScope::Paths { paths } => {
            if paths.is_empty() || paths.len() > 100 {
                return Err(field("scope.paths"));
            }
            for (index, path) in paths.iter().enumerate() {
                path_text(path, &format!("scope.paths[{index}]"))?;
            }
            Ok(())
        }
        WorkScope::Worktree { path } => path_text(path, "scope.path"),
    }
}

fn landing_reference(reference: &str, path: &str) -> Result<(), InputError> {
    bounded_text(reference, path, MAX_REFERENCE_BYTES)
}

fn route(route: &RequestedRoute, path: &str) -> Result<(), InputError> {
    let effort = match route {
        RequestedRoute::Model {
            provider,
            model,
            effort,
        } => {
            bounded_text(provider, &format!("{path}.provider"), MAX_ROUTE_BYTES)?;
            bounded_text(model, &format!("{path}.model"), MAX_ROUTE_BYTES)?;
            effort
        }
        RequestedRoute::Named { name, effort } | RequestedRoute::Review { name, effort, .. } => {
            bounded_text(name, &format!("{path}.name"), MAX_ROUTE_BYTES)?;
            effort
        }
    };
    optional_text(
        effort.as_deref(),
        &format!("{path}.effort"),
        MAX_ROUTE_BYTES,
    )
}

fn task_text(value: &str, path: &str) -> Result<(), InputError> {
    bounded_text(value, path, MAX_TEXT_BYTES)
}

impl ValidateInput for MailInput {
    fn validate_input(&self, _transport: WaitTransport) -> Result<(), InputError> {
        match self {
            Self::Send {
                recipients,
                body,
                artifacts: values,
                ..
            } => {
                if recipients.is_empty()
                    || recipients.len() > 100
                    || recipients
                        .iter()
                        .enumerate()
                        .any(|(index, recipient)| recipients[..index].contains(recipient))
                {
                    return Err(field("recipients"));
                }
                task_text(body, "body")?;
                artifacts(values, "artifacts")?;
                Ok(())
            }
            Self::Read { after, limit } => {
                if after.is_some_and(|value| value < 0) {
                    return Err(field("after"));
                }
                page(*limit, "limit")
            }
            Self::MarkRead { through } if *through < 0 => Err(field("through")),
            Self::MarkRead { .. } => Ok(()),
        }
    }
}

impl ValidateInput for TaskInput {
    fn validate_input(&self, _transport: WaitTransport) -> Result<(), InputError> {
        match self {
            Self::Create { objective, note }
            | Self::Update {
                objective, note, ..
            } => {
                task_text(objective, "objective")?;
                optional_text(note.as_deref(), "note", MAX_TEXT_BYTES)
            }
            Self::List { state, limit, .. } => {
                if state.is_some_and(|value| value == TaskState::Active) {
                    return Err(field("state"));
                }
                page(*limit, "limit")
            }
            Self::Claim { lease_seconds, .. } => lease(*lease_seconds),
            Self::Finish { state, .. }
                if !matches!(state, TaskState::Done | TaskState::Cancelled) =>
            {
                Err(field("state"))
            }
            Self::Finish { .. } | Self::Release { .. } => Ok(()),
        }
    }
}

impl ValidateInput for EventsInput {
    fn validate_input(&self, transport: WaitTransport) -> Result<(), InputError> {
        match self {
            Self::Read {
                after,
                limit,
                wait_seconds,
            } => {
                if after.is_some_and(|value| value < 0) {
                    return Err(field("after"));
                }
                page(*limit, "limit")?;
                if let Some(milliseconds) = wait_seconds {
                    wait(*milliseconds, transport)?;
                }
                Ok(())
            }
            Self::Note { note, dedup_key } => {
                task_text(note, "note")?;
                optional_text(dedup_key.as_deref(), "dedup_key", MAX_REFERENCE_BYTES)
            }
        }
    }
}

impl ValidateInput for ClaimInput {
    fn validate_input(&self, _transport: WaitTransport) -> Result<(), InputError> {
        match self {
            Self::Acquire {
                scope: value,
                lease_seconds,
            } => {
                scope(value)?;
                lease(*lease_seconds)
            }
            Self::Renew { lease_seconds, .. } => lease(*lease_seconds),
            Self::Verify { .. } | Self::Release { .. } => Ok(()),
            Self::List { limit, .. } => page(*limit, "limit"),
        }
    }
}

impl ValidateInput for LandingInput {
    fn validate_input(&self, transport: WaitTransport) -> Result<(), InputError> {
        match self {
            Self::Acquire {
                reference,
                lease_seconds,
                ..
            } => {
                landing_reference(reference, "reference")?;
                lease(*lease_seconds)
            }
            Self::Renew {
                proof,
                lease_seconds,
            } => {
                landing_reference(&proof.reference, "proof.reference")?;
                lease(*lease_seconds)
            }
            Self::Verify { proof } | Self::Release { proof } => {
                landing_reference(&proof.reference, "proof.reference")
            }
            Self::Wait {
                reference,
                wait_seconds,
                ..
            } => {
                landing_reference(reference, "reference")?;
                wait(*wait_seconds, transport)
            }
        }
    }
}

impl ValidateInput for IdentityInput {
    fn validate_input(&self, _transport: WaitTransport) -> Result<(), InputError> {
        match self {
            Self::Inspect {} => Ok(()),
        }
    }
}

impl ValidateInput for GraphInput {
    fn validate_input(&self, _transport: WaitTransport) -> Result<(), InputError> {
        match self {
            Self::Run { document } => path_text(document, "document"),
            Self::Resume { document, extend } => {
                path_text(document, "document")?;
                if extend.len() > 64 || extend.iter().any(|entry| {
                    entry.len() > 256 || !matches!(entry.split_once('='), Some((key, value)) if !key.is_empty() && !value.is_empty())
                }) {
                    return Err(field("extend"));
                }
                Ok(())
            }
            Self::Status { id } | Self::Cancel { id } => {
                if crate::graph::valid_identifier(id) {
                    Ok(())
                } else {
                    Err(field("id"))
                }
            }
        }
    }
}

impl ValidateInput for SubmitInput {
    fn validate_input(&self, transport: WaitTransport) -> Result<(), InputError> {
        match self {
            Self::Submit {
                tasks,
                concurrency,
                knobs,
                ..
            } => {
                if tasks.is_empty() || tasks.len() > 64 {
                    return Err(field("tasks"));
                }
                if concurrency.is_some_and(|n| n == 0 || n > 16) {
                    return Err(field("concurrency"));
                }
                if knobs.len() > 64 {
                    return Err(field("knobs"));
                }
                for (index, knob) in knobs.iter().enumerate() {
                    bounded_text(knob, &format!("knobs[{index}]"), 256)?;
                }
                for (index, task) in tasks.iter().enumerate() {
                    let path = format!("tasks[{index}]");
                    // Peer dispatch uses the CLI launch limit; MCP retains its
                    // smaller tool input bound.
                    let prompt_limit = match transport {
                        WaitTransport::Cli => MAX_CLI_PROMPT_BYTES,
                        WaitTransport::Mcp => MAX_TEXT_BYTES,
                    };
                    bounded_text(&task.prompt, &format!("{path}.prompt"), prompt_limit)?;
                    if let Some(title) = &task.title {
                        provenant_core::validate_task_title(title)
                            .map_err(|_| field(&format!("{path}.title")))?;
                    }
                    optional_text(
                        task.base.as_deref(),
                        &format!("{path}.base"),
                        MAX_REFERENCE_BYTES,
                    )?;
                    optional_text(task.head.as_deref(), &format!("{path}.head"), 64)?;
                    optional_text(task.account.as_deref(), &format!("{path}.account"), 64)?;
                    optional_text(
                        task.worktree.as_deref(),
                        &format!("{path}.worktree"),
                        MAX_ARTIFACT_PATH_BYTES,
                    )?;
                    if let Some(deliver) = &task.deliver {
                        bounded_text(&deliver.remote, &format!("{path}.deliver.remote"), 64)?;
                        bounded_text(&deliver.branch, &format!("{path}.deliver.branch"), 1024)?;
                    }
                    route(&task.route, &format!("{path}.route"))?;
                    if !(1..=14_400).contains(&task.timeout_seconds) {
                        return Err(field(&format!("{path}.timeout_seconds")));
                    }
                }
                Ok(())
            }
        }
    }
}

impl ValidateInput for RunInput {
    fn validate_input(&self, transport: WaitTransport) -> Result<(), InputError> {
        match self {
            Self::List { limit, .. } => page(u64::from(*limit), "limit"),
            Self::Status { .. } | Self::Cancel { .. } => Ok(()),
            Self::Wait {
                targets,
                wait_seconds,
                ..
            } => {
                if targets.is_empty() || targets.len() > 100 {
                    return Err(field("targets"));
                }
                wait(*wait_seconds, transport)
            }
            Self::Resume { prompt, .. } => task_text(prompt, "prompt"),
        }
    }
}

impl ValidateInput for OutputInput {
    fn validate_input(&self, _transport: WaitTransport) -> Result<(), InputError> {
        match self {
            Self::Read { max_bytes, .. } if !(1..=65_536).contains(max_bytes) => {
                Err(field("max_bytes"))
            }
            Self::Read { .. } => Ok(()),
            Self::Fetch {
                artifact: value,
                max_bytes,
                ..
            } => {
                if !(1..=MAX_FETCH_BYTES).contains(max_bytes) {
                    return Err(field("max_bytes"));
                }
                artifact(value, "artifact.path.path")
            }
        }
    }
}

impl ValidateInput for HostsInput {
    fn validate_input(&self, _transport: WaitTransport) -> Result<(), InputError> {
        match self {
            Self::Hosts { limit, .. } => page(u64::from(*limit), "limit"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::facade::{GraphInput, OutputInput, RunInput, SubmitInput};
    use super::super::{
        ClaimInput, CoordinationApi, CoordinationError, CoordinationService, LandingInput,
        TaskInput,
    };
    use provenant_core::{
        ActorId, CancelToken, Clock, MonoTime, PathResolver, ProjectId, RootInputs, TestClock,
        WallTime,
    };
    use provenant_fabric_store::{Request, Store, ThreadWait};
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};
    use std::path::{Path, PathBuf};

    fn rejects<T: DeserializeOwned + std::fmt::Debug>(value: Value, path: &str) {
        let error = serde_json::from_value::<T>(value).unwrap_err().to_string();
        assert!(error.contains(path), "expected `{path}` in `{error}`");
    }

    /// A one-task submit with this `concurrency`, as the CLI and store see it.
    fn one_task_submit_is_valid(concurrency: u64) -> bool {
        use super::ValidateInput;
        serde_json::from_value::<SubmitInput>(json!({
            "op":"submit","operation_id":"work-1","concurrency":concurrency,
            "tasks":[{"prompt":"do work","route":{"kind":"named","name":"utility"},"timeout_seconds":1}]
        }))
        .is_ok_and(|input| input.validate_input(provenant_core::WaitTransport::Cli).is_ok())
    }

    #[test]
    fn graph_resume_extensions_require_key_value_pairs() {
        for entry in ["nodes", "=10", "nodes="] {
            rejects::<GraphInput>(
                json!({"op":"resume", "document":"graph.json", "extend":[entry]}),
                "extend",
            );
        }
        assert!(serde_json::from_value::<GraphInput>(json!({
            "op":"resume", "document":"graph.json", "extend":["nodes=1024", "graph_tokens=10000"]
        })).is_ok());
    }

    #[test]
    fn submit_rejects_control_characters_in_titles() {
        for title in ["line\nbreak", "tab\there", "escape\u{1b}", "unicode\u{85}"] {
            let error = serde_json::from_value::<SubmitInput>(json!({
                "op":"submit", "operation_id":"work-1",
                "tasks":[{"prompt":"do work", "title":title, "route":{"kind":"named","name":"utility"}, "timeout_seconds":1}]
            })).unwrap_err().to_string();
            assert!(
                error.contains("tasks[0].title") && error.contains("control characters"),
                "{error}"
            );
        }
    }

    #[test]
    fn concurrency_above_the_task_count_is_accepted_up_to_sixteen() {
        // The CLI and the run store accept 16 for one task, so a peer must too.
        for accepted in [1, 2, 16] {
            assert!(one_task_submit_is_valid(accepted), "{accepted}");
        }
        for refused in [0, 17] {
            assert!(!one_task_submit_is_valid(refused), "{refused}");
        }
    }

    #[test]
    fn receipts_freeze_the_public_request() {
        let proof = super::super::ClaimProof {
            id: provenant_core::ClaimId::from_bytes([1; 16]),
            generation: 2,
            token: [0xab; 16],
        };
        let mutation = super::super::Mutation::Claim(ClaimInput::Renew {
            proof,
            lease_seconds: 3,
        });
        assert_eq!(
            serde_json::to_value(mutation).unwrap(),
            json!({"domain":"claim","input":{
                "op":"claim_renew","lease_seconds":3,
                "proof":{"id":"01".repeat(16),"generation":2,"token":"ab".repeat(16)}
            }})
        );
    }

    #[test]
    fn submission_prompt_bounds_follow_the_transport() {
        use super::ValidateInput;
        use provenant_core::WaitTransport;

        for bytes in [65_536, 65_537, 117 * 1024, 1_048_576] {
            let input: SubmitInput = serde_json::from_value(json!({
                "op":"submit", "operation_id":"large-prompt",
                "tasks":[{"prompt":"x".repeat(bytes), "route":{"kind":"named","name":"utility"}, "timeout_seconds":1}]
            }))
            .unwrap();
            assert!(input.validate_input(WaitTransport::Cli).is_ok());
            let mcp = input.validate_input(WaitTransport::Mcp);
            if bytes <= 65_536 {
                assert!(mcp.is_ok());
            } else {
                assert_eq!(mcp.unwrap_err().path, "tasks[0].prompt");
            }
        }
        for prompt in [String::new(), "x\0y".into(), "x".repeat(1_048_577)] {
            rejects::<SubmitInput>(
                json!({
                    "op":"submit", "operation_id":"invalid-prompt",
                    "tasks":[{"prompt":prompt, "route":{"kind":"named","name":"utility"}, "timeout_seconds":1}]
                }),
                "tasks[0].prompt",
            );
        }
    }

    #[test]
    fn submission_omits_absent_host() {
        let input = json!({
            "op":"submit",
            "operation_id":"place",
            "tasks":[{"prompt":"x","route":{"kind":"named","name":"utility"},"timeout_seconds":1}]
        });
        let local: SubmitInput = serde_json::from_value(input).unwrap();
        assert!(serde_json::to_value(&local).unwrap().get("host").is_none());
        let placed: SubmitInput = serde_json::from_value(json!({
            "op":"submit",
            "operation_id":"place",
            "tasks":[{"prompt":"x","route":{"kind":"named","name":"utility"},"timeout_seconds":1}],
            "host":"07070707070707070707070707070707"
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&placed).unwrap()["host"],
            "07070707070707070707070707070707"
        );
    }

    #[test]
    fn serde_rejects_out_of_bounds_task_text() {
        let input = serde_json::json!({
            "op": "create",
            "objective": "x".repeat(65_537),
            "note": null
        });

        rejects::<TaskInput>(input, "objective");
    }

    #[test]
    fn serde_rejects_bounds_on_nested_string_fields() {
        rejects::<LandingInput>(
            json!({
                "op":"landing_acquire",
                "reference":"r".repeat(1_025),
                "expected":"0123456789012345678901234567890123456789",
                "candidate":"abcdefabcdefabcdefabcdefabcdefabcdefabcd",
                "lease_seconds":1
            }),
            "reference",
        );
        rejects::<ClaimInput>(
            json!({"op":"claim_acquire","scope":{"kind":"issue","key":"k".repeat(1_025)},"lease_seconds":1}),
            "scope.key",
        );
        rejects::<ClaimInput>(
            json!({"op":"claim_acquire","scope":{"kind":"worktree","path":"/".to_owned() + &"p".repeat(4_096)},"lease_seconds":1}),
            "scope.path",
        );
        rejects::<ClaimInput>(
            json!({"op":"claim_acquire","scope":{"kind":"paths","paths":["p".repeat(4_097)]},"lease_seconds":1}),
            "scope.paths[0]",
        );
        let task = json!({"prompt":"do work","route":{"kind":"named","name":"utility"},"timeout_seconds":1});
        rejects::<SubmitInput>(
            json!({"op":"submit","operation_id":"work-1","tasks":vec![task; 65]}),
            "tasks",
        );
        for name in ["provider", "model", "effort"] {
            let mut route = json!({"kind":"model","provider":"codex","model":"model"});
            route[name] = json!("m".repeat(129));
            rejects::<SubmitInput>(
                json!({
                    "op":"submit","operation_id":"work-1","concurrency":1,
                    "tasks":[{"prompt":"do work","route":route,"timeout_seconds":1}]
                }),
                &format!("tasks[0].route.{name}"),
            );
        }
        rejects::<SubmitInput>(
            json!({"op":"submit","operation_id":"work-1","tasks":[{
                "prompt":"do work","route":{"kind":"named","name":"m".repeat(129)},"timeout_seconds":1
            }]}),
            "tasks[0].route.name",
        );
        rejects::<OutputInput>(
            json!({
                "op":"fetch","target":{"attempt":"00000000000000000000000000000001"},
                "artifact":{"path":{"encoding":"utf8","path":"a".repeat(4_097)},"digest":null},
                "max_bytes":1
            }),
            "artifact.path.path",
        );
        rejects::<LandingInput>(
            json!({
                "op":"landing_verify","proof":{"reference":"r".repeat(1_025),"id":"00000000000000000000000000000001",
                    "generation":0,"token":"00000000000000000000000000000000"}
            }),
            "proof.reference",
        );
    }

    #[test]
    fn serde_rejects_numeric_and_collection_bounds() {
        rejects::<TaskInput>(
            json!({"op":"claim","id":"00000000000000000000000000000001","lease_seconds":0}),
            "lease_seconds",
        );
        rejects::<TaskInput>(json!({"op":"list","state":"active","limit":1}), "state");
        rejects::<super::super::EventsInput>(
            json!({"op":"read","limit":1,"wait_seconds":0}),
            "wait_seconds",
        );
        rejects::<RunInput>(
            json!({
                "op":"wait","condition":"any","wait_seconds":1,
                "targets":vec![json!({"attempt":"00000000000000000000000000000001"}); 101]
            }),
            "targets",
        );
        rejects::<OutputInput>(
            json!({
                "op":"fetch","target":{"attempt":"00000000000000000000000000000001"},
                "artifact":{"path":{"encoding":"utf8","path":"a"},"digest":null},
                "max_bytes":super::MAX_FETCH_BYTES + 1
            }),
            "max_bytes",
        );
    }

    #[test]
    fn proof_tokens_are_hex_and_binary_output_is_base64() {
        use crate::coordination::TaskProof;
        use crate::coordination::facade::OutputBytes;
        let proof = TaskProof {
            task: provenant_core::TaskId::from_bytes([1; 16]),
            generation: 2,
            token: [0xab; 16],
        };
        let value = serde_json::to_value(&proof).unwrap();
        assert_eq!(value["token"], "ab".repeat(16));
        assert_eq!(
            serde_json::from_value::<TaskProof>(value.clone()).unwrap(),
            proof
        );
        for token in [json!(vec![0; 16]), json!("AB".repeat(16)), json!("ab")] {
            let mut invalid = value.clone();
            invalid["token"] = token;
            assert!(serde_json::from_value::<TaskProof>(invalid).is_err());
        }
        let bytes = OutputBytes::from_bytes(vec![0xff, 0x00]);
        let encoded = serde_json::to_value(&bytes).unwrap();
        assert_eq!(encoded, json!({"encoding":"base64","data":"/wA="}));
        assert_eq!(
            serde_json::from_value::<OutputBytes>(encoded).unwrap(),
            bytes
        );
    }

    #[test]
    fn submission_rejects_old_duration_and_route_shapes() {
        let valid = json!({
            "op":"submit","operation_id":"submit",
            "tasks":[{"prompt":"work","route":{"kind":"named","name":"utility"},"timeout_seconds":1}]
        });
        assert!(serde_json::from_value::<SubmitInput>(valid.clone()).is_ok());
        let mut old_duration = valid.clone();
        old_duration["tasks"][0]
            .as_object_mut()
            .unwrap()
            .remove("timeout_seconds");
        old_duration["tasks"][0]["timeout_ms"] = json!(1000);
        assert!(serde_json::from_value::<SubmitInput>(old_duration).is_err());
        for route in [
            json!({}),
            json!({"adapter":"codex","model":"model"}),
            json!({"route":"utility"}),
        ] {
            let mut invalid = valid.clone();
            invalid["tasks"][0]["route"] = route;
            assert!(serde_json::from_value::<SubmitInput>(invalid).is_err());
        }
    }

    struct Resolver;

    impl PathResolver for Resolver {
        fn resolve_existing_ancestor(&self, path: &Path) -> std::io::Result<PathBuf> {
            if path.exists() {
                path.canonicalize()
            } else {
                Ok(self
                    .resolve_existing_ancestor(
                        path.parent().ok_or(std::io::ErrorKind::InvalidInput)?,
                    )?
                    .join(path.file_name().ok_or(std::io::ErrorKind::InvalidInput)?))
            }
        }
    }

    #[test]
    fn coordination_api_rejects_directly_constructed_out_of_bounds_input() {
        let temp = crate::test_support::tempdir().expect("temp root");
        let base = temp.path().canonicalize().expect("canonical temp root");
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        let root = inputs.resolve(&Resolver).expect("state root").state;
        let clock = TestClock::new(
            WallTime::from_micros(2_000_000_000_000_000).expect("wall time"),
            MonoTime::from_micros(0),
        );
        let backoff = ThreadWait;
        let cancel = CancelToken::new();
        let budget = provenant_core::WaitBudget::mcp(clock.mono_now(), 5).expect("budget");
        let store = Store::open(
            &root,
            &Request {
                clock: &clock,
                budget,
                cancel: &cancel,
                backoff: &backoff,
            },
        )
        .expect("store");
        let service = CoordinationService::new(
            store,
            ProjectId::from_bytes([1; 16]),
            ActorId::from_bytes([2; 16]),
            &clock,
            &backoff,
        );

        // Task mutations need an operation id; `mutate` supplies a fresh one.
        let error = CoordinationApi::mutate(
            &service,
            crate::coordination::Mutation::Task(TaskInput::Claim {
                id: provenant_core::TaskId::from_bytes([3; 16]),
                lease_seconds: 0,
            }),
            None,
            budget,
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            CoordinationError::Input(error) if error.path == "lease_seconds"
        ));
    }
}

/// Fencing tokens are opaque, lower-case hex at every JSON boundary.
pub(crate) mod hex_token {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(token: &[u8; 16], serializer: S) -> Result<S::Ok, S::Error> {
        let text = provenant_core::hex(token);
        serializer.serialize_str(&text)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 16], D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.len() != 32
            || !text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(serde::de::Error::custom(
                "expected a 32-character lower-case hex token",
            ));
        }
        let mut token = [0; 16];
        for (index, byte) in token.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
                .map_err(serde::de::Error::custom)?;
        }
        Ok(token)
    }
}
