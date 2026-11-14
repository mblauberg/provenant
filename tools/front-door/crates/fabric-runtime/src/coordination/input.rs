use super::facade::{GraphInput, HostsInput, IdentityInput, OutputInput, RunInput, SubmitInput};
use super::{ArtifactRef, ClaimInput, EventsInput, MailInput, Subject};
use provenant_core::WaitTransport;
use provenant_records::run::RequestedRoute;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputError {
    pub path: String,
    cause: Option<super::facade::InvalidInputDetail>,
}

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid input field `{}`", self.path)?;
        if matches!(
            self.cause,
            Some(super::facade::InvalidInputDetail::FieldTitle(_))
        ) {
            write!(
                f,
                ": task title must be 1 to 256 bytes and contain no control characters"
            )?;
        }
        if let Some(super::facade::InvalidInputDetail::FieldTooLarge { limit, .. }) = &self.cause {
            write!(f, ": exceeds {limit} bytes")?;
        }
        Ok(())
    }
}

impl std::error::Error for InputError {}

impl InputError {
    /// The wire refusal code; `path` identifies the invalid field.
    pub fn code(&self) -> &'static str {
        super::facade::FacadeError::InvalidInput { detail: None }.code()
    }

    /// A bounded schema field path safe for diagnostic envelopes.
    pub fn safe_path(&self) -> Option<String> {
        super::facade::safe_input_field(&self.path)
    }

    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            cause: None,
        }
    }

    fn with_cause(path: &str, cause: super::facade::InvalidInputDetail) -> Self {
        Self {
            path: path.to_owned(),
            cause: Some(cause),
        }
    }

    pub(super) fn diagnostic_detail(&self) -> Option<super::facade::InvalidInputDetail> {
        use super::facade::InvalidInputDetail;
        let path = self.safe_path()?;
        match self.cause.as_ref() {
            Some(InvalidInputDetail::FieldTooLarge { field, limit })
                if super::facade::safe_input_field(field).as_ref() == Some(&path) =>
            {
                Some(InvalidInputDetail::FieldTooLarge {
                    field: path,
                    limit: *limit,
                })
            }
            Some(InvalidInputDetail::FieldTitle(field))
                if super::facade::safe_input_field(field).as_ref() == Some(&path) =>
            {
                Some(InvalidInputDetail::FieldTitle(path))
            }
            _ => Some(InvalidInputDetail::Field(path)),
        }
    }

    pub fn diagnostic(&self) -> provenant_records::diagnostic::Diagnostic {
        super::facade::FacadeError::InvalidInput {
            detail: self.diagnostic_detail(),
        }
        .diagnostic()
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
        #[serde(tag = "op", rename_all = "snake_case")]
        $vis enum $name {
            $( $(#[$variant_attr])* $variant { $( $(#[$field_attr])* $field: $field_type, )* #[serde(flatten)] extra: provenant_records::ExtraFields } ),*
        }

        #[derive(serde::Deserialize)]
        #[serde(tag = "op", rename_all = "snake_case")]
        enum $wire {
            $(
                $(#[$variant_attr])*
                $variant {
                    $( $(#[$field_attr])* $field: $field_type, )*
                    #[serde(flatten)] extra: provenant_records::ExtraFields,
                }
            ),*
        }

        impl From<$wire> for $name {
            fn from(value: $wire) -> Self {
                match value {
                    $(
                        $wire::$variant { $( $field, )* extra } => Self::$variant { $( $field, )* extra }
                    ),*
                }
            }
        }

        impl $name {
            /// Decode and validate for a transport while retaining the typed
            /// validation cause separately from structural decoding errors.
            pub fn decode_for_transport(
                value: serde_json::Value,
                transport: provenant_core::WaitTransport,
            ) -> Result<Result<Self, crate::coordination::input::InputError>, serde_json::Error> {
                let input = Self::from(serde_json::from_value::<$wire>(value)?);
                Ok(crate::coordination::input::ValidateInput::validate_input(&input, transport)
                    .map(|()| input))
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
/// Leaves over 250 KiB for the relay envelope under the 1 MiB frame cap after
/// worst-case sixfold JSON string escaping; base64 encoding stays below 2x.
pub const MAX_FETCH_BYTES: u64 = 131_072;

fn field(path: &str) -> InputError {
    InputError::new(path)
}

fn mail_session(session: Option<&str>) -> Result<(), InputError> {
    match session {
        Some(session) => crate::inbox::session_id(session).map_err(|_| field("session")),
        None => Ok(()),
    }
}

fn bounded_text(value: &str, path: &str, max: usize) -> Result<(), InputError> {
    if value.len() > max {
        return Err(InputError::with_cause(
            path,
            super::facade::InvalidInputDetail::FieldTooLarge {
                field: path.to_owned(),
                limit: max,
            },
        ));
    }
    if value.is_empty() || value.contains('\0') {
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

fn wait(seconds: u64, transport: WaitTransport) -> Result<(), InputError> {
    if seconds > transport.maximum_seconds() {
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

fn subject(subject: &Subject) -> Result<(), InputError> {
    bounded_text(
        &subject.id,
        "subject.id",
        if subject.kind.is_path() {
            MAX_ARTIFACT_PATH_BYTES
        } else {
            MAX_REFERENCE_BYTES
        },
    )
}

fn route(route: &RequestedRoute, path: &str) -> Result<(), InputError> {
    let effort = match route {
        RequestedRoute::Adapter { provider, name, .. } => {
            bounded_text(provider, &format!("{path}.provider"), MAX_ROUTE_BYTES)?;
            bounded_text(name, &format!("{path}.name"), MAX_ROUTE_BYTES)?;
            return Ok(());
        }
        RequestedRoute::Model {
            provider,
            model,
            effort,
            ..
        } => {
            bounded_text(provider, &format!("{path}.provider"), MAX_ROUTE_BYTES)?;
            bounded_text(model, &format!("{path}.model"), MAX_ROUTE_BYTES)?;
            effort
        }
        RequestedRoute::Named { name, effort, .. }
        | RequestedRoute::Review { name, effort, .. } => {
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
            Self::Read {
                after,
                limit,
                session,
                ..
            } => {
                if after.is_some_and(|value| value < 0) {
                    return Err(field("after"));
                }
                mail_session(session.as_deref())?;
                page(*limit, "limit")
            }
            Self::MarkRead {
                through, session, ..
            } => {
                if *through < 0 {
                    return Err(field("through"));
                }
                mail_session(session.as_deref())
            }
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
                ..
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
            Self::Note {
                note, dedup_key, ..
            } => {
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
                subject: value,
                holder,
                ..
            } => {
                subject(value)?;
                if let Some(chair) = holder.chair() {
                    optional_text(
                        chair.session.as_deref(),
                        "holder.session",
                        MAX_REFERENCE_BYTES,
                    )?;
                }
                Ok(())
            }
            Self::Renew { subject: value, .. }
            | Self::Verify { subject: value, .. }
            | Self::Release { subject: value, .. } => subject(value),
            Self::List { limit, .. } => page(*limit, "limit"),
        }
    }
}

impl ValidateInput for IdentityInput {
    fn validate_input(&self, _transport: WaitTransport) -> Result<(), InputError> {
        match self {
            Self::Inspect { .. } => Ok(()),
        }
    }
}

impl ValidateInput for GraphInput {
    fn validate_input(&self, _transport: WaitTransport) -> Result<(), InputError> {
        match self {
            Self::Run { document, .. } => path_text(document, "document"),
            Self::Resume {
                document, extend, ..
            } => {
                path_text(document, "document")?;
                if extend.len() > 64 || extend.iter().any(|entry| {
                    entry.len() > 256 || !matches!(entry.split_once('='), Some((key, value)) if !key.is_empty() && !value.is_empty())
                }) {
                    return Err(field("extend"));
                }
                Ok(())
            }
            Self::Cancel { id, reason, .. } => {
                if !crate::graph::valid_identifier(id) {
                    return Err(field("id"));
                }
                if reason.as_ref().is_some_and(|reason| {
                    reason.len() > crate::graph::MAX_CANCEL_REASON_BYTES || reason.contains('\0')
                }) {
                    return Err(field("reason"));
                }
                Ok(())
            }
            Self::Status { id, .. } => {
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
            Self::Submit { task, knobs, .. } => {
                if knobs.len() > 64 {
                    return Err(field("knobs"));
                }
                for (index, knob) in knobs.iter().enumerate() {
                    bounded_text(knob, &format!("knobs[{index}]"), 256)?;
                }
                {
                    let path = "task";
                    if task
                        .context
                        .as_ref()
                        .is_some_and(|context| !context.accepted())
                    {
                        return Err(field(&format!("{path}.context")));
                    }
                    // Peer dispatch uses the CLI launch limit; the relay retains its
                    // smaller frame bound.
                    let prompt_limit = match transport {
                        WaitTransport::Cli => MAX_CLI_PROMPT_BYTES,
                        WaitTransport::Relay => MAX_TEXT_BYTES,
                    };
                    bounded_text(&task.prompt, &format!("{path}.prompt"), prompt_limit)?;
                    if let Some(title) = &task.title {
                        provenant_core::validate_task_title(title).map_err(|_| {
                            let field = format!("{path}.title");
                            InputError::with_cause(
                                &field,
                                super::facade::InvalidInputDetail::FieldTitle(field.clone()),
                            )
                        })?;
                    }
                    optional_text(
                        task.base.as_deref(),
                        &format!("{path}.base"),
                        MAX_REFERENCE_BYTES,
                    )?;
                    optional_text(task.head.as_deref(), &format!("{path}.head"), 64)?;
                    optional_text(
                        task.source_head.as_deref(),
                        &format!("{path}.source_head"),
                        64,
                    )?;
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
                    if !(1..=provenant_core::MAX_LANE_RUNTIME_SECONDS)
                        .contains(&task.timeout_seconds)
                    {
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
        ClaimInput, CoordinationApi, CoordinationError, CoordinationService, Subject,
    };
    use provenant_core::ThreadPause;
    use provenant_core::{
        ActorId, CancelToken, Clock, MonoTime, PathResolver, ProjectId, RootInputs, TestClock,
        WallTime,
    };
    use provenant_fabric_store::{Request, Store};
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};
    use std::path::{Path, PathBuf};

    fn rejects<T: DeserializeOwned + std::fmt::Debug>(value: Value, path: &str) {
        let error = serde_json::from_value::<T>(value).unwrap_err().to_string();
        assert!(error.contains(path), "expected `{path}` in `{error}`");
    }

    #[test]
    fn graph_cancel_reason_is_optional_and_bounded() {
        for value in [
            json!({"op":"cancel", "id":"g-test"}),
            json!({"op":"cancel", "id":"g-test", "reason":""}),
            json!({"op":"cancel", "id":"g-test", "reason":"x".repeat(256)}),
        ] {
            assert!(serde_json::from_value::<GraphInput>(value).is_ok());
        }
        rejects::<GraphInput>(
            json!({"op":"cancel", "id":"g-test", "reason":"x".repeat(257)}),
            "reason",
        );
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
        use super::ValidateInput;
        for title in ["line\nbreak", "tab\there", "escape\u{1b}", "unicode\u{85}"] {
            let value = json!({
                "op":"submit", "operation_id":"work-1",
                "task":{"prompt":"do work", "title":title, "route":{"kind":"named","name":"utility"}, "timeout_seconds":1}
            });
            assert!(serde_json::from_value::<SubmitInput>(value).is_err());
            let mut input: SubmitInput = serde_json::from_value(json!({
                "op":"submit", "operation_id":"work-1",
                "task":{"prompt":"do work", "route":{"kind":"named","name":"utility"}, "timeout_seconds":1}
            })).unwrap();
            let SubmitInput::Submit { task, .. } = &mut input;
            task.title = Some(title.into());
            let error = input
                .validate_input(provenant_core::WaitTransport::Cli)
                .unwrap_err();
            assert_eq!(error.code(), "invalid_input");
            assert_eq!(error.path, "task.title");
        }
    }

    #[test]
    fn custody_requests_round_trip_the_subject() {
        for &kind in provenant_records::custody::SubjectKind::ALL {
            let input = ClaimInput::Renew {
                extra: Default::default(),
                subject: Subject::new(kind, "item"),
            };
            let encoded = serde_json::to_value(&input).unwrap();
            let read: ClaimInput = serde_json::from_value(encoded).unwrap();
            assert_eq!(input, read);
        }
    }

    #[test]
    fn submission_prompt_bounds_follow_the_transport() {
        use super::ValidateInput;
        use provenant_core::WaitTransport;

        for bytes in [65_536, 65_537, 117 * 1024, 1_048_576] {
            let input: SubmitInput = serde_json::from_value(json!({
                "op":"submit", "operation_id":"large-prompt",
                "task":{"prompt":"x".repeat(bytes), "route":{"kind":"named","name":"utility"}, "timeout_seconds":1}
            }))
            .unwrap();
            assert!(input.validate_input(WaitTransport::Cli).is_ok());
            let relay = input.validate_input(WaitTransport::Relay);
            if bytes <= 65_536 {
                assert!(relay.is_ok());
            } else {
                let error = relay.unwrap_err();
                assert_eq!(error.path, "task.prompt");
                let projection = error.diagnostic();
                let detail = projection.detail.as_deref().unwrap();
                assert!(detail.contains("task.prompt"));
                assert!(detail.contains("65536"));
                assert!(projection.next.is_some());
                let facade = super::super::facade::FacadeError::from(
                    &super::super::CoordinationError::Input(error),
                );
                let received: super::super::facade::FacadeError =
                    serde_json::from_value(serde_json::to_value(&facade).unwrap()).unwrap();
                assert_eq!(received.diagnostic(), facade.diagnostic());
            }
        }
        for prompt in [String::new(), "x\0y".into(), "x".repeat(1_048_577)] {
            rejects::<SubmitInput>(
                json!({
                    "op":"submit", "operation_id":"invalid-prompt",
                    "task":{"prompt":prompt, "route":{"kind":"named","name":"utility"}, "timeout_seconds":1}
                }),
                "task.prompt",
            );
        }
    }

    #[test]
    fn submission_omits_absent_host() {
        let input = json!({
            "op":"submit",
            "operation_id":"place",
            "task":{"prompt":"x","route":{"kind":"named","name":"utility"},"timeout_seconds":1}
        });
        let local: SubmitInput = serde_json::from_value(input).unwrap();
        assert!(serde_json::to_value(&local).unwrap().get("host").is_none());
        let placed: SubmitInput = serde_json::from_value(json!({
            "op":"submit",
            "operation_id":"place",
            "task":{"prompt":"x","route":{"kind":"named","name":"utility"},"timeout_seconds":1},
            "host":"07070707070707070707070707070707"
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&placed).unwrap()["host"],
            "07070707070707070707070707070707"
        );
    }

    #[test]
    fn serde_rejects_out_of_bounds_message_text() {
        let input = serde_json::json!({
            "op": "send",
            "recipients": ["00000000000000000000000000000001"],
            "kind": "note",
            "body": "x".repeat(65_537),
            "artifacts": [],
            "reply_to": null
        });

        rejects::<super::super::MailInput>(input, "body");
    }

    #[test]
    fn serde_rejects_bounds_on_nested_string_fields() {
        rejects::<ClaimInput>(
            json!({"op":"claim_verify","subject":{"kind":"issue","id":"k".repeat(1_025)}}),
            "subject.id",
        );
        for name in ["provider", "model", "effort"] {
            let mut route = json!({"kind":"model","provider":"codex","model":"model"});
            route[name] = json!("m".repeat(129));
            rejects::<SubmitInput>(
                json!({
                    "op":"submit","operation_id":"work-1",
                    "task":{"prompt":"do work","route":route,"timeout_seconds":1}
                }),
                &format!("task.route.{name}"),
            );
        }
        rejects::<SubmitInput>(
            json!({"op":"submit","operation_id":"work-1","task":{
                "prompt":"do work","route":{"kind":"named","name":"m".repeat(129)},"timeout_seconds":1
            }}),
            "task.route.name",
        );
        rejects::<OutputInput>(
            json!({
                "op":"fetch","target":{"attempt":"00000000000000000000000000000001"},
                "artifact":{"path":{"encoding":"utf8","path":"a".repeat(4_097)},"digest":null},
                "max_bytes":1
            }),
            "artifact.path.path",
        );
    }

    #[test]
    fn serde_rejects_numeric_and_collection_bounds() {
        rejects::<ClaimInput>(json!({"op":"claim_list","limit":0}), "limit");
        assert!(
            serde_json::from_value::<super::super::EventsInput>(
                json!({"op":"read","limit":1,"wait_seconds":0})
            )
            .is_ok()
        );
        assert!(
            serde_json::from_value::<RunInput>(json!({
                "op":"wait","condition":"any","wait_seconds":0,
                "targets":[{"attempt":"00000000000000000000000000000001"}]
            }))
            .is_ok()
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
    fn binary_output_is_base64() {
        use crate::coordination::facade::OutputBytes;
        let bytes = OutputBytes::from_bytes(vec![0xff, 0x00]);
        let encoded = serde_json::to_value(&bytes).unwrap();
        assert_eq!(encoded, json!({"encoding":"base64","data":"/wA="}));
        assert_eq!(
            serde_json::from_value::<OutputBytes>(encoded).unwrap(),
            bytes
        );
    }

    #[test]
    fn submission_rejects_invalid_route_shapes() {
        let valid = json!({
            "op":"submit","operation_id":"submit",
            "task":{"prompt":"work","route":{"kind":"named","name":"utility"},"timeout_seconds":1}
        });
        assert!(serde_json::from_value::<SubmitInput>(valid.clone()).is_ok());
        for route in [
            json!({}),
            json!({"adapter":"codex","model":"model"}),
            json!({"route":"utility"}),
        ] {
            let mut invalid = valid.clone();
            invalid["task"]["route"] = route;
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
        let backoff = ThreadPause;
        let cancel = CancelToken::new();
        let budget = provenant_core::WaitBudget::relay(clock.mono_now(), 5).expect("budget");
        let store = Store::open(
            &root,
            &Request {
                clock: &clock,
                cancel: &cancel,
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

        // Claim mutations need an operation id; `mutate` supplies a fresh one.
        let error = CoordinationApi::mutate(
            &service,
            crate::coordination::Mutation::Claim(ClaimInput::Acquire {
                extra: Default::default(),
                subject: Subject::new(provenant_records::custody::SubjectKind::Issue, ""),
                holder: provenant_records::custody::Holder::Attempt {
                    attempt: provenant_core::AttemptId::from_bytes([1; 16]),
                    chair: None,
                },
            }),
            None,
            budget,
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            CoordinationError::Input(error) if error.path == "subject.id"
        ));
    }
}
