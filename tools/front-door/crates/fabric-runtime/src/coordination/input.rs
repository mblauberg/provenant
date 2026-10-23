use super::facade::{
    HostsInput, IdentityInput, OutputInput, RoutePreference, RunInput, SubmitInput,
};
use super::{
    ArtifactRef, ClaimInput, EventsInput, LandingInput, MailInput, MessageRef, TaskInput, TaskRef,
    TaskState, WorkScope,
};
use provenant_fabric_store::LegacyRef;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid input field `{path}`")]
pub struct InputError {
    pub path: String,
}

impl InputError {
    pub(crate) fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }
}

pub trait ValidateInput {
    fn validate_input(&self) -> Result<(), InputError>;
}

macro_rules! bounded_input {
    (
        $vis:vis enum $name:ident as $wire:ident {
            $(
                $variant:ident {
                    $( $(#[$field_attr:meta])* $field:ident : $field_type:ty ),* $(,)?
                }
            ),* $(,)?
        }
    ) => {
        #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
        #[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
        $vis enum $name {
            $( $variant { $( $field: $field_type ),* } ),*
        }

        #[derive(serde::Deserialize)]
        #[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
        enum $wire {
            $(
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
                crate::coordination::input::ValidateInput::validate_input(&input)
                    .map_err(serde::de::Error::custom)?;
                Ok(input)
            }
        }
    };
}

pub(crate) use bounded_input;

pub const MAX_TEXT_BYTES: usize = 65_536;
pub const MAX_REFERENCE_BYTES: usize = 1_024;
pub const MAX_ROUTE_BYTES: usize = 128;
pub const MAX_ARTIFACT_PATH_BYTES: usize = 4_096;
/// Leaves over 250 KiB for the MCP envelope under the 1 MiB frame cap after
/// worst-case sixfold JSON string escaping; byte-array encoding stays below 4x.
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

fn lease(milliseconds: u64) -> Result<(), InputError> {
    if !(1_000..=3_600_000).contains(&milliseconds) {
        return Err(field("lease_ms"));
    }
    Ok(())
}

fn wait(milliseconds: u64) -> Result<(), InputError> {
    if !(1_000..=55_000).contains(&milliseconds) {
        return Err(field("wait_ms"));
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

fn legacy(reference: &LegacyRef, path: &str) -> Result<(), InputError> {
    bounded_text(
        &reference.source,
        &format!("{path}.source"),
        MAX_REFERENCE_BYTES,
    )?;
    bounded_text(
        &reference.project,
        &format!("{path}.project"),
        MAX_REFERENCE_BYTES,
    )?;
    bounded_text(&reference.id, &format!("{path}.id"), MAX_REFERENCE_BYTES)
}

fn message_ref(reference: &MessageRef, path: &str) -> Result<(), InputError> {
    if let MessageRef::Legacy { reference } = reference {
        legacy(reference, &format!("{path}.reference"))?;
    }
    Ok(())
}

fn task_ref(reference: &TaskRef, path: &str) -> Result<(), InputError> {
    if let TaskRef::Legacy { reference } = reference {
        legacy(reference, &format!("{path}.reference"))?;
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

fn route(route: &RoutePreference, path: &str) -> Result<(), InputError> {
    for (name, value) in [
        ("adapter", route.adapter.as_deref()),
        ("model", route.model.as_deref()),
        ("role", route.role.as_deref()),
        ("task_class", route.task_class.as_deref()),
        ("effort", route.effort.as_deref()),
    ] {
        optional_text(value, &format!("{path}.{name}"), MAX_ROUTE_BYTES)?;
    }
    Ok(())
}

fn task_text(value: &str, path: &str) -> Result<(), InputError> {
    bounded_text(value, path, MAX_TEXT_BYTES)
}

impl ValidateInput for MailInput {
    fn validate_input(&self) -> Result<(), InputError> {
        match self {
            Self::Send {
                recipients,
                body,
                artifacts: values,
                reply_to,
                task,
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
                if let Some(reference) = reply_to {
                    message_ref(reference, "reply_to")?;
                }
                if let Some(reference) = task {
                    task_ref(reference, "task")?;
                }
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
    fn validate_input(&self) -> Result<(), InputError> {
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
            Self::Claim { lease_ms, .. } => lease(*lease_ms),
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
    fn validate_input(&self) -> Result<(), InputError> {
        match self {
            Self::Read {
                after,
                limit,
                wait_ms,
            } => {
                if after.is_some_and(|value| value < 0) {
                    return Err(field("after"));
                }
                page(*limit, "limit")?;
                if let Some(milliseconds) = wait_ms {
                    wait(*milliseconds)?;
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
    fn validate_input(&self) -> Result<(), InputError> {
        match self {
            Self::Acquire {
                scope: value,
                lease_ms,
            } => {
                scope(value)?;
                lease(*lease_ms)
            }
            Self::Renew { lease_ms, .. } => lease(*lease_ms),
            Self::Verify { .. } | Self::Release { .. } => Ok(()),
            Self::List { limit, .. } => page(*limit, "limit"),
        }
    }
}

impl ValidateInput for LandingInput {
    fn validate_input(&self) -> Result<(), InputError> {
        match self {
            Self::Acquire {
                reference,
                lease_ms,
                ..
            } => {
                landing_reference(reference, "reference")?;
                lease(*lease_ms)
            }
            Self::Renew { proof, lease_ms } => {
                landing_reference(&proof.reference, "proof.reference")?;
                lease(*lease_ms)
            }
            Self::Verify { proof } | Self::Release { proof } => {
                landing_reference(&proof.reference, "proof.reference")
            }
            Self::Wait {
                reference, wait_ms, ..
            } => {
                landing_reference(reference, "reference")?;
                wait(*wait_ms)
            }
        }
    }
}

impl ValidateInput for IdentityInput {
    fn validate_input(&self) -> Result<(), InputError> {
        match self {
            Self::Inspect {} => Ok(()),
        }
    }
}

impl ValidateInput for SubmitInput {
    fn validate_input(&self) -> Result<(), InputError> {
        match self {
            Self::Submission {
                tasks, concurrency, ..
            } => {
                if tasks.is_empty() || tasks.len() > 100 {
                    return Err(field("tasks"));
                }
                if *concurrency == 0 || usize::from(*concurrency) > tasks.len() {
                    return Err(field("concurrency"));
                }
                for (index, task) in tasks.iter().enumerate() {
                    let path = format!("tasks[{index}]");
                    task_text(&task.prompt, &format!("{path}.prompt"))?;
                    if task.inputs.len() > 100 {
                        return Err(field(&format!("{path}.inputs")));
                    }
                    for (input_index, reference) in task.inputs.iter().enumerate() {
                        artifact(
                            reference,
                            &format!("{path}.inputs[{input_index}].path.path"),
                        )?;
                    }
                    route(&task.route, &format!("{path}.route"))?;
                    if !(1_000..=14_400_000).contains(&task.timeout_ms) {
                        return Err(field(&format!("{path}.timeout_ms")));
                    }
                }
                Ok(())
            }
        }
    }
}

impl ValidateInput for RunInput {
    fn validate_input(&self) -> Result<(), InputError> {
        match self {
            Self::List { limit, .. } => page(u64::from(*limit), "limit"),
            Self::Status { .. } | Self::Cancel { .. } => Ok(()),
            Self::Wait {
                targets, wait_ms, ..
            } => {
                if targets.is_empty() || targets.len() > 100 {
                    return Err(field("targets"));
                }
                wait(*wait_ms)
            }
            Self::Resume { prompt, .. } => task_text(prompt, "prompt"),
        }
    }
}

impl ValidateInput for OutputInput {
    fn validate_input(&self) -> Result<(), InputError> {
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
    fn validate_input(&self) -> Result<(), InputError> {
        match self {
            Self::List { limit, .. } => page(u64::from(*limit), "limit"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::facade::{OutputInput, RunInput, SubmitInput};
    use super::super::{
        ClaimInput, CoordinationApi, CoordinationError, CoordinationService, LandingInput,
        MailInput, TaskInput,
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
                "op":"acquire",
                "reference":"r".repeat(1_025),
                "expected":"0123456789012345678901234567890123456789",
                "candidate":"abcdefabcdefabcdefabcdefabcdefabcdefabcd",
                "lease_ms":1_000
            }),
            "reference",
        );
        rejects::<ClaimInput>(
            json!({"op":"acquire","scope":{"kind":"issue","key":"k".repeat(1_025)},"lease_ms":1_000}),
            "scope.key",
        );
        rejects::<ClaimInput>(
            json!({"op":"acquire","scope":{"kind":"worktree","path":"/".to_owned() + &"p".repeat(4_096)},"lease_ms":1_000}),
            "scope.path",
        );
        rejects::<ClaimInput>(
            json!({"op":"acquire","scope":{"kind":"paths","paths":["p".repeat(4_097)]},"lease_ms":1_000}),
            "scope.paths[0]",
        );
        for name in ["adapter", "model", "role", "task_class", "effort"] {
            let mut route = json!({});
            route[name] = json!("m".repeat(129));
            rejects::<SubmitInput>(
                json!({
                    "op":"submission","operation_id":"work-1","concurrency":1,
                    "tasks":[{"prompt":"do work","route":route,"timeout_ms":1_000}]
                }),
                &format!("tasks[0].route.{name}"),
            );
        }
        rejects::<OutputInput>(
            json!({
                "op":"fetch","target":{"attempt":"00000000000000000000000000000001"},
                "artifact":{"path":{"encoding":"utf8","path":"a".repeat(4_097)},"digest":null},
                "max_bytes":1
            }),
            "artifact.path.path",
        );
        for name in ["source", "project", "id"] {
            let mut reference = json!({"source":"s","project":"p","id":"i"});
            reference[name] = json!("r".repeat(1_025));
            rejects::<MailInput>(
                json!({
                    "op":"send","recipients":["00000000000000000000000000000001"],
                    "kind":"note","body":"ok","artifacts":[],
                    "reply_to":{"scope":"legacy","reference":reference}
                }),
                &format!("reply_to.reference.{name}"),
            );
        }
        rejects::<LandingInput>(
            json!({
                "op":"verify","proof":{"reference":"r".repeat(1_025),"id":"00000000000000000000000000000001",
                    "generation":0,"token":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}
            }),
            "proof.reference",
        );
    }

    #[test]
    fn serde_rejects_numeric_and_collection_bounds() {
        rejects::<TaskInput>(
            json!({"op":"claim","id":"00000000000000000000000000000001","lease_ms":999}),
            "lease_ms",
        );
        rejects::<TaskInput>(json!({"op":"list","state":"active","limit":1}), "state");
        rejects::<super::super::EventsInput>(
            json!({"op":"read","limit":1,"wait_ms":999}),
            "wait_ms",
        );
        rejects::<RunInput>(
            json!({
                "op":"wait","condition":"any","wait_ms":1_000,
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
        let temp = tempfile::tempdir_in(std::fs::canonicalize("/tmp").expect("tmp path"))
            .expect("temp root");
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

        let error = service
            .task(
                TaskInput::Claim {
                    id: provenant_core::TaskId::from_bytes([3; 16]),
                    lease_ms: 999,
                },
                budget,
                &cancel,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            CoordinationError::Input(error) if error.path == "lease_ms"
        ));
    }
}
