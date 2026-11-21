//! One hardened Git environment for host operations that talk to a remote, and
//! the configuration inspection that finds what a lane can influence.
//!
//! The threat model is the lane: it can write its worktree and its scratch,
//! and cannot write `.git/config`, hooks or `info/`. The repository and user
//! configuration belong to the user and are trusted, so a helper they name
//! elsewhere stays allowed. What is refused is any configuration file or
//! program that lives where a lane can write.
use provenant_os::fs::canonicalize;
pub use provenant_os::git_config::ConfigContext;
use std::ffi::OsString;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

/// Bound each part of a Git-config-read cause. Paths and keys are names, not
/// values; this keeps an error bounded and one line even when Git reports a
/// long origin or an unexpected diagnostic.
pub(crate) const CONFIG_CAUSE_BOUND: usize = 192;

/// Replace control characters and bound a cause part so a refusal stays one
/// line and cannot smuggle a terminal escape or an unbounded diagnostic.
pub(crate) fn cause_text(value: impl fmt::Display) -> String {
    let raw = value.to_string();
    let clean: String = raw
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    if clean.chars().count() <= CONFIG_CAUSE_BOUND {
        return clean;
    }
    let mut bounded: String = clean.chars().take(CONFIG_CAUSE_BOUND).collect();
    bounded.push('…');
    bounded
}

/// A bounded, secret-free explanation of a failed Git configuration read.
///
/// It names the operation, the config file and the key or directive involved,
/// plus a bounded underlying diagnostic. Config *values* are never carried:
/// keys and paths only, so a credential in a config file cannot leak through
/// a refusal. `key` is a static label from the code, never a string taken from
/// the config file being inspected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitConfigCause {
    /// Stable label for the read that failed.
    pub operation: &'static str,
    /// The config file or directory involved, when one is known.
    pub file: Option<String>,
    /// Static key or directive label involved, never its value.
    pub key: Option<&'static str>,
    /// Bounded underlying error, such as a Git exit status or parse failure.
    pub detail: String,
}

impl GitConfigCause {
    pub(crate) fn new(
        operation: &'static str,
        file: Option<&Path>,
        key: Option<&'static str>,
        detail: impl fmt::Display,
    ) -> Self {
        Self {
            operation,
            file: file.map(|path| cause_text(path.display())),
            key,
            detail: cause_text(detail),
        }
    }
}

impl fmt::Display for GitConfigCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.operation)?;
        if let Some(file) = &self.file {
            write!(formatter, " on {file}")?;
        }
        if let Some(key) = self.key {
            write!(formatter, " for key {key}")?;
        }
        if !self.detail.is_empty() {
            write!(formatter, ": {}", self.detail)?;
        }
        Ok(())
    }
}

impl std::error::Error for GitConfigCause {}

/// Build a contextual cause for a failure detail from the shared command
/// context. The core owns the context type; transport only renders its failures.
pub fn cause(context: &ConfigContext, detail: impl fmt::Display) -> GitConfigCause {
    GitConfigCause::new(context.operation, Some(&context.file), context.key, detail)
}

/// Build an opaque-but-identified transport failure from the same context.
pub fn unreadable(context: &ConfigContext, detail: impl fmt::Display) -> TransportError {
    TransportError::Unreadable {
        cause: cause(context, detail),
    }
}

/// Config applied to every host advertisement and fetch: no hooks, no
/// fsmonitor, and only the transports delivery uses.
pub const CONFIG: [(&str, &str); 7] = [
    ("core.hooksPath", "/dev/null"),
    ("core.fsmonitor", "false"),
    ("protocol.allow", "never"),
    ("protocol.ssh.allow", "always"),
    ("protocol.https.allow", "always"),
    ("protocol.http.allow", "always"),
    ("protocol.file.allow", "always"),
];

/// Environment variables that select a repository or answer a prompt. The
/// host's own transport and configuration variables (`GIT_SSH`,
/// `GIT_ASKPASS`, `GIT_CONFIG_COUNT` for an `http.extraHeader` token, and the
/// like) are kept: they are the user's, and `check_listing` still refuses any
/// program they name inside a lane area.
fn removed_from_host(name: &str) -> bool {
    matches!(
        name,
        "GIT_DIR"
            | "GIT_WORK_TREE"
            | "GIT_COMMON_DIR"
            | "GIT_INDEX_FILE"
            | "GIT_OBJECT_DIRECTORY"
            | "GIT_ALTERNATE_OBJECT_DIRECTORIES"
            | "GIT_NAMESPACE"
            | "GIT_SHALLOW_FILE"
            | "GIT_TERMINAL_PROMPT"
    )
}

/// The `-c key=value` arguments of [`CONFIG`], to place before the subcommand.
pub fn config_args() -> Vec<OsString> {
    CONFIG
        .iter()
        .flat_map(|(key, value)| ["-c".into(), format!("{key}={value}").into()])
        .collect()
}

/// The host environment with repository selectors and ambient config
/// overrides removed, and prompts disabled. `extra` is appended last, so the
/// caller can bind `GIT_DIR`, `HOME` and the like explicitly.
pub fn environment(
    host: impl IntoIterator<Item = (OsString, OsString)>,
    extra: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    let extra: Vec<_> = extra.into_iter().collect();
    let mut env: Vec<_> = host
        .into_iter()
        .filter(|(key, _)| {
            !key.to_str().is_some_and(removed_from_host)
                && !extra.iter().any(|(name, _)| name == key)
        })
        .collect();
    env.push(("GIT_TERMINAL_PROMPT".into(), "0".into()));
    env.extend(extra);
    env
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TransportError {
    #[error(
        "{context}: Git configuration includes {file}, which lives where lanes can write; remove that include or move the file"
    )]
    LaneConfig {
        file: &'static str,
        context: ConfigContext,
    },
    // The program is a config *value* and is deliberately not carried: naming
    // it could leak a credential embedded in a helper command. The key names
    // the offending setting and the context names the read; that is enough to
    // act on the refusal without disclosing a value.
    #[error(
        "{context}: {key} names a program in a lane-writable location; point it at a program outside the project"
    )]
    LaneProgram {
        key: &'static str,
        context: ConfigContext,
    },
    #[error("Git configuration could not be read: {cause}")]
    Unreadable { cause: GitConfigCause },
    #[error("could not resolve lane roots during {step}: {detail}")]
    LaneRoots { step: &'static str, detail: String },
    /// A malformed port table is a port error, not a Git configuration error.
    /// The detail already names the file and key that must be corrected.
    #[error("port configuration could not be read: {detail}")]
    UnreadablePorts { detail: String },
}

/// Locations a lane can write. A path is inside when its lexical form or its
/// symlink-resolved form falls under one of the roots.
#[derive(Clone, Debug, Default)]
pub struct LaneRoots(Vec<PathBuf>, Vec<PathBuf>);

impl LaneRoots {
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut all = Vec::new();
        for root in roots {
            let lexical = normalise(&root);
            let resolved = resolve(&lexical);
            for path in [lexical, resolved] {
                if !all.contains(&path) {
                    all.push(path);
                }
            }
        }
        Self(all, Vec::new())
    }

    /// The lane areas of a project checkout: its configured worktrees and scratch.
    pub fn project(root: &Path, host_state: &Path) -> Result<Self, TransportError> {
        let ports =
            provenant_config::PortTable::read_for(Some(root), host_state).map_err(|error| {
                TransportError::UnreadablePorts {
                    detail: error.to_string(),
                }
            })?;
        Ok(Self::project_roots(root, ports))
    }

    /// Project-declared roots when the caller has no validated host state to
    /// read host-level port overrides from.
    pub fn project_without_host_state(root: &Path) -> Result<Self, TransportError> {
        let ports = provenant_config::PortTable::read_project_only(root).map_err(|error| {
            TransportError::UnreadablePorts {
                detail: error.to_string(),
            }
        })?;
        Ok(Self::project_roots(root, ports))
    }

    fn project_roots(root: &Path, ports: provenant_config::PortTable) -> Self {
        Self::layout_roots(root, ports, true)
    }

    /// Host-managed roots beneath a common Git directory with no project
    /// checkout, as in a verified bare repository. Project scratch does not
    /// belong to the common metadata directory.
    fn host_only(root: &Path, host_state: &Path) -> Result<Self, TransportError> {
        let ports = provenant_config::PortTable::read_for(None, host_state).map_err(|error| {
            TransportError::UnreadablePorts {
                detail: error.to_string(),
            }
        })?;
        Ok(Self::layout_roots(root, ports, false))
    }

    /// Compiled host roots for a common-only layout when no validated state
    /// root is available to provide host overrides.
    fn host_only_without_host_state(root: &Path) -> Self {
        Self::layout_roots(root, provenant_config::PortTable::builtin(), false)
    }

    fn layout_roots(root: &Path, ports: provenant_config::PortTable, project: bool) -> Self {
        let mut roots = ports.worktree_roots_under(root);
        if project {
            roots.extend([".provenant", ".agent-run"].map(|name| root.join(name)));
        }
        Self::new(roots)
    }

    /// The lane areas for a repository whose private and common Git
    /// directories are known. A linked worktree is itself lane-writable, and
    /// the project that owns the common directory contributes its declared areas.
    /// `project=None` is a verified common-only layout and uses host roots under
    /// the common directory without treating its metadata as a project checkout.
    pub fn for_repository(
        root: &Path,
        private: &Path,
        common: &Path,
        project: Option<&Path>,
        extra: &Self,
        host_state: Option<&Path>,
    ) -> Result<Self, TransportError> {
        let mut roots = extra.0.clone();
        if private != common {
            roots.push(root.to_path_buf());
        }
        let layout = match (project, host_state) {
            (Some(project), Some(host_state)) => Self::project(project, host_state)?,
            (Some(project), None) => Self::project_without_host_state(project)?,
            (None, Some(host_state)) => Self::host_only(common, host_state)?,
            (None, None) => Self::host_only_without_host_state(common),
        };
        roots.extend(layout.0);
        Ok(Self::new(roots))
    }

    /// Roots already proved before a linked checkout's project layout is
    /// known. This bootstrap is intentionally limited to the selected checkout,
    /// explicit runtime roots, and host-managed roots under its common dir.
    pub(crate) fn bootstrap_for_repository(
        root: &Path,
        private: &Path,
        common: &Path,
        extra: &Self,
        host_state: &Path,
    ) -> Result<Self, TransportError> {
        let mut roots = extra.0.clone();
        if private != common {
            roots.push(root.to_path_buf());
        }
        roots.extend(Self::host_only(common, host_state)?.0);
        Ok(Self::new(roots))
    }

    pub fn into_paths(self) -> Vec<PathBuf> {
        self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Exempt a verified unsandboxed lane's own checkout, never an alias
    /// that resolves outside it. Other lane roots remain protected.
    pub fn except_owned_worktree(mut self, worktree: &Path) -> Self {
        let lexical = normalise(worktree);
        let resolved = resolve(&lexical);
        self.1.extend([lexical, resolved]);
        self
    }

    /// `path` must be absolute; a relative path is never inside.
    pub fn contains(&self, path: &Path) -> bool {
        if !path.is_absolute() {
            return false;
        }
        // `..` after a symlink climbs out of the link's target, not out of
        // the link's own directory, so the raw path is resolved as well as the
        // lexical one.
        let lexical = normalise(path);
        let resolved = resolve(&lexical);
        let raw = resolve(path);
        if self.1.iter().any(|owned| {
            lexical.starts_with(owned)
                && resolved.starts_with(owned)
                && raw.starts_with(owned)
                && !self.0.iter().any(|root| {
                    root != owned
                        && root.starts_with(owned)
                        && (lexical.starts_with(root)
                            || resolved.starts_with(root)
                            || raw.starts_with(root))
                })
                && path
                    .ancestors()
                    .take_while(|parent| normalise(parent).starts_with(owned))
                    .all(|parent| resolve(parent).starts_with(owned))
        }) {
            return false;
        }
        self.0.iter().any(|root| {
            lexical.starts_with(root)
                || resolved.starts_with(root)
                || raw.starts_with(root)
                // A lane can retarget a symlink that it owns, even if the
                // resolved target lies outside. Resolve each parent separately
                // so aliases of the lane root do not hide that ownership.
                || path.ancestors().skip(1).any(|parent| resolve(parent).starts_with(root))
        })
    }
}

pub(crate) fn normalise(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normal.pop();
            }
            Component::CurDir => {}
            other => normal.push(other.as_os_str()),
        }
    }
    normal
}

/// Resolve symlinks in the longest existing prefix, then append the rest.
fn resolve(path: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut head = path.to_path_buf();
    loop {
        if let Ok(resolved) = canonicalize(&head) {
            return rest.iter().rev().fold(resolved, |acc, part| acc.join(part));
        }
        match (head.file_name().map(ToOwned::to_owned), head.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                head = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// One entry of `git config --list --show-origin -z`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigEntry {
    /// `file:<path>`, `command line:`, `standard input:` or `blob:<id>`.
    pub origin: String,
    /// Section and variable names are lowercase; a subsection keeps its case.
    pub key: String,
    pub value: Option<String>,
}

impl ConfigEntry {
    /// The file this entry was read from, resolved against `base` when Git
    /// printed a relative path.
    pub fn file(&self, base: &Path) -> Option<PathBuf> {
        let path = Path::new(self.origin.strip_prefix("file:")?);
        Some(if path.is_absolute() {
            normalise(path)
        } else {
            normalise(&base.join(path))
        })
    }
}

/// Parse `git config --includes --show-origin -z --list` output. `ctx` names
/// the read so a malformed listing fails as an identified config error rather
/// than an opaque one.
pub fn parse_listing(
    bytes: &[u8],
    ctx: &ConfigContext,
) -> Result<Vec<ConfigEntry>, TransportError> {
    if !bytes.is_empty() && !bytes.ends_with(&[0]) {
        return Err(unreadable(ctx, "git config listing is not NUL-terminated"));
    }
    let mut fields = bytes.split(|byte| *byte == 0);
    let mut entries = Vec::new();
    while let Some(origin) = fields.next() {
        if origin.is_empty() && fields.clone().next().is_none() {
            break;
        }
        let entry = fields
            .next()
            .ok_or_else(|| unreadable(ctx, "truncated git config listing"))?;
        let (key, value) = match entry.iter().position(|byte| *byte == b'\n') {
            Some(split) => (&entry[..split], Some(&entry[split + 1..])),
            None => (entry, None),
        };
        entries.push(ConfigEntry {
            origin: String::from_utf8_lossy(origin).into_owned(),
            key: String::from_utf8_lossy(key).into_owned(),
            value: value.map(|value| String::from_utf8_lossy(value).into_owned()),
        });
    }
    Ok(entries)
}

pub fn is_include_directive(key: &str) -> bool {
    key == "include.path" || (key.starts_with("includeif.") && key.ends_with(".path"))
}

/// Where an include directive in `from` points. Git resolves a relative path
/// against the including file's directory and expands a leading `~/`.
pub fn include_target(from: &Path, value: &str, home: &Path) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        return home.join(rest);
    }
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        from.parent().unwrap_or(Path::new("/")).join(path)
    }
}

/// Most files followed while looking for a lane-writable include. Git itself
/// stops at depth ten.
const MAX_INCLUDE_FILES: usize = 64;

/// Lists one config file without following includes, or `None` when absent.
pub type ReadConfigFile<'a> =
    dyn FnMut(&Path) -> Result<Option<Vec<ConfigEntry>>, TransportError> + 'a;

/// Refuse an include directive whose target lies where a lane can write,
/// whether or not the target exists or Git would read it today.
///
/// The effective listing only shows files Git read, so a missing target or an
/// `includeIf` condition that is false for now (`gitdir:`, `onbranch:`,
/// `hasconfig:`) would escape [`check_listing`], and a lane could create the
/// file in the window before Git rereads the configuration. `top` lists
/// directives without following includes; `read` lists one file or returns
/// `None` when absent. Targets outside lane areas may include onward.
pub fn check_includes(
    top: &[ConfigEntry],
    base: &Path,
    home: &Path,
    roots: &LaneRoots,
    read: &mut ReadConfigFile<'_>,
    ctx: &ConfigContext,
) -> Result<(), TransportError> {
    let mut pending = Vec::new();
    let collect = |entries: &[ConfigEntry], origin: Option<&Path>, pending: &mut Vec<PathBuf>| {
        for entry in entries {
            let Some(value) = entry.value.as_deref().filter(|value| !value.is_empty()) else {
                continue;
            };
            if is_include_directive(&entry.key) {
                let from = origin
                    .map(Path::to_path_buf)
                    .or_else(|| entry.file(base))
                    .unwrap_or_else(|| base.join("config"));
                pending.push(include_target(&from, value, home));
            }
        }
    };
    collect(top, None, &mut pending);
    let mut visited = std::collections::BTreeSet::new();
    while let Some(target) = pending.pop() {
        if roots.contains(&target) {
            return Err(TransportError::LaneConfig {
                file: "lane-writable include target",
                context: ctx.clone(),
            });
        }
        if !visited.insert(target.clone()) {
            continue;
        }
        if visited.len() > MAX_INCLUDE_FILES {
            return Err(unreadable(ctx, "too many config include targets to verify"));
        }
        if let Some(entries) = read(&target)? {
            collect(&entries, Some(&target), &mut pending);
        }
    }
    Ok(())
}

/// Keys whose value names a program Git runs for a transport.
fn is_program_key(key: &str) -> bool {
    matches!(
        key,
        "core.sshcommand" | "core.gitproxy" | "core.askpass" | "credential.helper"
    ) || (key.starts_with("credential.") && key.ends_with(".helper"))
        || (key.starts_with("remote.")
            && (key.ends_with(".uploadpack") || key.ends_with(".receivepack")))
}

/// Path-like words in a program value. The value is a shell command, so split
/// on whitespace and shell punctuation, drop quotes, and look at `NAME=path`
/// words too. Bare words resolve through `PATH`, which is not a lane location.
pub fn program_paths(value: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for word in value.split(|c: char| c.is_whitespace() || ";|&()<>`".contains(c)) {
        let word = word.trim_matches(['\'', '"', '!']);
        for candidate in [word, word.split_once('=').map_or("", |(_, rest)| rest)] {
            let candidate = candidate.trim_matches(['\'', '"']);
            if candidate.starts_with('/') || candidate.starts_with('~') || candidate.contains('/') {
                paths.push(candidate.to_owned());
            }
        }
    }
    paths
}

fn expand(word: &str, base: &Path) -> PathBuf {
    if let Some(rest) = word.strip_prefix("~/")
        && let Some(home) = std::env::var_os(provenant_core::consumer_env::HOME)
    {
        return Path::new(&home).join(rest);
    }
    let path = Path::new(word);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// A config key may contain credentials in its subsection. Only these labels
/// may cross the error boundary.
pub fn key_class(key: &str) -> &'static str {
    if key.starts_with("credential.") {
        "credential setting"
    } else if key.starts_with("remote.") {
        "remote setting"
    } else if key.starts_with("branch.") {
        "branch setting"
    } else if key.starts_with("include") {
        "include directive"
    } else if key.starts_with("core.") {
        "core setting"
    } else {
        "Git configuration setting"
    }
}

fn check_program(
    key: &str,
    value: &str,
    base: &Path,
    roots: &LaneRoots,
    ctx: &ConfigContext,
) -> Result<(), TransportError> {
    for word in program_paths(value) {
        if roots.contains(&expand(&word, base)) {
            return Err(TransportError::LaneProgram {
                key: key_class(key),
                context: ctx.clone(),
            });
        }
    }
    Ok(())
}

/// Refuse a transport operation when its effective configuration is read from
/// a lane-writable file, or names a transport program in a lane-writable
/// location. `base` is the working directory of the Git process.
pub fn check_listing(
    entries: &[ConfigEntry],
    base: &Path,
    roots: &LaneRoots,
    ctx: &ConfigContext,
) -> Result<(), TransportError> {
    for entry in entries {
        if let Some(file) = entry.file(base)
            && roots.contains(&file)
        {
            return Err(TransportError::LaneConfig {
                file: "lane-writable config file",
                context: ctx.clone(),
            });
        }
        if is_program_key(&entry.key)
            && let Some(value) = &entry.value
        {
            check_program(&entry.key, value, base, roots, ctx)?;
        }
    }
    Ok(())
}

/// The environment's own transport programs get the same refusal.
pub fn check_environment(
    env: &[(OsString, OsString)],
    base: &Path,
    roots: &LaneRoots,
    ctx: &ConfigContext,
) -> Result<(), TransportError> {
    for (name, value) in env {
        let Some(name) = name.to_str() else { continue };
        if matches!(
            name,
            "GIT_SSH" | "GIT_SSH_COMMAND" | "GIT_ASKPASS" | "SSH_ASKPASS" | "GIT_PROXY_COMMAND"
        ) {
            check_program(name, &value.to_string_lossy(), base, roots, ctx)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn test_ctx() -> ConfigContext {
        ConfigContext::new("test config read", Path::new("/project/.git/config"), None)
    }

    fn listing(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (origin, entry) in entries {
            bytes.extend_from_slice(origin.as_bytes());
            bytes.push(0);
            bytes.extend_from_slice(entry.as_bytes());
            bytes.push(0);
        }
        bytes
    }

    #[test]
    fn owned_worktree_exception_keeps_other_roots_and_aliases_protected() -> TestResult {
        let temp = tempfile::tempdir()?;
        let parent = std::fs::canonicalize(temp.path())?;
        let own = parent.join("own");
        let other = parent.join("other");
        let nested = own.join("nested");
        std::fs::create_dir_all(&nested)?;
        std::fs::create_dir(&other)?;
        std::os::unix::fs::symlink(&other, own.join("other-alias"))?;
        let roots = LaneRoots::new([parent.clone(), nested.clone()]).except_owned_worktree(&own);
        assert!(!roots.contains(&own.join("config")));
        assert!(roots.contains(&nested.join("config")));
        assert!(roots.contains(&other.join("config")));
        assert!(roots.contains(&own.join("other-alias/config")));
        Ok(())
    }

    #[test]
    fn parses_valueless_and_valued_entries() -> TestResult {
        let bytes = listing(&[
            ("file:.git/config", "core.bare\nfalse"),
            ("file:/x/y.cfg", "user.flag"),
            ("command line:", "a.b\n"),
        ]);
        let entries = parse_listing(&bytes, &test_ctx())?;
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].value.as_deref(), Some("false"));
        assert_eq!(entries[1].value, None);
        assert_eq!(entries[2].value.as_deref(), Some(""));
        assert_eq!(
            entries[0].file(Path::new("/repo")),
            Some(PathBuf::from("/repo/.git/config"))
        );
        assert_eq!(entries[2].file(Path::new("/repo")), None);
        assert!(parse_listing(b"file:x", &test_ctx()).is_err());
        assert!(parse_listing(b"", &test_ctx()).is_ok_and(|entries| entries.is_empty()));
        Ok(())
    }

    #[test]
    fn lane_root_link_shapes_resolve_with_kernel_path_semantics() -> TestResult {
        #[derive(Clone, Copy)]
        enum LinkShape {
            AliasedRootAndMissingTail,
            FinalLinkUnderRoot,
            ParentAfterLink,
        }

        for (label, shape) in [
            (
                "aliased root and missing tail",
                LinkShape::AliasedRootAndMissingTail,
            ),
            ("final link under root", LinkShape::FinalLinkUnderRoot),
            ("parent after link", LinkShape::ParentAfterLink),
        ] {
            let temp = tempfile::tempdir()?;
            let root = std::fs::canonicalize(temp.path())?;
            let (roots, included, excluded) = match shape {
                LinkShape::AliasedRootAndMissingTail => {
                    std::fs::create_dir_all(root.join("lane"))?;
                    std::os::unix::fs::symlink(root.join("lane"), root.join("alias"))?;
                    (
                        LaneRoots::new([root.join("lane")]),
                        vec![
                            root.join("lane/a/b"),
                            root.join("alias/not/yet/created"),
                            root.join("other/../lane/x"),
                        ],
                        vec![root.join("laneish"), PathBuf::from("relative/lane")],
                    )
                }
                LinkShape::FinalLinkUnderRoot => {
                    std::fs::create_dir(root.join("lane"))?;
                    std::fs::write(root.join("outside"), "config")?;
                    std::os::unix::fs::symlink(root.join("lane"), root.join("alias"))?;
                    std::os::unix::fs::symlink(root.join("outside"), root.join("lane/config"))?;
                    (
                        LaneRoots::new([root.join("lane")]),
                        vec![root.join("alias/config")],
                        vec![root.join("outside")],
                    )
                }
                LinkShape::ParentAfterLink => {
                    std::fs::create_dir_all(root.join("lane/deep"))?;
                    std::fs::create_dir_all(root.join("safe"))?;
                    // `safe/link/../x` resolves under lane for the kernel.
                    std::os::unix::fs::symlink(root.join("lane/deep"), root.join("safe/link"))?;
                    (
                        LaneRoots::new([root.join("lane")]),
                        vec![root.join("safe/link/../not-yet")],
                        vec![root.join("safe/not-yet")],
                    )
                }
            };
            for path in included {
                assert!(roots.contains(&path), "{label}: expected {path:?} inside");
            }
            for path in excluded {
                assert!(!roots.contains(&path), "{label}: expected {path:?} outside");
            }
        }
        Ok(())
    }

    fn directive(file: &str, key: &str, value: &str) -> ConfigEntry {
        ConfigEntry {
            origin: format!("file:{file}"),
            key: key.to_owned(),
            value: Some(value.to_owned()),
        }
    }

    #[test]
    fn project_lane_roots_use_the_sourced_worktree_layout() -> TestResult {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let config = root.join(".provenant");
        std::fs::create_dir_all(&config)?;
        let mut ports = provenant_config::PortTable::builtin();
        ports.worktree_layout.roots = vec![PathBuf::from(".claude/worktrees")];
        ports.worktree_layout.default = PathBuf::from(".claude/worktrees/<branch>");
        std::fs::write(config.join("ports.json"), serde_json::to_vec(&ports)?)?;

        let roots = LaneRoots::project(&root, &root)?;
        assert!(roots.contains(&root.join(".claude/worktrees/topic/helper")));
        assert!(roots.contains(&root.join(".provenant/state")));
        assert!(roots.contains(&root.join(".agent-run/state")));
        assert!(!roots.contains(&root.join(".worktrees/topic/helper")));
        Ok(())
    }

    #[test]
    fn bare_common_lane_roots_use_host_ports_without_project_scratch() -> TestResult {
        let temp = tempfile::tempdir()?;
        let parent = std::fs::canonicalize(temp.path())?;
        let common = parent.join("common.git");
        let linked = parent.join("linked-checkout");
        let private = common.join("worktrees/linked");
        std::fs::create_dir_all(&private)?;
        std::fs::create_dir(&linked)?;

        let state = parent.join("state");
        provenant_os::fixture::private_state_dir(&state)?;
        let mut host_ports = provenant_config::PortTable::builtin();
        host_ports.worktree_layout.roots = vec![PathBuf::from("host-managed")];
        host_ports.worktree_layout.default = PathBuf::from("host-managed/<branch>");
        std::fs::write(state.join("ports.json"), serde_json::to_vec(&host_ports)?)?;

        // A common Git directory is not a project checkout. A colocated
        // project file must not contribute project-specific roots or scratch.
        let project_config = common.join(provenant_config::PORTS_CONFIG);
        std::fs::create_dir_all(
            project_config
                .parent()
                .ok_or("common project config parent")?,
        )?;
        let mut project_ports = provenant_config::PortTable::builtin();
        project_ports.worktree_layout.roots = vec![PathBuf::from("project-only")];
        project_ports.worktree_layout.default = PathBuf::from("project-only/<branch>");
        std::fs::write(project_config, serde_json::to_vec(&project_ports)?)?;

        let extra = LaneRoots::default();
        let with_host =
            LaneRoots::for_repository(&linked, &private, &common, None, &extra, Some(&state))?;
        assert!(with_host.contains(&linked.join("config.worktree")));
        assert!(with_host.contains(&common.join("host-managed/branch/config")));
        assert!(!with_host.contains(&common.join("project-only/branch/config")));
        assert!(!with_host.contains(&common.join(".provenant/state/config")));
        assert!(!with_host.contains(&common.join(".agent-run/state/config")));

        // With no validated host state, only compiled host policy applies.
        let without_host =
            LaneRoots::for_repository(&linked, &private, &common, None, &extra, None)?;
        assert!(without_host.contains(&linked.join("config.worktree")));
        assert!(without_host.contains(&common.join(".worktrees/branch/config")));
        assert!(!without_host.contains(&common.join("host-managed/branch/config")));
        assert!(!without_host.contains(&common.join("project-only/branch/config")));
        assert!(!without_host.contains(&common.join(".provenant/state/config")));
        assert!(!without_host.contains(&common.join(".agent-run/state/config")));
        Ok(())
    }

    #[test]
    fn include_targets_in_lane_areas_are_refused_even_when_unread() {
        let roots = LaneRoots::new([PathBuf::from("/project/.worktrees")]);
        let base = Path::new("/project");
        let home = Path::new("/home/user");
        // The files a followed include would list, by path.
        let check = |top: &[ConfigEntry], files: &[(&str, Vec<ConfigEntry>)]| {
            let mut read = |file: &Path| {
                Ok(files
                    .iter()
                    .find(|(name, _)| Path::new(name) == file)
                    .map(|(_, entries)| entries.clone()))
            };
            check_includes(top, base, home, &roots, &mut read, &test_ctx())
        };
        let config = "/project/.git/config";
        // Absolute, relative to the including file and `..`-laden paths,
        // through `include` and each `includeIf` condition, none of them read.
        for (key, value) in [
            ("include.path", "/project/.worktrees/x/cfg"),
            ("include.path", "../.worktrees/x/cfg"),
            ("include.path", "../.worktrees/../.worktrees/x/cfg"),
            (
                "includeif.onbranch:nothing.path",
                "/project/.worktrees/x/cfg",
            ),
            ("includeif.gitdir:/elsewhere/.path", "../.worktrees/x/cfg"),
            (
                "includeif.hasconfig:remote.*.url:never.path",
                "../.worktrees/x/cfg",
            ),
        ] {
            assert!(
                matches!(
                    check(&[directive(config, key, value)], &[]),
                    Err(TransportError::LaneConfig { .. })
                ),
                "{key}={value}"
            );
        }
        // Home-relative, and other locations, are fine.
        assert!(check(&[directive(config, "include.path", "~/x/cfg")], &[]).is_ok());
        assert!(
            check(
                &[directive(config, "user.name", "/project/.worktrees/x")],
                &[]
            )
            .is_ok()
        );

        // An include outside the lane areas is followed, and its own includes
        // are held to the same rule.
        let outside = [directive(config, "include.path", "/etc/shared.cfg")];
        let nested = [(
            "/etc/shared.cfg",
            vec![directive(
                "/etc/shared.cfg",
                "include.path",
                "/project/.worktrees/x/cfg",
            )],
        )];
        assert!(matches!(
            check(&outside, &nested),
            Err(TransportError::LaneConfig { .. })
        ));
        let inert = [(
            "/etc/shared.cfg",
            vec![directive("/etc/shared.cfg", "user.name", "x")],
        )];
        assert!(check(&outside, &inert).is_ok());
        // A cycle terminates.
        let cycle = [(
            "/etc/shared.cfg",
            vec![directive(
                "/etc/shared.cfg",
                "include.path",
                "/etc/shared.cfg",
            )],
        )];
        assert!(check(&outside, &cycle).is_ok());
    }

    #[test]
    fn refusal_does_not_reveal_key_or_value() {
        let key = "credential.https://user:password@example.invalid.helper";
        let value = "/project/.worktrees/secret-helper";
        let entries = [ConfigEntry {
            origin: "file:/project/.git/config".into(),
            key: key.into(),
            value: Some(value.into()),
        }];
        let error = check_listing(
            &entries,
            Path::new("/project"),
            &LaneRoots::new([PathBuf::from("/project/.worktrees")]),
            &test_ctx(),
        )
        .unwrap_err();
        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(!rendered.contains(key));
            assert!(!rendered.contains("user:password"));
            assert!(!rendered.contains(value));
            assert!(rendered.contains("credential setting"));
        }
    }

    #[test]
    fn refuses_only_programs_and_files_inside_lane_roots() -> TestResult {
        let roots = LaneRoots::new([PathBuf::from("/project/.worktrees")]);
        let base = Path::new("/project");
        let check = |key: &str, value: &str| {
            let bytes = listing(&[("file:/project/.git/config", &format!("{key}\n{value}"))]);
            check_listing(
                &parse_listing(&bytes, &test_ctx())?,
                base,
                &roots,
                &test_ctx(),
            )
        };
        let lane = "/project/.worktrees/x/helper";
        for (key, value) in [
            ("core.sshcommand", format!("{lane} -o X=1")),
            ("core.sshcommand", format!("ssh -o ProxyCommand={lane}")),
            ("credential.helper", format!("!'{lane}' get")),
            (
                "credential.https://h.example.helper",
                format!("GH='/x' {lane}"),
            ),
            ("remote.origin.uploadpack", lane.to_owned()),
            ("remote.up.receivepack", format!("env {lane}")),
            ("remote.origin.uploadpack", ".worktrees/x/helper".to_owned()),
        ] {
            assert!(
                matches!(check(key, &value), Err(TransportError::LaneProgram { .. })),
                "{key}={value}"
            );
        }
        for (key, value) in [
            ("core.sshcommand", "/usr/bin/ssh -o BatchMode=yes"),
            ("core.sshcommand", "ssh"),
            ("credential.helper", "osxkeychain"),
            (
                "credential.helper",
                "!/opt/homebrew/bin/gh auth git-credential",
            ),
            ("remote.origin.uploadpack", "/usr/bin/git-upload-pack"),
            ("user.name", "/project/.worktrees/not-a-program"),
        ] {
            assert!(check(key, value).is_ok(), "{key}={value}");
        }
        let lane_file = listing(&[("file:/project/.worktrees/x/extra.cfg", "user.name\nsomeone")]);
        assert!(matches!(
            check_listing(
                &parse_listing(&lane_file, &test_ctx())?,
                base,
                &roots,
                &test_ctx()
            ),
            Err(TransportError::LaneConfig { .. })
        ));
        Ok(())
    }

    #[test]
    fn environment_drops_selectors_and_keeps_user_transport_variables() {
        let host = [
            ("GIT_DIR", "/lane/.git"),
            ("GIT_CONFIG_COUNT", "1"),
            ("GIT_INDEX_FILE", "/lane/index"),
            ("GIT_TERMINAL_PROMPT", "1"),
            ("GIT_SSH", "/usr/bin/ssh"),
            ("PATH", "/usr/bin"),
        ]
        .map(|(key, value)| (OsString::from(key), OsString::from(value)));
        let env = environment(host, [("GIT_DIR".into(), "/host/.git".into())]);
        let get = |name: &str| {
            env.iter()
                .filter(|(key, _)| key == name)
                .map(|(_, value)| value.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(get("GIT_DIR"), ["/host/.git"]);
        assert_eq!(get("GIT_TERMINAL_PROMPT"), ["0"]);
        assert_eq!(get("GIT_SSH"), ["/usr/bin/ssh"]);
        assert_eq!(get("GIT_CONFIG_COUNT"), ["1"]);
        assert!(get("GIT_INDEX_FILE").is_empty());
        assert_eq!(get("PATH"), ["/usr/bin"]);
    }

    #[test]
    fn environment_programs_in_lane_roots_are_refused() {
        let roots = LaneRoots::new([PathBuf::from("/project/.worktrees")]);
        let env = [(
            OsString::from("GIT_ASKPASS"),
            OsString::from("/project/.worktrees/x/ask"),
        )];
        assert!(check_environment(&env, Path::new("/project"), &roots, &test_ctx()).is_err());
        let env = [(
            OsString::from("GIT_ASKPASS"),
            OsString::from("/usr/bin/ask"),
        )];
        assert!(check_environment(&env, Path::new("/project"), &roots, &test_ctx()).is_ok());
    }
}
