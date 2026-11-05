//! Safe, bounded diagnostic facts shared by command, graph and transport edges.
use serde::{Deserialize, Serialize};
use std::path::{Component, Path};

/// Maximum UTF-8 bytes in each projected field, after terminal escaping.
pub const MAX_DIAGNOSTIC_BYTES: usize = 1024;

/// The hard boundary protected by a typed refusal, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryClass {
    None,
    LaneGit,
    Credentials,
    Integrity,
    Authority,
}

/// Where a configurable admission threshold came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingSource {
    Configuration,
    ShippedDefault,
}

impl SettingSource {
    pub fn description(self) -> &'static str {
        match self {
            Self::Configuration => "host configuration",
            Self::ShippedDefault => "shipped default",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PressureLevel {
    Normal,
    Warning,
    Critical,
}
impl PressureLevel {
    pub fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PressureThreshold {
    Warning,
    Critical,
    Never,
}
impl PressureThreshold {
    pub fn name(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Critical => "critical",
            Self::Never => "never",
        }
    }
}

/// The ledger limit that actually prevented a lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneLimit {
    HostWriters,
    ProjectWriters,
    HostTotal,
    ProjectTotal,
}

impl LaneLimit {
    pub fn setting(self) -> Option<&'static str> {
        match self {
            Self::HostWriters => Some("domains.fabric.lanes.max_writer_lanes"),
            Self::ProjectWriters => Some("domains.fabric.lanes.max_project_writer_lanes"),
            Self::HostTotal | Self::ProjectTotal => None,
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::HostWriters => "host writer lanes",
            Self::ProjectWriters => "project writer lanes",
            Self::HostTotal => "host lanes",
            Self::ProjectTotal => "project lanes",
        }
    }
}

/// Closed resource facts safe to project across host boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResourceFact {
    MemoryPressure {
        level: PressureLevel,
        threshold: PressureThreshold,
        origin: SettingSource,
    },
    LaneCap {
        active: u64,
        limit: u64,
        lane_limit: LaneLimit,
        origin: SettingSource,
    },
    Processes {
        used: u64,
        limit: u64,
        reserved: u64,
    },
}

impl ResourceFact {
    pub fn project(&self, diagnostic: Diagnostic) -> Diagnostic {
        match self {
            Self::MemoryPressure { level, threshold, origin } => diagnostic
                .detail(format!("host memory pressure is {}; domains.fabric.lanes.memory_pressure_threshold is {} (source: {})", level.name(), threshold.name(), origin.description()))
                .next("reduce host memory pressure before retrying"),
            Self::LaneCap { active, limit, lane_limit, origin } => {
                let description = lane_limit.description();
                match lane_limit.setting() {
                    Some(setting) => diagnostic
                        .detail(format!("{active} of {limit} {description} are active; {setting} (source: {})", origin.description()))
                        .next(format!("wait for a writer lane to finish or adjust {setting}")),
                    None => diagnostic
                        .detail(format!("{active} of {limit} {description} are active; fixed host safety ceiling"))
                        .next("wait for an active lane to finish before retrying"),
                }
            }
            Self::Processes { used, limit, reserved } => diagnostic
                .detail(format!("host user process count is {used} of the operating system RLIMIT_NPROC soft limit {limit}; shipped reserved headroom is {reserved}"))
                .next("reduce host process use or raise the per-user process limit before retrying"),
        }
    }
}

/// I/O facts whose field and reason are safe to disclose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoFact {
    InputUnreadable,
}

impl IoFact {
    pub fn project(self, diagnostic: Diagnostic) -> Diagnostic {
        match self {
            Self::InputUnreadable => diagnostic
                .detail("input could not be read")
                .next("inspect the input source and retry"),
        }
    }
}

/// An explicitly recognised delivery command and its enforced output limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryCommand {
    GitWorktreeList,
    GitLog,
    GitStatus,
    GitFetch,
    GitPush,
    GitRevParse,
    GhPr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryLimit {
    pub stage: crate::run::WriterDeliveryStage,
    pub command: DeliveryCommand,
    pub limit: u64,
}

/// Typed facts retained in-process until an edge chooses its envelope. They are
/// never serialized as raw diagnostic strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticFact {
    CredentialGrant {
        access: &'static str,
    },
    DeliveryLimit(DeliveryLimit),
    DeliveryStage(crate::run::WriterDeliveryStage),
    GenerationConflict(u64),
    WorktreeUnregistered,
    Filesystem {
        operation: &'static str,
    },
    DiskPressure {
        available: u64,
        floor: u64,
        reclaimed: u64,
        origin: SettingSource,
    },
    DiskUnobserved,
    PromptLimit {
        task_index: Option<usize>,
        limit: u64,
    },
    Resource(ResourceFact),
}
impl DeliveryLimit {
    pub fn new(
        stage: crate::run::WriterDeliveryStage,
        tool: &str,
        command: &str,
        limit: usize,
    ) -> Option<Self> {
        let command = match (tool, command) {
            ("git", "worktree list") => DeliveryCommand::GitWorktreeList,
            ("git", "log") => DeliveryCommand::GitLog,
            ("git", "status") => DeliveryCommand::GitStatus,
            ("git", "fetch") => DeliveryCommand::GitFetch,
            ("git", "push") => DeliveryCommand::GitPush,
            ("git", "rev-parse") => DeliveryCommand::GitRevParse,
            ("gh", "pr") => DeliveryCommand::GhPr,
            _ => return None,
        };
        Some(Self {
            stage,
            command,
            limit: u64::try_from(limit).ok()?,
        })
    }
    pub fn detail(&self) -> String {
        let command = match self.command {
            DeliveryCommand::GitWorktreeList => "git worktree list",
            DeliveryCommand::GitLog => "git log",
            DeliveryCommand::GitStatus => "git status",
            DeliveryCommand::GitFetch => "git fetch",
            DeliveryCommand::GitPush => "git push",
            DeliveryCommand::GitRevParse => "git rev-parse",
            DeliveryCommand::GhPr => "gh pr",
        };
        format!(
            "host writer delivery failed at {:?}: {command} output exceeded the {}-byte host limit",
            self.stage, self.limit
        )
    }
}

/// Only explicitly selected safe facts belong in these fields; source errors,
/// captured output, prompts and credentials are not safe facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(skip)]
    pub fact: Option<DiagnosticFact>,
}

/// Projection fields beside an envelope's established code and message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiagnosticFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}
impl From<&Diagnostic> for DiagnosticFields {
    fn from(value: &Diagnostic) -> Self {
        Self {
            category: value.category.clone(),
            detail: value.detail.clone(),
            next: value.next.clone(),
        }
    }
}

impl Diagnostic {
    pub fn new(code: impl AsRef<str>) -> Self {
        Self {
            code: safe_text(code.as_ref()),
            category: None,
            detail: None,
            next: None,
            fact: None,
        }
    }
    pub fn category(mut self, value: impl AsRef<str>) -> Self {
        self.category = Some(safe_text(value.as_ref()));
        self
    }
    pub fn detail(mut self, value: impl AsRef<str>) -> Self {
        self.detail = Some(safe_text(value.as_ref()));
        self
    }
    pub fn next(mut self, value: impl AsRef<str>) -> Self {
        self.next = Some(safe_text(value.as_ref()));
        self
    }
    pub fn fact(mut self, fact: DiagnosticFact) -> Self {
        self.fact = Some(fact);
        self
    }
}

fn terminal_control(character: char) -> bool {
    character.is_control()
        || matches!(character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Escape terminal controls and truncate at a UTF-8 boundary after escaping.
pub fn safe_text(value: &str) -> String {
    let mut output = String::new();
    for character in value.chars() {
        let part = if terminal_control(character) {
            character.escape_default().to_string()
        } else {
            character.to_string()
        };
        if output.len() + part.len() > MAX_DIAGNOSTIC_BYTES {
            break;
        }
        output.push_str(&part);
    }
    output
}

/// A local CLI may show absolute paths. Other edges need an approved anchor.
#[derive(Debug, Clone, Copy)]
pub enum Destination<'a> {
    Local,
    Remote {
        repository: Option<&'a Path>,
        home: Option<&'a Path>,
    },
}

pub fn safe_path(path: &Path, destination: Destination<'_>) -> Option<String> {
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let display = match destination {
        Destination::Local => path.to_string_lossy().into_owned(),
        Destination::Remote { repository, home } => {
            if let Some(relative) = repository.and_then(|root| path.strip_prefix(root).ok()) {
                format!("./{}", relative.display())
            } else {
                let relative = home.and_then(|root| path.strip_prefix(root).ok())?;
                format!("~/{}", relative.display())
            }
        }
    };
    let escaped = safe_text(&display);
    if display
        .chars()
        .map(|c| {
            if terminal_control(c) {
                c.escape_default().to_string().len()
            } else {
                c.len_utf8()
            }
        })
        .sum::<usize>()
        > MAX_DIAGNOSTIC_BYTES
    {
        return None;
    }
    Some(escaped)
}

/// Quote a path as one shell argument after applying destination disclosure.
pub fn shell_path(path: &Path, destination: Destination<'_>) -> Option<String> {
    if !matches!(destination, Destination::Local) || path.to_str()?.chars().any(terminal_control) {
        return None;
    }
    let path = safe_path(path, destination)?;
    Some(format!("'{}'", path.replace('\'', "'\\''")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_has_code_and_optional_bounded_safe_fields() {
        let empty = serde_json::to_value(Diagnostic::new("INVALID")).unwrap();
        assert_eq!(empty, serde_json::json!({"code":"INVALID"}));
        let input = format!("secret-free\u{1b}[31m{}", "é".repeat(1000));
        let detail = Diagnostic::new("INVALID").detail(input).next("inspect");
        let text = detail.detail.unwrap();
        assert!(text.len() <= MAX_DIAGNOSTIC_BYTES);
        assert!(!text.chars().any(char::is_control));
        assert!(text.contains("\\u{1b}"));
        let bidi = safe_text("before\u{202e}after");
        assert!(bidi.contains("\\u{202e}"));
        assert!(!bidi.contains('\u{202e}'));
    }
    #[test]
    fn destination_paths_require_anchors_and_shell_quotes() {
        let remote = Destination::Remote {
            repository: Some(Path::new("/repo")),
            home: Some(Path::new("/home/me")),
        };
        assert_eq!(
            safe_path(Path::new("/repo/a b"), remote).as_deref(),
            Some("./a b")
        );
        assert_eq!(
            safe_path(Path::new("/home/me/keys"), remote).as_deref(),
            Some("~/keys")
        );
        assert_eq!(safe_path(Path::new("/elsewhere/a"), remote), None);
        assert_eq!(safe_path(Path::new("/repo/../elsewhere"), remote), None);
        assert_eq!(
            shell_path(Path::new("/repo/a'b"), Destination::Local).as_deref(),
            Some("'/repo/a'\\''b'")
        );
        let bidi_path = Path::new("/repo/a\u{202e}b");
        assert_eq!(shell_path(bidi_path, Destination::Local), None);
        assert!(
            safe_path(bidi_path, Destination::Local)
                .unwrap()
                .contains("\\u{202e}")
        );
    }

    #[test]
    fn delivery_limit_accepts_only_known_commands() {
        use crate::run::WriterDeliveryStage;
        let fact =
            DeliveryLimit::new(WriterDeliveryStage::Prepare, "git", "worktree list", 4096).unwrap();
        assert!(fact.detail().contains("git worktree list"));
        assert!(fact.detail().contains("4096-byte"));
        assert!(
            DeliveryLimit::new(WriterDeliveryStage::Prepare, "git", "token-sentinel", 4096)
                .is_none()
        );
        let projection = Diagnostic::new("IO").fact(DiagnosticFact::DeliveryLimit(fact));
        assert_eq!(
            serde_json::to_value(projection).unwrap(),
            serde_json::json!({"code": "IO"})
        );
    }
}
