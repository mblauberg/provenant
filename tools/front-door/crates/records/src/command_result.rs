//! Version-two command transport facts, shared by CLI edges.

use crate::Layer;
use crate::warning::Warning;
use crate::wire::ENVELOPE_VERSION;
use provenant_core::{GitOid, ProjectId};
use serde::{Deserialize, Serialize};

/// Encoded command/relay output capacity; retained run artifacts have a separate bound.
pub const COMMAND_RESULT_MAX_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Failed,
    Refused,
    Invalid,
    Error,
    Unavailable,
    Timeout,
    Cancelled,
    /// The request was accepted but its effect has not happened yet, such as
    /// a merge queued by the repository.
    Pending,
    /// A wait ended before the work settled. The work continues, so this is
    /// not a failure: the caller polls or waits again.
    Waiting,
    /// A wait ended because a live attempt had been silent past its threshold.
    /// The work is still running; this reports the stall, not a settlement.
    Stalled,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CommandError {
    pub code: String,
    pub message: String,
    pub cause: Option<String>,
    #[serde(default)]
    pub class: ErrorClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Subject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setting: Option<SettingRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<Box<CommandError>>,
    #[serde(default)]
    pub next: Vec<NextStep>,
    #[serde(flatten)]
    pub diagnostic: Option<crate::diagnostic::DiagnosticFields>,
}

/// The subject an error or warning concerns, in machine-readable form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subject {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<GitOid>,
}

/// The setting that selected the behaviour reported by an error or warning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingRef {
    pub key: String,
    pub value: String,
    pub layer: Layer,
    /// The exact source, such as a `file:line` location or command-line flag.
    pub origin: String,
}

/// Whether an error is an immutable boundary, a configured stop, or an
/// operation that cannot proceed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    Boundary(crate::policy::Boundary),
    Configured,
    Operation,
    Transient {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_at: Option<jiff::Timestamp>,
    },
}

impl Default for ErrorClass {
    fn default() -> Self {
        Self::Operation
    }
}

impl CommandError {
    /// Construct an error and translate the former repair hint into a typed
    /// next step. Commands beginning with `provenant` become argv; other text
    /// remains a condition for the caller to satisfy.
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        cause: Option<&str>,
        repair: impl Into<String>,
    ) -> Self {
        let repair = repair.into();
        Self {
            diagnostic: None,
            code: code.into(),
            message: message.into(),
            cause: cause.map(Into::into),
            class: ErrorClass::Operation,
            subject: None,
            setting: None,
            root: None,
            next: vec![NextStep::from_text(&repair)],
        }
    }

    pub fn with_subject(mut self, subject: Subject) -> Self {
        self.subject = Some(subject);
        self
    }

    pub fn with_setting(mut self, setting: SettingRef) -> Self {
        self.setting = Some(setting);
        self
    }

    pub fn with_class(mut self, class: ErrorClass) -> Self {
        self.class = class;
        self
    }

    /// Attach diagnostic details and move their legacy next hint into the
    /// structured next-step list, avoiding a duplicate flattened `next` key.
    pub fn set_diagnostic(&mut self, mut diagnostic: Option<crate::diagnostic::DiagnosticFields>) {
        if let Some(details) = diagnostic.as_mut()
            && let Some(next) = details.next.take()
        {
            let next = NextStep::from_text(&next);
            if !self.next.contains(&next) {
                self.next.push(next);
            }
        }
        self.diagnostic = diagnostic;
    }
}
impl Outcome {
    /// Every status, in declaration order; the published schemas enumerate it.
    pub const ALL: [Outcome; 11] = [
        Self::Ok,
        Self::Failed,
        Self::Refused,
        Self::Invalid,
        Self::Error,
        Self::Unavailable,
        Self::Timeout,
        Self::Cancelled,
        Self::Pending,
        Self::Waiting,
        Self::Stalled,
    ];
    /// The process exit code of an envelope with this status: the stable
    /// tier of the consumer contract. It lives beside the status so a
    /// process without the CLI, such as a guardian, exits by the same table.
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::Ok | Self::Waiting => 0,
            Self::Failed | Self::Refused => 1,
            Self::Invalid => 2,
            Self::Error => 3,
            Self::Unavailable => 4,
            Self::Pending => 5,
            Self::Timeout => 124,
            Self::Cancelled => 130,
            Self::Stalled => 6,
        }
    }
}
/// One suggested next command, carried as data rather than formatted text.
/// `argv` holds the executable and its arguments; the binary edge turns it into
/// one human line with the single command formatter.
/// Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextStep {
    /// The command as an argument vector, beginning with `provenant`.
    pub argv: Vec<String>,
    /// One sentence saying why this is the next step.
    pub why: String,
}
impl NextStep {
    pub fn new(argv: Vec<String>, why: impl Into<String>) -> Self {
        Self {
            argv,
            why: why.into(),
        }
    }
    /// Build a step from literal arguments. Owners use this form so the argv a
    /// consumer reads and the line the edge prints come from one value.
    pub fn from_args(argv: &[&str], why: impl Into<String>) -> Self {
        Self::new(argv.iter().map(|arg| (*arg).to_owned()).collect(), why)
    }
    /// Build a step from an already-safe command string whose arguments are
    /// separated by whitespace. Each token becomes one literal argv entry.
    pub fn from_command(command: &str, why: impl Into<String>) -> Self {
        Self::new(command.split_whitespace().map(str::to_owned).collect(), why)
    }

    /// Interpret a legacy free-form repair hint as a command or a condition.
    pub fn from_text(text: &str) -> Self {
        if text.split_whitespace().next() == Some("provenant") {
            Self::from_command(text, "")
        } else {
            Self::new(Vec::new(), text)
        }
    }

    /// Human-readable content without a `next:` prefix.
    pub fn display_text(&self) -> String {
        if self.argv.is_empty() {
            self.why.clone()
        } else if self.why.is_empty() {
            self.argv.join(" ")
        } else {
            format!("{}; {}", self.why, self.argv.join(" "))
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CommandResult<T> {
    pub schema_version: u64,
    pub command: String,
    pub status: Outcome,
    pub data: T,
    pub error: Option<CommandError>,
    /// Every warning raised while running the command. Always on the wire:
    /// under `--json` nothing is printed to stderr, so this is the only place
    /// a consumer can see them. Spec: docs/specs/cli.md#warnings-are-typed-values
    pub warnings: Vec<Warning>,
    /// Suggested next commands. Always on the wire, empty when the command
    /// suggests nothing; rendered once by the binary edge.
    /// Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    pub next: Vec<NextStep>,
}
impl<T> CommandResult<T> {
    pub fn new(command: impl Into<String>, status: Outcome, data: T) -> Self {
        Self {
            schema_version: ENVELOPE_VERSION,
            command: command.into(),
            status,
            data,
            error: None,
            warnings: Vec::new(),
            next: Vec::new(),
        }
    }

    /// Attach suggested next commands to a result before it is emitted.
    pub fn with_next(mut self, next: Vec<NextStep>) -> Self {
        self.next = next;
        self
    }
}
#[derive(Debug, Serialize)]
pub struct HelpCatalogue {
    pub commands: Vec<HelpCommand>,
    /// Stable `error.code` values and their meanings. Only the whole
    /// catalogue lists them; a scoped `help COMMAND` leaves this empty.
    pub error_codes: Vec<HelpErrorCode>,
    /// Provider control capabilities and the actions each provider takes.
    /// Like the error codes, only the whole catalogue lists them.
    pub provider_controls: Vec<crate::control::ControlCapability>,
}
#[derive(Debug, Serialize)]
pub struct HelpErrorCode {
    pub code: String,
    pub summary: String,
}
#[derive(Debug, Serialize)]
pub struct HelpCommand {
    pub command: String,
    pub description: Option<String>,
    /// Direct public subcommands, including commands omitted from compact help.
    pub subcommands: Vec<String>,
    /// Configuration domains consumed by this command.
    pub config_domains: Vec<String>,
    pub arguments: Vec<HelpArgument>,
    pub groups: Vec<HelpGroup>,
    pub constraints: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct HelpGroup {
    pub name: String,
    pub arguments: Vec<String>,
    pub required: bool,
    pub multiple: bool,
}
#[derive(Debug, Serialize)]
pub struct HelpArgument {
    pub name: String,
    pub description: Option<String>,
    pub long: Option<String>,
    pub positional: bool,
    pub required: bool,
    pub repeatable: bool,
    pub defaults: Vec<String>,
    pub conditional_defaults: Vec<(String, String)>,
    pub values: Vec<String>,
    pub conflicts: Vec<String>,
    pub requires: Vec<String>,
    /// Arguments whose presence makes this argument optional.
    pub required_unless: Vec<String>,
    pub bounds: Option<(u64, u64)>,
}
