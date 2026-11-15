//! Shared repository port and forge operations. Intended for `ports/forge.rs`.
use super::{
    CommandExitCodes, ExitDisposition, LandBackend, LandPort, Port, PortBackend, PortCommand,
    PortCommandDisposition, PortEvidenceError, PortSource,
};
use provenant_os::exec::{
    ExecOptions, ExecOutcome, Executor, StreamPolicy, SystemExecutor, Termination,
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    time::Duration,
};

pub const REST_API_VERSION_HEADER: &str = "X-GitHub-Api-Version:2026-03-10";

#[derive(Debug, thiserror::Error)]
pub enum PortError {
    #[error("port {owner} is disabled (source: {port_source:?})")]
    Disabled {
        owner: &'static str,
        port_source: PortSource,
    },
    #[error("cannot start port executor: {0}")]
    Executor(#[source] io::Error),
    #[error("port command evidence failed")]
    Evidence(#[source] PortEvidenceError),
    #[error("command receipt is visible but durability is unconfirmed: {cause}")]
    EvidenceVisible { cause: String },
    #[error("port execution returned {0:?}")]
    Disposition(PortCommandDisposition),
    #[error("invalid forge response: {0}")]
    Response(&'static str),
}

/// Result shared by builtin and command implementations. `outcome` is kept so
/// callers can preserve bounded stdout/stderr and process details.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortExecution {
    pub disposition: PortCommandDisposition,
    pub outcome: Option<ExecOutcome>,
}

impl PortExecution {
    pub fn capture_complete(&self) -> bool {
        self.outcome
            .as_ref()
            .is_some_and(super::evidence::outcome_complete)
    }
}

/// The sole Builtin/Command/None dispatch point for sourced repository ports.
pub fn dispatch_port_backend<'a, T>(
    backend: &'a PortBackend,
    builtin: impl FnOnce() -> T,
    command: impl FnOnce(&'a PortCommand) -> T,
    none: impl FnOnce() -> T,
) -> T {
    match backend {
        PortBackend::Builtin => builtin(),
        PortBackend::Command {
            command: declaration,
        } => command(declaration),
        PortBackend::None => none(),
    }
}

/// Normalize the landing-only publisher protocol into the shared command
/// backend. Keep its declared timeout as an adapter budget ceiling.
pub fn land_backend(port: &LandPort) -> (Port, Option<Duration>) {
    match &port.backend {
        LandBackend::Builtin => (
            Port {
                source: port.source,
                backend: PortBackend::Builtin,
            },
            None,
        ),
        LandBackend::None => (
            Port {
                source: port.source,
                backend: PortBackend::None,
            },
            None,
        ),
        LandBackend::Command { command } => (
            Port {
                source: port.source,
                backend: PortBackend::Command {
                    command: PortCommand {
                        argv: command.argv.clone(),
                        exit_codes: CommandExitCodes {
                            success: vec![0],
                            unknown: vec![command.unknown_exit_code as i32],
                            other: ExitDisposition::Refused,
                        },
                    },
                },
            },
            Some(Duration::from_secs(command.timeout_seconds)),
        ),
    }
}

pub struct Adapter {
    owner: &'static str,
    port_source: PortSource,
    backend: PortBackend,
    project: PathBuf,
    state: PathBuf,
    executor: SystemExecutor,
    cancel: provenant_core::CancelToken,
    budget_ceiling: Option<Duration>,
    request: Option<super::Request>,
}
impl Adapter {
    pub fn new(port: Port, owner: &'static str, project: &Path, state: &Path) -> io::Result<Self> {
        Self::from_parts(port, owner, project, state, None)
    }

    pub fn from_land_port(
        port: LandPort,
        owner: &'static str,
        project: &Path,
        state: &Path,
    ) -> io::Result<Self> {
        let (port, ceiling) = land_backend(&port);
        Self::from_parts(port, owner, project, state, ceiling)
    }

    fn from_parts(
        port: Port,
        owner: &'static str,
        project: &Path,
        state: &Path,
        budget_ceiling: Option<Duration>,
    ) -> io::Result<Self> {
        Ok(Self {
            owner,
            port_source: port.source,
            backend: port.backend,
            project: project.to_owned(),
            state: state.to_owned(),
            executor: SystemExecutor::new(1).map_err(io::Error::other)?,
            cancel: provenant_core::CancelToken::new(),
            budget_ceiling,
            request: None,
        })
    }

    pub fn set_cancel(&mut self, cancel: provenant_core::CancelToken) {
        self.cancel = cancel;
    }

    fn bind(&mut self, request: &super::Request) {
        self.cancel = request.cancel().clone();
        self.request = Some(request.clone());
    }

    fn budget(&self, budget: Duration) -> Result<Duration, PortError> {
        let budget = self.budget_ceiling.map_or(budget, |cap| budget.min(cap));
        let budget = self
            .request
            .as_ref()
            .map_or(budget, |request| budget.min(request.remaining()));
        if self.cancel.is_cancelled() {
            return Err(PortError::Disposition(PortCommandDisposition::Cancelled));
        }
        if budget.is_zero() {
            return Err(PortError::Disposition(PortCommandDisposition::TimedOut));
        }
        Ok(budget)
    }

    /// Select the backend once, preserving a caller's builtin runner and its
    /// cwd, executable, environment, stdin and cancellation boundary.
    #[allow(
        clippy::too_many_arguments,
        reason = "shared adapter preserves the owning execution context"
    )]
    pub fn execute_with(
        &mut self,
        builtin: impl FnOnce(Duration) -> io::Result<ExecOutcome>,
        arguments: &[OsString],
        environment: &[(OsString, OsString)],
        input: Option<&[u8]>,
        budget: Duration,
        phase: &'static str,
        request_key: &str,
    ) -> Result<PortExecution, PortError> {
        let budget = self.budget(budget)?;
        let owner = self.owner;
        let source = self.port_source;
        let disabled = || {
            Err(PortError::Disabled {
                owner,
                port_source: source,
            })
        };
        dispatch_port_backend(
            &self.backend,
            || {
                let outcome = builtin(budget).map_err(PortError::Executor)?;
                let disposition = disposition(&outcome);
                Ok(PortExecution {
                    disposition,
                    outcome: Some(outcome),
                })
            },
            |command| {
                let execution = super::execute_port_command_with_input(
                    &self.executor,
                    command,
                    &self.project,
                    arguments,
                    environment,
                    input,
                    budget,
                    &self.cancel,
                );
                let publication = super::record_port_command(
                    &self.state,
                    owner,
                    source,
                    phase,
                    request_key,
                    &execution,
                    super::PortReceiptLifetime::AgeDays {
                        days: super::DEFAULT_PORT_COMMAND_RECEIPT_AGE_DAYS,
                    },
                )
                .map_err(PortError::Evidence)?;
                if let super::PortCommandPublication::VisibleDurabilityUnconfirmed { cause } =
                    publication
                {
                    return Err(PortError::EvidenceVisible { cause });
                }
                Ok(PortExecution {
                    disposition: execution.disposition,
                    outcome: execution.outcome,
                })
            },
            disabled,
        )
    }
}

fn disposition(outcome: &ExecOutcome) -> PortCommandDisposition {
    if outcome.timed_out {
        PortCommandDisposition::TimedOut
    } else if outcome.cancelled {
        PortCommandDisposition::Cancelled
    } else if outcome.termination == Termination::Exit(0) {
        PortCommandDisposition::Succeeded
    } else {
        PortCommandDisposition::Refused
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForgeOperation {
    Repository,
    PullByHead {
        branch: String,
        fields: Vec<String>,
    },
    PullSnapshot {
        repository: String,
    },
    RequiredChecks {
        target: String,
    },
    Pull {
        repository: String,
        number: u64,
    },
    Merge {
        repository: String,
        number: u64,
        head: String,
        method: String,
    },
    Land {
        intent: Vec<u8>,
    },
    Open {
        base: String,
        head: String,
        title: String,
        body: Vec<u8>,
        draft: bool,
    },
    RevertAdoption {
        repository: String,
        branch: String,
        head: String,
    },
    Issue {
        number: u64,
    },
    Api {
        path: String,
        arguments: Vec<OsString>,
        etag: Option<String>,
    },
    Cli {
        arguments: Vec<OsString>,
        input: Option<Vec<u8>>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeadersObservation {
    Present {
        status: u16,
        values: BTreeMap<String, String>,
    },
    Absent,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForgeResponse {
    pub status: Option<u16>,
    pub body: Vec<u8>,
    pub headers: HeadersObservation,
    pub execution: PortExecution,
}

pub trait Forge {
    /// Bind subsequent operations to one absolute deadline and cancellation.
    fn bind(&mut self, _request: &super::Request) {}

    fn execute(
        &mut self,
        operation: ForgeOperation,
        budget: Duration,
    ) -> Result<PortExecution, PortError> {
        self.request(operation, budget)
            .map(|response| response.execution)
    }

    fn request(
        &mut self,
        operation: ForgeOperation,
        budget: Duration,
    ) -> Result<ForgeResponse, PortError>;

    fn get(
        &mut self,
        path: &str,
        etag: Option<&str>,
        budget: Duration,
    ) -> Result<ForgeResponse, PortError> {
        self.request(
            ForgeOperation::Api {
                path: path.to_owned(),
                arguments: Vec::new(),
                etag: etag.map(str::to_owned),
            },
            budget,
        )
    }
}

/// Typed forge facade over the common port adapter.
pub struct ForgePort {
    adapter: Adapter,
}
impl ForgePort {
    pub fn new(port: Port, owner: &'static str, project: &Path, state: &Path) -> io::Result<Self> {
        Ok(Self {
            adapter: Adapter::new(port, owner, project, state)?,
        })
    }

    pub fn from_land_port(
        port: LandPort,
        owner: &'static str,
        project: &Path,
        state: &Path,
    ) -> io::Result<Self> {
        Ok(Self {
            adapter: Adapter::from_land_port(port, owner, project, state)?,
        })
    }

    pub fn set_cancel(&mut self, cancel: provenant_core::CancelToken) {
        self.adapter.set_cancel(cancel);
    }

    pub fn execute_with(
        &mut self,
        operation: ForgeOperation,
        budget: Duration,
        builtin: impl FnOnce(
            &[OsString],
            Option<&[u8]>,
            &[(OsString, OsString)],
            Duration,
        ) -> io::Result<ExecOutcome>,
    ) -> Result<PortExecution, PortError> {
        let budget = self.adapter.budget(budget)?;
        let invocation = Invocation::for_operation(&operation, self.adapter.owner);
        let argv = invocation.builtin_argv.clone();
        let environment = invocation.environment.clone();
        let input = invocation.input.clone();
        let command_arguments = invocation.command_arguments.clone();
        let command_environment = invocation.environment.clone();
        let command_input = invocation.input.clone();
        let phase = invocation.phase;
        let key = invocation.key.clone();
        self.adapter.execute_with(
            move |budget| builtin(&argv, input.as_deref(), &environment, budget),
            &command_arguments,
            &command_environment,
            command_input.as_deref(),
            budget,
            phase,
            &key,
        )
    }

    pub fn request_with(
        &mut self,
        operation: ForgeOperation,
        budget: Duration,
        builtin: impl FnOnce(
            &[OsString],
            Option<&[u8]>,
            &[(OsString, OsString)],
            Duration,
        ) -> io::Result<ExecOutcome>,
    ) -> Result<ForgeResponse, PortError> {
        let budget = self.adapter.budget(budget)?;
        let invocation = Invocation::for_operation(&operation, self.adapter.owner);
        let argv = invocation.builtin_argv.clone();
        let environment = invocation.environment.clone();
        let input = invocation.input.clone();
        let api = invocation.api;
        let key = invocation.key.clone();
        let phase = invocation.phase;
        let command_arguments = invocation.command_arguments.clone();
        let command_environment = invocation.environment.clone();
        let command_input = invocation.input.clone();
        let command_wire = dispatch_port_backend(
            &self.adapter.backend,
            || false,
            |_| invocation.command_wire,
            || false,
        );
        let execution = self.adapter.execute_with(
            move |budget| builtin(&argv, input.as_deref(), &environment, budget),
            &command_arguments,
            &command_environment,
            command_input.as_deref(),
            budget,
            phase,
            &key,
        )?;
        // gh reports HTTP error responses with a nonzero process exit. Their
        // status/rate-limit headers remain observations, not transport failures.
        let builtin_http_error = api
            && execution.disposition == PortCommandDisposition::Refused
            && dispatch_port_backend(&self.adapter.backend, || true, |_| false, || false);
        if execution.disposition != PortCommandDisposition::Succeeded && !builtin_http_error {
            return Err(PortError::Disposition(execution.disposition));
        }
        if !execution.capture_complete() {
            return Err(PortError::Disposition(PortCommandDisposition::Unknown));
        }
        let stdout = &execution
            .outcome
            .as_ref()
            .ok_or(PortError::Response("missing process outcome"))?
            .stdout
            .bytes;
        let (status, headers, mut body) = if api {
            http_response(stdout, command_wire)?
        } else {
            (None, HeadersObservation::Absent, stdout.clone())
        };
        if builtin_http_error && status.is_none() {
            return Err(PortError::Disposition(execution.disposition));
        }
        if matches!(operation, ForgeOperation::Issue { .. })
            && dispatch_port_backend(&self.adapter.backend, || true, |_| false, || false)
        {
            body = normalize_issue(&body)?;
        }
        Ok(ForgeResponse {
            status,
            body,
            headers: status.map_or(HeadersObservation::Absent, |status| match headers {
                HeadersObservation::Present { values, .. } => {
                    HeadersObservation::Present { status, values }
                }
                HeadersObservation::Absent => HeadersObservation::Absent,
            }),
            execution,
        })
    }

    pub fn request(
        &mut self,
        operation: ForgeOperation,
        budget: Duration,
    ) -> Result<ForgeResponse, PortError> {
        <Self as Forge>::request(self, operation, budget)
    }

    pub fn get(
        &mut self,
        path: &str,
        etag: Option<&str>,
        budget: Duration,
    ) -> Result<ForgeResponse, PortError> {
        <Self as Forge>::get(self, path, etag, budget)
    }
}

impl Forge for ForgePort {
    fn bind(&mut self, request: &super::Request) {
        self.adapter.bind(request);
    }

    fn execute(
        &mut self,
        operation: ForgeOperation,
        budget: Duration,
    ) -> Result<PortExecution, PortError> {
        let project = self.adapter.project.clone();
        let cancel = self.adapter.cancel.clone();
        self.execute_with(
            operation,
            budget,
            move |argv, input, environment, budget| {
                run_gh(&project, argv, environment, input, budget, &cancel)
            },
        )
    }

    fn request(
        &mut self,
        operation: ForgeOperation,
        budget: Duration,
    ) -> Result<ForgeResponse, PortError> {
        let project = self.adapter.project.clone();
        let cancel = self.adapter.cancel.clone();
        self.request_with(
            operation,
            budget,
            move |argv, input, environment, budget| {
                run_gh(&project, argv, environment, input, budget, &cancel)
            },
        )
    }
}

struct Invocation {
    builtin_argv: Vec<OsString>,
    command_arguments: Vec<OsString>,
    environment: Vec<(OsString, OsString)>,
    input: Option<Vec<u8>>,
    phase: &'static str,
    key: String,
    api: bool,
    command_wire: bool,
}
impl Invocation {
    fn for_operation(operation: &ForgeOperation, owner: &'static str) -> Self {
        let mut env = Vec::new();
        let (args, input, phase, key, api, _command_wire) = match operation {
            ForgeOperation::Repository => (
                strings(&["repo", "view", "--json", "nameWithOwner"]),
                None,
                "repository",
                "repository".into(),
                false,
                false,
            ),
            ForgeOperation::PullByHead { branch, fields } => (
                strings(&["pr", "list", "--head"])
                    .into_iter()
                    .chain([
                        branch.into(),
                        "--state".into(),
                        "open".into(),
                        "--json".into(),
                        fields.join(",").into(),
                    ])
                    .collect(),
                None,
                "pull_by_head",
                format!("pull_by_head:{branch}"),
                false,
                false,
            ),
            ForgeOperation::PullSnapshot { repository } => (
                vec![
                    "api".into(),
                    "--method".into(),
                    "GET".into(),
                    "--paginate".into(),
                    "--slurp".into(),
                    format!("repos/{repository}/pulls").into(),
                    "-f".into(),
                    "state=all".into(),
                    "-f".into(),
                    "per_page=100".into(),
                ],
                None,
                "pull_snapshot",
                format!("pull_snapshot:{repository}"),
                false,
                false,
            ),
            ForgeOperation::RequiredChecks { target } => (
                vec!["pr".into(), "checks".into(), target.into()],
                None,
                "required_checks",
                format!("required_checks:{target}"),
                false,
                false,
            ),
            ForgeOperation::Pull { repository, number } => (
                vec![
                    "api".into(),
                    "--include".into(),
                    format!("repos/{repository}/pulls/{number}").into(),
                ],
                None,
                "pull",
                format!("pull:{repository}:{number}"),
                true,
                true,
            ),
            ForgeOperation::Merge {
                repository,
                number,
                head,
                method,
            } => (
                vec![
                    "api".into(),
                    "--method".into(),
                    "PUT".into(),
                    format!("repos/{repository}/pulls/{number}/merge").into(),
                    "-f".into(),
                    format!("sha={head}").into(),
                    "-f".into(),
                    format!("merge_method={method}").into(),
                ],
                None,
                "merge",
                format!("merge:{repository}:{number}:{head}"),
                false,
                false,
            ),
            ForgeOperation::Land { intent } => (
                Vec::new(),
                Some(intent.clone()),
                "publish",
                "landing".into(),
                false,
                false,
            ),
            ForgeOperation::Open {
                base,
                head,
                title,
                body,
                draft,
            } => {
                let mut args = strings(&["pr", "create", "--base"])
                    .into_iter()
                    .chain([
                        base.into(),
                        "--head".into(),
                        head.into(),
                        "--title".into(),
                        title.into(),
                        "--body-file".into(),
                        "-".into(),
                    ])
                    .collect::<Vec<_>>();
                if *draft {
                    args.push("--draft".into());
                }
                (
                    args,
                    Some(body.clone()),
                    "open",
                    format!("open:{head}"),
                    false,
                    false,
                )
            }
            ForgeOperation::RevertAdoption {
                repository,
                branch,
                head,
            } => (
                vec![
                    "api".into(),
                    "--include".into(),
                    format!("repos/{repository}/pulls?head={branch}&state=all&head_sha={head}")
                        .into(),
                ],
                None,
                "revert_adoption",
                format!("revert_adoption:{branch}:{head}"),
                true,
                true,
            ),
            ForgeOperation::Issue { number } => {
                let args = if owner == "tracker" {
                    vec![number.to_string().into()]
                } else {
                    vec![
                        "issue".into(),
                        "view".into(),
                        number.to_string().into(),
                        "--json".into(),
                        "body,state,issueType".into(),
                    ]
                };
                (
                    args,
                    None,
                    "issue",
                    number.to_string(),
                    false,
                    owner == "tracker",
                )
            }
            ForgeOperation::Api {
                path,
                arguments,
                etag,
            } => {
                let mut args = strings(&[
                    "api",
                    "--include",
                    "-H",
                    "Accept:application/vnd.github+json",
                    "-H",
                    REST_API_VERSION_HEADER,
                ]);
                if let Some(etag) = etag {
                    args.extend(["-H".into(), format!("If-None-Match:{etag}").into()]);
                }
                args.push(path.into());
                args.extend(arguments.iter().cloned());
                env.push((
                    provenant_core::consumer_env::PORT_REQUEST_PATH.into(),
                    path.into(),
                ));
                env.push((
                    provenant_core::consumer_env::PORT_IF_NONE_MATCH.into(),
                    etag.clone().unwrap_or_default().into(),
                ));
                (args, None, "request", path.clone(), true, true)
            }
            ForgeOperation::Cli { arguments, input } => (
                arguments.clone(),
                input.clone(),
                "request",
                format!(
                    "cli:{}",
                    arguments
                        .first()
                        .map_or("empty".into(), |arg| arg.to_string_lossy().into_owned())
                ),
                false,
                false,
            ),
        };
        let builtin_args = match operation {
            ForgeOperation::Issue { number } => vec![
                "issue".into(),
                "view".into(),
                number.to_string().into(),
                "--json".into(),
                "body,state,issueType".into(),
            ],
            _ => args.clone(),
        };
        let command_arguments = match operation {
            ForgeOperation::Api { path, etag, .. } if owner == "ci_observation" => {
                env.retain(|(name, _)| {
                    name != provenant_core::consumer_env::PORT_REQUEST_PATH
                        && name != provenant_core::consumer_env::PORT_IF_NONE_MATCH
                });
                env.push((
                    provenant_core::consumer_env::PORT_REQUEST_PATH.into(),
                    path.into(),
                ));
                env.push((
                    provenant_core::consumer_env::PORT_IF_NONE_MATCH.into(),
                    etag.clone().unwrap_or_default().into(),
                ));
                Vec::new()
            }
            _ => args.clone(),
        };
        // The built-in goes through `gh`; a command port receives only the
        // operation arguments. The JSON operation tag disambiguates the shared
        // command adapter while preserving the established Issue/API inputs.
        env.push((
            provenant_core::consumer_env::PORT_OPERATION.into(),
            operation_name(operation).into(),
        ));
        Self {
            builtin_argv: std::iter::once("gh".into()).chain(builtin_args).collect(),
            command_arguments,
            environment: env,
            input,
            phase,
            key,
            api,
            command_wire: api,
        }
    }
}

fn strings(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn operation_name(operation: &ForgeOperation) -> &'static str {
    match operation {
        ForgeOperation::Repository => "repository",
        ForgeOperation::PullByHead { .. } => "pull_by_head",
        ForgeOperation::PullSnapshot { .. } => "pull_snapshot",
        ForgeOperation::RequiredChecks { .. } => "required_checks",
        ForgeOperation::Pull { .. } => "pull",
        ForgeOperation::Open { .. } => "open",
        ForgeOperation::Merge { .. } => "merge",
        ForgeOperation::Land { .. } => "land",
        ForgeOperation::RevertAdoption { .. } => "revert_adoption",
        ForgeOperation::Issue { .. } => "issue",
        ForgeOperation::Api { .. } => "api",
        ForgeOperation::Cli { .. } => "cli",
    }
}

fn run_gh(
    project: &Path,
    argv: &[OsString],
    environment: &[(OsString, OsString)],
    input: Option<&[u8]>,
    budget: Duration,
    cancel: &provenant_core::CancelToken,
) -> io::Result<ExecOutcome> {
    let mut options = ExecOptions::new(argv, project, budget);
    let mut inherited: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    for (name, value) in environment {
        inherited.insert(name.clone(), value.clone());
    }
    let inherited: Vec<_> = inherited.into_iter().collect();
    options.env = Some(&inherited);
    options.stdin = input;
    options.stdout = StreamPolicy::Capture {
        limit: 8 * 1024 * 1024,
    };
    options.stderr = StreamPolicy::Tail { limit: 64 * 1024 };
    options.process_group = true;
    options.cleanup_process_group = true;
    options.group_watchdog = true;
    options.cancel = Some(cancel);
    SystemExecutor::new(1)
        .map_err(io::Error::other)?
        .execute(options)
        .map_err(io::Error::other)
}

#[derive(Deserialize)]
struct IssueWire {
    body: Option<String>,
    state: String,
    #[serde(rename = "issueType")]
    issue_type: Option<IssueTypeWire>,
    #[serde(default)]
    kind: Option<String>,
}
#[derive(Deserialize)]
struct IssueTypeWire {
    name: String,
}
fn normalize_issue(bytes: &[u8]) -> Result<Vec<u8>, PortError> {
    let issue: IssueWire =
        serde_json::from_slice(bytes).map_err(|_| PortError::Response("invalid issue JSON"))?;
    serde_json::to_vec(&serde_json::json!({"body":issue.body,"state":issue.state,"kind":issue.kind.or_else(|| issue.issue_type.map(|kind| kind.name))}))
        .map_err(|_| PortError::Response("cannot encode normalized issue"))
}

#[derive(Deserialize)]
struct HttpWire {
    status: u16,
    #[serde(default)]
    headers: Option<BTreeMap<String, String>>,
    body: String,
}
fn http_response(
    bytes: &[u8],
    command_wire: bool,
) -> Result<(Option<u16>, HeadersObservation, Vec<u8>), PortError> {
    if command_wire {
        let response: HttpWire =
            provenant_records::decode(bytes, provenant_records::Limits::default())
                .map_err(|_| PortError::Response("invalid command HTTP envelope"))?;
        if !(100..=599).contains(&response.status) {
            return Err(PortError::Response("invalid HTTP status"));
        }
        let headers = if let Some(headers) = response.headers {
            let mut values = BTreeMap::new();
            for (name, value) in headers {
                if values.insert(name.to_ascii_lowercase(), value).is_some() {
                    return Err(PortError::Response("duplicate HTTP header"));
                }
            }
            HeadersObservation::Present {
                status: response.status,
                values,
            }
        } else {
            HeadersObservation::Absent
        };
        return Ok((Some(response.status), headers, response.body.into_bytes()));
    }
    if !bytes.starts_with(b"HTTP/") {
        return Ok((None, HeadersObservation::Absent, bytes.to_vec()));
    }
    let (split, separator) = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| (index, 4))
        .or_else(|| {
            bytes
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|index| (index, 2))
        })
        .ok_or(PortError::Response("incomplete HTTP headers"))?;
    let header_block = std::str::from_utf8(&bytes[..split])
        .map_err(|_| PortError::Response("HTTP headers are not UTF-8"))?;
    let mut lines = header_block.lines();
    let status_line = lines
        .next()
        .ok_or(PortError::Response("missing HTTP status line"))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|status| (100..=599).contains(status))
        .ok_or(PortError::Response("invalid HTTP status line"))?;
    let mut values = BTreeMap::new();
    for line in lines {
        let (name, value) = line
            .split_once(':')
            .ok_or(PortError::Response("invalid HTTP header"))?;
        if values
            .insert(name.trim().to_ascii_lowercase(), value.trim().to_owned())
            .is_some()
        {
            return Err(PortError::Response("duplicate HTTP header"));
        }
    }
    Ok((
        Some(status),
        HeadersObservation::Present { status, values },
        bytes[split + separator..].to_vec(),
    ))
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor};
    use std::{os::unix::fs::PermissionsExt, path::PathBuf};
    use tempfile::TempDir;

    const BUILTIN_FAKE: &str = r##"#!/bin/sh
case "$PROVENANT_PORT_OPERATION" in
  issue) printf '{"body":"body","state":"OPEN","issueType":{"name":"Bug"}}' ;;
  api) printf '{"ok":true}' ;;
  pull) printf 'HTTP/2 200\r\nETag: fake-tag\r\n\r\n{"ok":true}' ;;
  *) printf '{"ok":true}' ;;
esac
"##;
    const COMMAND_FAKE: &str = r##"#!/bin/sh
case "$PROVENANT_PORT_OPERATION" in
  issue) printf '{"body":"body","state":"OPEN","kind":"Bug"}' ;;
  api) printf '{"status":200,"body":"{\"ok\":true}"}' ;;
  pull) printf '{"status":200,"headers":{"etag":"fake-tag"},"body":"{\"ok\":true}"}' ;;
  *) printf '{"ok":true}' ;;
esac
"##;

    fn executable(dir: &TempDir, name: &str, script: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, script).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    fn operations() -> Vec<ForgeOperation> {
        vec![
            ForgeOperation::Repository,
            ForgeOperation::PullByHead {
                branch: "feature".into(),
                fields: vec!["url".into(), "headRefOid".into()],
            },
            ForgeOperation::PullSnapshot {
                repository: "owner/repo".into(),
            },
            ForgeOperation::RequiredChecks { target: "7".into() },
            ForgeOperation::Pull {
                repository: "owner/repo".into(),
                number: 7,
            },
            ForgeOperation::Merge {
                repository: "owner/repo".into(),
                number: 7,
                head: "abc".into(),
                method: "merge".into(),
            },
            ForgeOperation::Land {
                intent: b"{}".to_vec(),
            },
            ForgeOperation::Open {
                base: "main".into(),
                head: "feature".into(),
                title: "Title".into(),
                body: b"body".to_vec(),
                draft: true,
            },
            ForgeOperation::RevertAdoption {
                repository: "owner/repo".into(),
                branch: "revert/7".into(),
                head: "abc".into(),
            },
            ForgeOperation::Issue { number: 17 },
            ForgeOperation::Api {
                path: "repos/owner/repo/branches/main".into(),
                arguments: Vec::new(),
                etag: Some("old-tag".into()),
            },
            ForgeOperation::Cli {
                arguments: vec!["pr".into(), "view".into(), "7".into()],
                input: None,
            },
        ]
    }

    fn port(backend: PortBackend) -> Port {
        Port {
            source: PortSource::Project,
            backend,
        }
    }

    type FakeRun = dyn FnOnce(
        &[OsString],
        Option<&[u8]>,
        &[(OsString, OsString)],
        Duration,
    ) -> io::Result<ExecOutcome>;

    fn run_fake_gh(fake: PathBuf) -> Box<FakeRun> {
        Box::new(move |argv, input, environment, budget| {
            let mut actual = argv.to_vec();
            actual[0] = fake.clone().into_os_string();
            let mut options = ExecOptions::new(&actual, fake.parent().unwrap(), budget);
            options.env = Some(environment);
            options.stdin = input;
            options.stdout = StreamPolicy::Capture {
                limit: 8 * 1024 * 1024,
            };
            options.stderr = StreamPolicy::Capture { limit: 64 * 1024 };
            SystemExecutor::new(1)
                .map_err(io::Error::other)?
                .execute(options)
                .map_err(io::Error::other)
        })
    }

    fn command_backend(fake: &Path) -> PortBackend {
        PortBackend::Command {
            command: PortCommand {
                argv: vec![fake.display().to_string()],
                exit_codes: CommandExitCodes {
                    success: vec![0],
                    unknown: vec![9],
                    other: ExitDisposition::Refused,
                },
            },
        }
    }

    #[test]
    fn every_forge_operation_uses_each_backend_and_preserves_typed_headers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("state")).unwrap();
        let builtin = executable(&dir, "gh", BUILTIN_FAKE);
        let command = executable(&dir, "forge-command", COMMAND_FAKE);
        let budget = Duration::from_secs(2);

        for operation in operations() {
            let mut builtin_port = ForgePort::new(
                port(PortBackend::Builtin),
                "tracker",
                dir.path(),
                &dir.path().join("state"),
            )
            .unwrap();
            let response = builtin_port
                .request_with(operation.clone(), budget, run_fake_gh(builtin.clone()))
                .unwrap();
            assert_eq!(
                response.execution.disposition,
                PortCommandDisposition::Succeeded
            );
            assert!(response.execution.outcome.is_some());
            if matches!(&operation, ForgeOperation::Api { .. }) {
                assert_eq!(response.headers, HeadersObservation::Absent);
            }
            if matches!(&operation, ForgeOperation::Pull { .. }) {
                assert!(matches!(
                    response.headers,
                    HeadersObservation::Present { .. }
                ));
            }
            if matches!(&operation, ForgeOperation::Issue { .. }) {
                let value: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
                assert_eq!(value["kind"], "Bug");
            }

            let mut command_port = ForgePort::new(
                port(command_backend(&command)),
                "tracker",
                dir.path(),
                &dir.path().join("state"),
            )
            .unwrap();
            let response = command_port
                .request_with(operation.clone(), budget, |_, _, _, _| {
                    panic!("command backend ran builtin closure")
                })
                .unwrap();
            assert_eq!(
                response.execution.disposition,
                PortCommandDisposition::Succeeded
            );
            if matches!(&operation, ForgeOperation::Api { .. }) {
                assert_eq!(response.headers, HeadersObservation::Absent);
            }
            if matches!(&operation, ForgeOperation::Pull { .. }) {
                assert!(matches!(
                    response.headers,
                    HeadersObservation::Present { .. }
                ));
            }
            if matches!(&operation, ForgeOperation::Issue { .. }) {
                let value: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
                assert_eq!(value["kind"], "Bug");
            }

            let mut none_port = ForgePort::new(
                port(PortBackend::None),
                "tracker",
                dir.path(),
                &dir.path().join("state"),
            )
            .unwrap();
            let error = none_port
                .request_with(operation, budget, |_, _, _, _| {
                    panic!("none backend ran builtin closure")
                })
                .unwrap_err();
            assert!(matches!(
                error,
                PortError::Disabled {
                    owner: "tracker",
                    ..
                }
            ));
        }
    }

    #[test]
    fn builtin_http_errors_retain_their_status_and_rate_headers() {
        let dir = tempfile::tempdir().unwrap();
        let gh = executable(
            &dir,
            "gh",
            "#!/bin/sh\nprintf 'HTTP/2 429\\r\\nRetry-After: 60\\r\\n\\r\\n{}'\nexit 1\n",
        );
        let mut forge =
            ForgePort::new(port(PortBackend::Builtin), "forge", dir.path(), dir.path()).unwrap();
        let response = forge
            .request_with(
                ForgeOperation::Api {
                    path: "repos/owner/repo".into(),
                    arguments: Vec::new(),
                    etag: None,
                },
                Duration::from_secs(2),
                run_fake_gh(gh),
            )
            .unwrap();
        assert_eq!(response.status, Some(429));
        assert!(matches!(
            response.headers,
            HeadersObservation::Present { .. }
        ));
        assert_eq!(
            response.execution.disposition,
            PortCommandDisposition::Refused
        );
    }

    #[test]
    fn command_exit_outcomes_follow_the_declaration_and_retain_output() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        for (code, expected) in [
            (0, PortCommandDisposition::Succeeded),
            (1, PortCommandDisposition::Refused),
            (9, PortCommandDisposition::Unknown),
        ] {
            let executable = executable(
                &dir,
                "command",
                &format!("#!/bin/sh\nprintf 'evidence'\nexit {code}\n"),
            );
            let mut forge = ForgePort::new(
                port(command_backend(&executable)),
                "tracker",
                dir.path(),
                &state,
            )
            .unwrap();
            let execution = forge
                .execute_with(
                    ForgeOperation::Cli {
                        arguments: vec!["pr".into(), "view".into()],
                        input: None,
                    },
                    Duration::from_secs(2),
                    |_, _, _, _| unreachable!(),
                )
                .unwrap();
            assert_eq!(execution.disposition, expected);
            assert_eq!(execution.outcome.unwrap().stdout.bytes, b"evidence");
        }
    }
}
