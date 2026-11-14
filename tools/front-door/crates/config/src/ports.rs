//! One sourced, versioned declaration for repository integration ports.
use provenant_records::{CanonicalV1, FileDigest};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs, io,
    io::Read,
    path::{Component, Path, PathBuf},
    time::Duration,
};

pub const PORTS_CONFIG: &str = ".provenant/ports.json";
pub const PORTS_SCHEMA_VERSION: u16 = 2;

mod evidence;
pub use evidence::{PortEvidenceError, record_port_command};
const MAX_PORTS_BYTES: u64 = 65_536;
const BRANCH_TOKEN: &str = "<branch>";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortSource {
    Builtin,
    Host,
    Project,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortBackendKind {
    Builtin,
    Command,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitDisposition {
    Refused,
    Unknown,
}

/// Success and unknown codes are declared. Other codes use the declared typed
/// disposition, so a command never relies on an implicit exit-code guess.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandExitCodes {
    pub success: Vec<i32>,
    pub unknown: Vec<i32>,
    pub other: ExitDisposition,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortCommand {
    pub argv: Vec<String>,
    pub exit_codes: CommandExitCodes,
}

impl PortCommand {
    /// Digest the exact stored command declaration in its deterministic schema order.
    pub fn digest(&self) -> FileDigest {
        command_digest(self)
    }

    fn valid(&self) -> bool {
        let argv_valid = self.argv.first().is_some_and(|program| !program.is_empty());
        let mut codes = std::collections::BTreeSet::new();
        argv_valid
            && !self.exit_codes.success.is_empty()
            && self
                .exit_codes
                .success
                .iter()
                .chain(&self.exit_codes.unknown)
                .all(|code| codes.insert(*code))
    }
}

/// Typed result from executing a repository command port. The bounded process
/// capture is retained as evidence; callers decide whether to render it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PortCommandDisposition {
    Succeeded,
    Unknown,
    Refused,
    TimedOut,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortCommandExecution {
    pub command_digest: FileDigest,
    pub disposition: PortCommandDisposition,
    pub outcome: Option<provenant_os::exec::ExecOutcome>,
}

impl PortCommandExecution {
    /// Represent a command that could not be started while retaining the exact
    /// declaration whose execution was attempted.
    pub fn failed(command: &PortCommand) -> Self {
        Self {
            command_digest: command.digest(),
            disposition: PortCommandDisposition::Failed,
            outcome: None,
        }
    }
}

#[allow(
    clippy::expect_used,
    reason = "PortCommand fields serialize infallibly"
)]
fn command_digest(command: &PortCommand) -> FileDigest {
    let declaration = serde_json::to_vec(command)
        .expect("PortCommand serialization is infallible for its declared fields");
    CanonicalV1::file_digest(&declaration)
}

// Project declarations do not grant access to ambient credential variables.
const COMMAND_CONTEXT_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "TMP",
    "TEMP",
    "LANG",
    "LANGUAGE",
    "TZ",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_COLLATE",
    "LC_TIME",
    "LC_NUMERIC",
    "LC_MONETARY",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
];

fn command_environment(
    ambient: impl IntoIterator<Item = (OsString, OsString)>,
    explicit: &[(OsString, OsString)],
) -> Vec<(OsString, OsString)> {
    let mut environment: Vec<_> = ambient
        .into_iter()
        .filter(|(name, _)| {
            name.to_str()
                .is_some_and(|name| COMMAND_CONTEXT_ENV.contains(&name))
        })
        .collect();
    for (name, value) in explicit {
        environment.retain(|(existing, _)| existing != name);
        environment.push((name.clone(), value.clone()));
    }
    environment
}

/// Run a declared argv port with process-group cleanup and bounded output.
/// Additional arguments and environment values are appended by the owning
/// consumer; configured argv is never interpreted by a shell.
pub fn execute_port_command(
    executor: &dyn provenant_os::exec::Executor,
    command: &PortCommand,
    cwd: &Path,
    arguments: &[OsString],
    environment: &[(OsString, OsString)],
    timeout: Duration,
    cancel: &provenant_core::CancelToken,
) -> PortCommandExecution {
    use provenant_os::exec::{ExecOptions, StreamPolicy, Termination};
    let command_digest = command.digest();
    if !command.valid() {
        return PortCommandExecution {
            command_digest,
            disposition: PortCommandDisposition::Failed,
            outcome: None,
        };
    }
    let mut argv: Vec<_> = command.argv.iter().map(OsString::from).collect();
    argv.extend_from_slice(arguments);
    let env = command_environment(std::env::vars_os(), environment);
    let mut options = ExecOptions::new(&argv, cwd, timeout);
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture {
        limit: 8 * 1024 * 1024,
    };
    options.stderr = StreamPolicy::Tail { limit: 64 * 1024 };
    options.process_group = true;
    options.cleanup_process_group = true;
    options.group_watchdog = true;
    options.cancel = Some(cancel);
    let Ok(outcome) = executor.execute(options) else {
        return PortCommandExecution {
            command_digest,
            disposition: PortCommandDisposition::Failed,
            outcome: None,
        };
    };
    let disposition = if outcome.timed_out {
        PortCommandDisposition::TimedOut
    } else if outcome.cancelled {
        PortCommandDisposition::Cancelled
    } else {
        match outcome.termination {
            Termination::Exit(code)
                if i32::try_from(code)
                    .ok()
                    .is_some_and(|code| command.exit_codes.success.contains(&code)) =>
            {
                PortCommandDisposition::Succeeded
            }
            Termination::Exit(code)
                if i32::try_from(code)
                    .ok()
                    .is_some_and(|code| command.exit_codes.unknown.contains(&code)) =>
            {
                PortCommandDisposition::Unknown
            }
            Termination::Exit(_) if command.exit_codes.other == ExitDisposition::Unknown => {
                PortCommandDisposition::Unknown
            }
            Termination::Exit(_) => PortCommandDisposition::Refused,
            Termination::Signal(_) => PortCommandDisposition::Failed,
        }
    };
    PortCommandExecution {
        command_digest,
        disposition,
        outcome: Some(outcome),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PortBackend {
    Builtin,
    Command {
        command: PortCommand,
    },
    #[serde(rename = "none")]
    None,
}

impl PortBackend {
    fn kind(&self) -> PortBackendKind {
        match self {
            Self::Builtin => PortBackendKind::Builtin,
            Self::Command { .. } => PortBackendKind::Command,
            Self::None => PortBackendKind::None,
        }
    }

    fn valid(&self) -> bool {
        match self {
            Self::Command { command } => command.valid(),
            Self::Builtin | Self::None => true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    pub source: PortSource,
    pub backend: PortBackend,
}

impl Port {
    fn builtin() -> Self {
        Self {
            source: PortSource::Builtin,
            backend: PortBackend::Builtin,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyPort {
    pub source: PortSource,
    pub backend: PortBackend,
    pub base: String,
    pub workspace: PathBuf,
    pub remote: String,
    pub load_per_cpu: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_threads: Option<u32>,
    pub package_paths: Vec<PackagePath>,
    pub quarantine_command: Vec<String>,
    /// Tier names are repository-defined strings, not a closed numeric enum.
    pub tiers: BTreeMap<String, VerifyTier>,
    pub default_tier: String,
    pub integrations: Vec<Integration>,
    pub runners: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifySelection {
    Packages,
    Integrations,
    All,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyTier {
    pub commands: Vec<String>,
    pub selection: VerifySelection,
    pub warn_for_host_load: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackagePath {
    pub paths: Vec<String>,
    pub package: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Integration {
    pub paths: Vec<String>,
    pub package: String,
    pub binary: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LandBackend {
    Builtin,
    /// The records owner declares the command and unknown exit code. Exit 0
    /// means landed; other nonzero exits refuse unless the declared unknown code.
    Command {
        command: provenant_records::landing::RepositoryCommand,
    },
    #[serde(rename = "none")]
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LandPort {
    pub source: PortSource,
    pub backend: LandBackend,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorktreeLayoutPort {
    pub source: PortSource,
    pub backend: PortBackend,
    pub roots: Vec<PathBuf>,
    pub default: PathBuf,
}

impl WorktreeLayoutPort {
    pub fn unavailable_reason(&self) -> Option<&'static str> {
        match self.backend {
            PortBackend::Builtin => None,
            PortBackend::Command { .. } => {
                Some("worktree layout command backend is not implemented")
            }
            PortBackend::None => Some("the effective declaration disables this port"),
        }
    }

    /// Resolve the declared default path for a validated Git branch.
    pub fn path_for_branch(&self, branch: &str) -> io::Result<PathBuf> {
        if let Some(reason) = self.unavailable_reason() {
            return Err(io::Error::new(io::ErrorKind::Unsupported, reason));
        }
        if branch.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "worktree layout is unavailable",
            ));
        }
        let template = self.default.to_string_lossy();
        let rendered = template.replace(BRANCH_TOKEN, &branch.replace('/', "-"));
        let branch_component = template
            .split('/')
            .position(|component| component.contains(BRANCH_TOKEN))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid worktree template")
            })?;
        if rendered.len() > 4096
            || rendered.split('/').enumerate().any(|(index, component)| {
                component.len() > if index == branch_component { 240 } else { 255 }
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "worktree layout path is too long",
            ));
        }
        Ok(PathBuf::from(rendered))
    }

    /// Return the branch slug matched by the declared default template under
    /// any configured root. Secondary roots reuse that same template tail.
    pub fn branch_component<'a>(&self, relative: &'a Path) -> Option<&'a str> {
        if !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
            || !matches!(self.backend, PortBackend::Builtin)
        {
            return None;
        }
        let template_root = self
            .roots
            .iter()
            .filter(|root| self.default.starts_with(root))
            .max_by_key(|root| root.components().count())?;
        let template = self.default.strip_prefix(template_root).ok()?.to_str()?;
        let (prefix, suffix) = template.split_once(BRANCH_TOKEN)?;
        self.roots.iter().find_map(|root| {
            let actual = relative.strip_prefix(root).ok()?.to_str()?;
            let branch = actual.strip_prefix(prefix)?.strip_suffix(suffix)?;
            (!branch.is_empty() && !branch.contains('/')).then_some(branch)
        })
    }

    pub fn contains_worktree(&self, relative: &Path) -> bool {
        self.branch_component(relative).is_some()
    }
}

trait PortEntry {
    fn source(&self) -> PortSource;
    fn set_source(&mut self, source: PortSource);
    fn backend_kind(&self) -> PortBackendKind;
    fn valid(&self) -> bool;
    fn unavailable_reason(&self) -> Option<&'static str> {
        if !self.valid() {
            Some("the port declaration is invalid")
        } else if self.backend_kind() == PortBackendKind::None {
            Some("the effective declaration disables this port")
        } else {
            None
        }
    }
}

impl PortEntry for Port {
    fn source(&self) -> PortSource {
        self.source
    }
    fn set_source(&mut self, source: PortSource) {
        self.source = source;
    }
    fn backend_kind(&self) -> PortBackendKind {
        self.backend.kind()
    }
    fn valid(&self) -> bool {
        self.backend.valid()
    }
}
impl PortEntry for VerifyPort {
    fn source(&self) -> PortSource {
        self.source
    }
    fn set_source(&mut self, source: PortSource) {
        self.source = source;
    }
    fn backend_kind(&self) -> PortBackendKind {
        self.backend.kind()
    }
    fn valid(&self) -> bool {
        self.backend.valid()
            && relative(&self.workspace, true)
            && !self.base.is_empty()
            && !self.remote.is_empty()
            && self.load_per_cpu.is_finite()
            && self.load_per_cpu > 0.0
            && self.test_threads.map_or(true, |threads| threads > 0)
            && self.tiers.contains_key(&self.default_tier)
            && self
                .tiers
                .iter()
                .all(|(name, tier)| !name.is_empty() && !tier.commands.is_empty())
    }
}
impl PortEntry for LandPort {
    fn unavailable_reason(&self) -> Option<&'static str> {
        match self.backend {
            LandBackend::Builtin => None,
            LandBackend::Command { .. } => {
                Some("landing command strategy wiring is not implemented")
            }
            LandBackend::None => Some("the effective declaration disables this port"),
        }
    }
    fn source(&self) -> PortSource {
        self.source
    }
    fn set_source(&mut self, source: PortSource) {
        self.source = source;
    }
    fn backend_kind(&self) -> PortBackendKind {
        match &self.backend {
            LandBackend::Builtin => PortBackendKind::Builtin,
            LandBackend::Command { .. } => PortBackendKind::Command,
            LandBackend::None => PortBackendKind::None,
        }
    }
    fn valid(&self) -> bool {
        match &self.backend {
            LandBackend::Command { command } => command.valid(),
            LandBackend::Builtin | LandBackend::None => true,
        }
    }
}
impl PortEntry for WorktreeLayoutPort {
    fn unavailable_reason(&self) -> Option<&'static str> {
        WorktreeLayoutPort::unavailable_reason(self)
    }
    fn source(&self) -> PortSource {
        self.source
    }
    fn set_source(&mut self, source: PortSource) {
        self.source = source;
    }
    fn backend_kind(&self) -> PortBackendKind {
        self.backend.kind()
    }
    fn valid(&self) -> bool {
        self.backend.valid()
            && !self.roots.is_empty()
            && self
                .roots
                .iter()
                .all(|root| relative(root, false) && !root.to_string_lossy().contains(BRANCH_TOKEN))
            && self
                .roots
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == self.roots.len()
            && relative(&self.default, false)
            && self.default.to_string_lossy().matches(BRANCH_TOKEN).count() == 1
            && self.roots.iter().any(|root| self.default.starts_with(root))
    }
}

fn relative(path: &Path, allow_current: bool) -> bool {
    !path.as_os_str().is_empty()
        && path.components().all(|component| {
            matches!(component, Component::Normal(_))
                || (allow_current && component == Component::CurDir)
        })
}

macro_rules! declare_ports {
    ($($field:ident: $kind:ty),+ $(,)?) => {
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct PortTable {
            pub schema_version: u16,
            $(pub $field: $kind),+
        }

        impl PortTable {
            pub const PORT_NAMES: &'static [&'static str] = &[$(stringify!($field)),+];

            fn set_sources(&mut self, source: PortSource) {
                $(self.$field.set_source(source);)+
            }

            fn set_port_source(&mut self, name: &str, source: PortSource) -> bool {
                $(if name == stringify!($field) {
                    self.$field.set_source(source);
                    return true;
                })+
                false
            }

            pub fn entries(&self) -> impl Iterator<Item = (&'static str, PortSource, PortBackendKind, bool, Option<&'static str>)> + '_ {
                [$(
                    (stringify!($field), self.$field.source(), self.$field.backend_kind(), self.$field.valid(), self.$field.unavailable_reason()),
                )+].into_iter()
            }
        }
    }
}

declare_ports! {
    verify: VerifyPort,
    land: LandPort,
    ci_observation: Port,
    worktree_layout: WorktreeLayoutPort,
    naming: Port,
    post_create: Port,
    tracker: Port,
    release: Port,
    admission: Port,
}

fn apply_layer(
    table: &mut serde_json::Value,
    sources: &mut BTreeMap<String, PortSource>,
    path: &Path,
    source: PortSource,
) -> io::Result<()> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(MAX_PORTS_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PORTS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "port table exceeds size limit",
        ));
    }
    let mut overlay: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    let object = overlay.as_object_mut().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "port table must be an object")
    })?;
    if object
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(u64::from(PORTS_SCHEMA_VERSION))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid port table schema",
        ));
    }
    let target = table.as_object_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "compiled port table must be an object",
        )
    })?;
    for (name, value) in object.iter() {
        if name != "schema_version" {
            target.insert(name.clone(), value.clone());
            sources.insert(name.clone(), source);
        }
    }
    Ok(())
}

impl PortTable {
    pub fn builtin() -> Self {
        let mut table: Self = serde_json::from_str(include_str!("../assets/ports.json"))
            .expect("compiled port table asset must match its schema");
        table.set_sources(PortSource::Builtin);
        table
    }

    /// Resolve the configured path for a managed writer branch.
    pub fn managed_writer_path(&self, branch: &str) -> io::Result<PathBuf> {
        self.worktree_layout.path_for_branch(branch)
    }

    /// Identify a managed writer branch under any declared layout root.
    pub fn managed_writer_branch<'a>(&self, relative: &'a Path) -> Option<&'a str> {
        self.worktree_layout.branch_component(relative)
    }

    /// Resolve each configured worktree root beneath its owning project or
    /// Git common directory for host path protection.
    pub fn worktree_roots_under(&self, root: &Path) -> Vec<PathBuf> {
        self.worktree_layout
            .roots
            .iter()
            .map(|relative| root.join(relative))
            .collect()
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.schema_version != PORTS_SCHEMA_VERSION
            || self.entries().any(|(_, _, _, valid, _)| !valid)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid port table",
            ));
        }
        Ok(())
    }

    /// Resolve the effective table by applying explicit host entries to the
    /// compiled declarations, then explicit project entries. Omitted ports
    /// inherit their lower layer and retain that layer's source.
    pub fn read_for(project: Option<&Path>, host_state: &Path) -> io::Result<Self> {
        let table = Self::read_for_inspection(project, host_state)?;
        table.validate()?;
        Ok(table)
    }

    /// Inspect well-typed declarations without hiding semantically invalid ports.
    pub fn read_for_inspection(project: Option<&Path>, host_state: &Path) -> io::Result<Self> {
        let mut document = serde_json::to_value(Self::builtin()).map_err(io::Error::other)?;
        let mut sources = BTreeMap::new();
        apply_layer(
            &mut document,
            &mut sources,
            &host_state.join("ports.json"),
            PortSource::Host,
        )?;
        if let Some(project) = project {
            apply_layer(
                &mut document,
                &mut sources,
                &project.join(PORTS_CONFIG),
                PortSource::Project,
            )?;
        }
        let mut table: Self = serde_json::from_value(document).map_err(io::Error::other)?;
        for (name, source) in sources {
            if !table.set_port_source(&name, source) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unknown port entry",
                ));
            }
        }
        Ok(table)
    }
}

impl Default for PortTable {
    fn default() -> Self {
        Self::builtin()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_port_table_and_backend_cases_round_trip_from_the_declarations() {
        let table = PortTable::builtin();
        let value = serde_json::to_value(&table).unwrap();
        assert_eq!(PortTable::PORT_NAMES.len(), table.entries().count());
        for name in PortTable::PORT_NAMES {
            assert!(
                value.get(name).is_some(),
                "port declaration {name} is encodable"
            );
        }
        assert_eq!(serde_json::from_value::<PortTable>(value).unwrap(), table);

        let cases = [
            PortBackend::Builtin,
            PortBackend::Command {
                command: PortCommand {
                    argv: vec!["verify".into()],
                    exit_codes: CommandExitCodes {
                        success: vec![0],
                        unknown: vec![75],
                        other: ExitDisposition::Refused,
                    },
                },
            },
            PortBackend::None,
        ];
        for backend in cases {
            let mut layout = table.worktree_layout.clone();
            layout.backend = backend.clone();
            assert_eq!(
                layout.unavailable_reason().is_none(),
                layout.path_for_branch("feature/topic").is_ok()
            );
            let value = serde_json::to_value(&backend).unwrap();
            assert_eq!(
                serde_json::from_value::<PortBackend>(value).unwrap(),
                backend
            );
        }

        let land_cases = [
            LandBackend::Builtin,
            LandBackend::Command {
                command: provenant_records::landing::RepositoryCommand {
                    argv: vec!["provenant-land".into()],
                    unknown_exit_code: 75,
                    timeout_seconds: 30,
                },
            },
            LandBackend::None,
        ];
        for backend in land_cases {
            let value = serde_json::to_value(&backend).unwrap();
            assert_eq!(
                serde_json::from_value::<LandBackend>(value).unwrap(),
                backend
            );
        }

        let selection_cases = [
            VerifySelection::Packages,
            VerifySelection::Integrations,
            VerifySelection::All,
        ];
        for selection in selection_cases {
            let value = serde_json::to_value(selection).unwrap();
            assert_eq!(
                serde_json::from_value::<VerifySelection>(value).unwrap(),
                selection
            );
        }

        let command = provenant_records::landing::RepositoryCommand {
            argv: vec!["land".into()],
            unknown_exit_code: 75,
            timeout_seconds: 30,
        };
        assert_eq!(
            command.outcome(0),
            provenant_records::landing::RepositoryCommandOutcome::Landed,
        );
        assert_eq!(
            command.outcome(1),
            provenant_records::landing::RepositoryCommandOutcome::Refused,
        );
        assert_eq!(
            command.outcome(75),
            provenant_records::landing::RepositoryCommandOutcome::Unknown,
        );

        let source_cases = [PortSource::Builtin, PortSource::Host, PortSource::Project];
        for source in source_cases {
            let value = serde_json::to_value(source).unwrap();
            assert_eq!(serde_json::from_value::<PortSource>(value).unwrap(), source);
        }
    }

    #[test]
    fn worktree_layout_resolves_default_and_custom_roots_from_one_declaration() {
        let mut table = PortTable::builtin();
        assert!(table.validate().is_ok());
        {
            let custom = &mut table.worktree_layout;
            custom.roots = vec![
                PathBuf::from(".worktrees"),
                PathBuf::from(".claude/worktrees"),
            ];
            custom.default = PathBuf::from(".claude/worktrees/<branch>");
            assert_eq!(
                custom.path_for_branch("feature/topic").unwrap(),
                PathBuf::from(".claude/worktrees/feature-topic"),
            );
            assert_eq!(
                custom.branch_component(Path::new(".claude/worktrees/feature-topic")),
                Some("feature-topic"),
            );
            assert!(custom.contains_worktree(Path::new(".worktrees/other")));
            assert!(
                custom
                    .branch_component(Path::new(".claude/worktrees/deep/name"))
                    .is_none()
            );
            custom.roots = vec![PathBuf::from("trees"), PathBuf::from("legacy")];
            custom.default = PathBuf::from("trees/topic-<branch>/checkout");
            let path = custom.path_for_branch("feature/topic").unwrap();
            assert_eq!(path, PathBuf::from("trees/topic-feature-topic/checkout"));
            assert_eq!(custom.branch_component(&path), Some("feature-topic"));
            assert_eq!(
                custom.branch_component(Path::new("legacy/topic-feature-topic/checkout")),
                Some("feature-topic"),
            );
        }
        assert!(table.validate().is_ok());
        table.worktree_layout.roots = vec![PathBuf::from("trees/<branch>")];
        table.worktree_layout.default = PathBuf::from("trees/<branch>/checkout");
        assert!(table.validate().is_err());
        table = PortTable::builtin();
        table.worktree_layout.default = PathBuf::from("../outside/<branch>");
        assert!(table.validate().is_err());
    }

    #[test]
    fn command_environment_inherits_execution_context_only() {
        let ambient = COMMAND_CONTEXT_ENV
            .iter()
            .map(|name| (OsString::from(*name), OsString::from("context")))
            .chain([
                ("GH_TOKEN".into(), "credential".into()),
                ("SSH_AUTH_SOCK".into(), "agent".into()),
            ]);
        let result: BTreeMap<_, _> =
            command_environment(ambient, &[("PATH".into(), "explicit".into())])
                .into_iter()
                .collect();
        for name in COMMAND_CONTEXT_ENV {
            assert!(result.contains_key(&OsString::from(*name)));
        }
        assert_eq!(
            result.get(&OsString::from("PATH")),
            Some(&OsString::from("explicit"))
        );
        assert!(!result.contains_key(&OsString::from("GH_TOKEN")));
        assert!(!result.contains_key(&OsString::from("SSH_AUTH_SOCK")));
    }

    #[test]
    fn inspection_reports_invalid_ports_without_executing_them() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join(".provenant")).unwrap();
        std::fs::write(directory.path().join(PORTS_CONFIG), r#"{"schema_version":2,"naming":{"backend":{"kind":"command","command":{"argv":[],"exit_codes":{"success":[0],"unknown":[],"other":"refused"}}}}}"#).unwrap();
        let table =
            PortTable::read_for_inspection(Some(directory.path()), directory.path()).unwrap();
        let naming = table.entries().find(|entry| entry.0 == "naming").unwrap();
        assert!(!naming.3);
        assert!(naming.4.is_some());
        assert!(PortTable::read_for(Some(directory.path()), directory.path()).is_err());
    }

    #[test]
    fn command_digest_binds_argv_and_each_exit_disposition() {
        let base = PortCommand {
            argv: vec!["admit".into(), "--phase".into()],
            exit_codes: CommandExitCodes {
                success: vec![0],
                unknown: vec![75],
                other: ExitDisposition::Refused,
            },
        };
        assert_eq!(base.digest(), base.digest());
        let mut variants = Vec::new();
        let mut changed = base.clone();
        changed.argv[0] = "another-admit".into();
        variants.push(changed);
        let mut changed = base.clone();
        changed.exit_codes.success = vec![0, 2];
        variants.push(changed);
        let mut changed = base.clone();
        changed.exit_codes.unknown = vec![75, 76];
        variants.push(changed);
        let mut changed = base.clone();
        changed.exit_codes.other = ExitDisposition::Unknown;
        variants.push(changed);
        assert!(
            variants
                .iter()
                .all(|command| command.digest() != base.digest())
        );
    }

    #[test]
    fn explicit_host_and_project_ports_overlay_without_relabelling_omissions() {
        let temp = tempfile::tempdir().unwrap();
        let host = temp.path().join("host");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&host).unwrap();
        std::fs::create_dir_all(project.join(".provenant")).unwrap();
        std::fs::write(
            host.join("ports.json"),
            r#"{"schema_version":2,"ci_observation":{"source":"project","backend":{"kind":"none"}}}"#,
        )
        .unwrap();
        std::fs::write(
            project.join(PORTS_CONFIG),
            r#"{"schema_version":2,"naming":{"source":"host","backend":{"kind":"none"}}}"#,
        )
        .unwrap();

        let table = PortTable::read_for(Some(&project), &host).unwrap();
        let sources: BTreeMap<_, _> = table
            .entries()
            .map(|(name, source, _, _, _)| (name, source))
            .collect();
        assert_eq!(sources["ci_observation"], PortSource::Host);
        assert_eq!(sources["naming"], PortSource::Project);
        assert_eq!(sources["admission"], PortSource::Builtin);
        assert_eq!(table.ci_observation.backend_kind(), PortBackendKind::None);
        assert_eq!(table.naming.backend_kind(), PortBackendKind::None);
        let host_only = PortTable::read_for(None, &host).unwrap();
        assert_eq!(host_only.ci_observation.source, PortSource::Host);
        assert_eq!(host_only.naming.source, PortSource::Builtin);
    }

    #[test]
    fn project_location_overrides_serialized_source_labels() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".provenant")).unwrap();
        let mut forged = PortTable::builtin();
        let bytes = serde_json::to_vec(&forged).unwrap();
        std::fs::write(root.path().join(PORTS_CONFIG), bytes).unwrap();
        let loaded = PortTable::read_for(Some(root.path()), root.path()).unwrap();
        assert!(
            loaded
                .entries()
                .all(|(_, source, _, _, _)| source == PortSource::Project)
        );
        forged.set_sources(PortSource::Project);
        assert_eq!(
            serde_json::to_value(&loaded).unwrap(),
            serde_json::to_value(&forged).unwrap()
        );
    }
}
