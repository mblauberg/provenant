//! One hardened Git environment for host operations that talk to a remote, and
//! the configuration inspection that finds what a lane can influence.
//!
//! The threat model is the lane: it can write its worktree and its scratch,
//! and cannot write `.git/config`, hooks or `info/`. The repository and user
//! configuration belong to the user and are trusted, so a helper they name
//! elsewhere stays allowed. What is refused is any configuration file or
//! program that lives where a lane can write.
use provenant_os::fs::canonicalize;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

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
        "Git configuration includes {file}, which lives where lanes can write; remove that include or move the file"
    )]
    LaneConfig { file: String },
    #[error(
        "{key} runs {program}, which lives where lanes can write; point it at a program outside the project"
    )]
    LaneProgram { key: String, program: String },
    #[error("Git configuration could not be read")]
    Unreadable,
}

/// Locations a lane can write. A path is inside when its lexical form or its
/// symlink-resolved form falls under one of the roots.
#[derive(Clone, Debug, Default)]
pub struct LaneRoots(Vec<PathBuf>);

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
        Self(all)
    }

    /// The lane areas of a project checkout: its worktrees and scratch.
    pub fn project(root: &Path) -> Self {
        Self::new([".worktrees", ".provenant", ".agent-run"].map(|name| root.join(name)))
    }

    /// The lane areas for a repository whose private and common Git
    /// directories are known. A linked worktree is itself lane-writable, and
    /// the project that owns the common directory contributes its lane areas.
    pub fn for_repository(root: &Path, private: &Path, common: &Path, extra: &Self) -> Self {
        let mut roots = extra.0.clone();
        if private != common {
            roots.push(root.to_path_buf());
        }
        if common.file_name().is_some_and(|name| name == ".git")
            && let Some(project) = common.parent()
        {
            roots.extend(Self::project(project).0);
        }
        Self::new(roots)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `path` must be absolute; a relative path is never inside.
    pub fn contains(&self, path: &Path) -> bool {
        if !path.is_absolute() {
            return false;
        }
        let lexical = normalise(path);
        let resolved = resolve(&lexical);
        self.0
            .iter()
            .any(|root| lexical.starts_with(root) || resolved.starts_with(root))
    }
}

fn normalise(path: &Path) -> PathBuf {
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

/// The argv tail that produces what [`parse_listing`] reads.
pub const LISTING_ARGS: [&str; 5] = ["config", "--includes", "--show-origin", "-z", "--list"];

/// Parse `git config --includes --show-origin -z --list` output.
pub fn parse_listing(bytes: &[u8]) -> Result<Vec<ConfigEntry>, TransportError> {
    if !bytes.is_empty() && !bytes.ends_with(&[0]) {
        return Err(TransportError::Unreadable);
    }
    let mut fields = bytes.split(|byte| *byte == 0);
    let mut entries = Vec::new();
    while let Some(origin) = fields.next() {
        if origin.is_empty() && fields.clone().next().is_none() {
            break;
        }
        let entry = fields.next().ok_or(TransportError::Unreadable)?;
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
        && let Some(home) = std::env::var_os("HOME")
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

fn check_program(
    key: &str,
    value: &str,
    base: &Path,
    roots: &LaneRoots,
) -> Result<(), TransportError> {
    for word in program_paths(value) {
        if roots.contains(&expand(&word, base)) {
            return Err(TransportError::LaneProgram {
                key: key.to_owned(),
                program: word,
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
) -> Result<(), TransportError> {
    for entry in entries {
        if let Some(file) = entry.file(base)
            && roots.contains(&file)
        {
            return Err(TransportError::LaneConfig {
                file: file.display().to_string(),
            });
        }
        if is_program_key(&entry.key)
            && let Some(value) = &entry.value
        {
            check_program(&entry.key, value, base, roots)?;
        }
    }
    Ok(())
}

/// The environment's own transport programs get the same refusal.
pub fn check_environment(
    env: &[(OsString, OsString)],
    base: &Path,
    roots: &LaneRoots,
) -> Result<(), TransportError> {
    for (name, value) in env {
        let Some(name) = name.to_str() else { continue };
        if matches!(
            name,
            "GIT_SSH" | "GIT_SSH_COMMAND" | "GIT_ASKPASS" | "SSH_ASKPASS" | "GIT_PROXY_COMMAND"
        ) {
            check_program(name, &value.to_string_lossy(), base, roots)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

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
    fn parses_valueless_and_valued_entries() -> TestResult {
        let bytes = listing(&[
            ("file:.git/config", "core.bare\nfalse"),
            ("file:/x/y.cfg", "user.flag"),
            ("command line:", "a.b\n"),
        ]);
        let entries = parse_listing(&bytes)?;
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].value.as_deref(), Some("false"));
        assert_eq!(entries[1].value, None);
        assert_eq!(entries[2].value.as_deref(), Some(""));
        assert_eq!(
            entries[0].file(Path::new("/repo")),
            Some(PathBuf::from("/repo/.git/config"))
        );
        assert_eq!(entries[2].file(Path::new("/repo")), None);
        assert!(parse_listing(b"file:x").is_err());
        assert!(parse_listing(b"").is_ok_and(|entries| entries.is_empty()));
        Ok(())
    }

    #[test]
    fn lane_roots_resolve_links_and_missing_tails() -> TestResult {
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        std::fs::create_dir_all(root.join("lane"))?;
        std::os::unix::fs::symlink(root.join("lane"), root.join("alias"))?;
        let roots = LaneRoots::new([root.join("lane")]);
        assert!(roots.contains(&root.join("lane/a/b")));
        assert!(roots.contains(&root.join("alias/not/yet/created")));
        assert!(roots.contains(&root.join("other/../lane/x")));
        assert!(!roots.contains(&root.join("laneish")));
        assert!(!roots.contains(Path::new("relative/lane")));
        Ok(())
    }

    #[test]
    fn refuses_only_programs_and_files_inside_lane_roots() -> TestResult {
        let roots = LaneRoots::new([PathBuf::from("/project/.worktrees")]);
        let base = Path::new("/project");
        let check = |key: &str, value: &str| {
            let bytes = listing(&[("file:/project/.git/config", &format!("{key}\n{value}"))]);
            check_listing(&parse_listing(&bytes)?, base, &roots)
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
            check_listing(&parse_listing(&lane_file)?, base, &roots),
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
        assert!(check_environment(&env, Path::new("/project"), &roots).is_err());
        let env = [(
            OsString::from("GIT_ASKPASS"),
            OsString::from("/usr/bin/ask"),
        )];
        assert!(check_environment(&env, Path::new("/project"), &roots).is_ok());
    }
}
