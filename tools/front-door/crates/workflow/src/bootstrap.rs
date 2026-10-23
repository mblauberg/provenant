//! Bootstrap unit 02: mission creation and CONFIG KNOBS substitution.
use crate::policy::strip;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub const PROG: &str = "bootstrap-autopilot.py";
pub const REQUIRED_KNOBS: &[&str] = &[
    "DOMAIN",
    "MISSION",
    "LOCKED_CONSTRAINTS",
    "BUILD_CEILING",
    "ESCALATION_GATES",
];
pub const SUBST_TARGETS: &[&str] = &["README.md", "STATE.md", "HANDOFF.md"];
pub const SCAN_TARGETS: &[&str] = &["README.md", "STATE.md", "HANDOFF.md", "QUEUE.md"];
pub const DEFAULT_RUNAWAY_CAPS: &str = "max ~4 concurrent jobs; max ~3 active forks; fork-depth <=2; ~3 runs/unit before escalation; bounded-retry <=2; long-wake ~3600s";

#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error("{0}")]
    Usage(String),
    #[error("'{0}'")]
    UnknownFallback(String),
    #[error("PosixPath('{}') has an empty name", path.display())]
    EmptyName { path: PathBuf },
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: {source}")]
    Encoding {
        path: PathBuf,
        #[source]
        source: std::string::FromUtf8Error,
    },
}
impl BootstrapError {
    pub fn exit_code(&self) -> i32 {
        if matches!(self, Self::Usage(_)) {
            2
        } else {
            1
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapArgs {
    pub mission_id: String,
    pub repo_root: String,
    pub domain: String,
    pub force: bool,
    pub dry_run: bool,
    pub quiet: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedArgs {
    Help,
    Values(BootstrapArgs),
}

pub fn usage() -> &'static str {
    include_str!("bootstrap_defaults/usage.txt")
}
pub fn err(message: &str) -> String {
    if message.is_empty() {
        format!("{PROG}:\n")
    } else {
        format!("{PROG}: {message}\n")
    }
}
/// Quiet mode suppresses notes, never warnings.
pub fn warn(message: &str, _quiet: bool) -> String {
    format!("{PROG}: WARNING: {message}\n")
}

pub fn parse_args(argv: &[String]) -> Result<ParsedArgs, BootstrapError> {
    let mut values = BootstrapArgs::default();
    let mut positionals = Vec::new();
    let mut index = 0;
    while let Some(arg) = argv.get(index) {
        match arg.as_str() {
            "--" => break, // The Python helper discards the remainder, rather than consuming it.
            "--id" | "--repo-root" | "--domain" => {
                index += 1;
                let value = argv
                    .get(index)
                    .ok_or_else(|| BootstrapError::Usage(format!("missing value for {arg}")))?;
                match arg.as_str() {
                    "--id" => values.mission_id = value.clone(),
                    "--repo-root" => values.repo_root = value.clone(),
                    _ => values.domain = value.clone(),
                }
            }
            "--force" => values.force = true,
            "--dry-run" => values.dry_run = true,
            "-q" | "--quiet" => values.quiet = true,
            "-h" | "--help" => return Ok(ParsedArgs::Help),
            _ if arg.starts_with('-') => {
                return Err(BootstrapError::Usage(format!(
                    "unknown option: {arg} (try --help)"
                )));
            }
            _ => positionals.push(arg.clone()),
        }
        index += 1;
    }
    let mut positionals = positionals.into_iter();
    if values.mission_id.is_empty() {
        values.mission_id = positionals.next().unwrap_or_default();
    }
    if values.domain.is_empty() {
        values.domain = positionals.next().unwrap_or_default();
    }
    if let Some(extra) = positionals.next() {
        return Err(BootstrapError::Usage(format!(
            "unexpected extra argument: {extra} (try --help)"
        )));
    }
    Ok(ParsedArgs::Values(values))
}

fn universal_newlines(s: String) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}
pub fn safe_read(path: &Path) -> Result<Option<String>, BootstrapError> {
    match fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(universal_newlines)
            .map(Some)
            .map_err(|source| BootstrapError::Encoding {
                path: path.into(),
                source,
            }),
        Err(_) => Ok(None),
    }
}
pub fn git_output(args: &[OsString], cwd: Option<&Path>) -> Result<Option<String>, BootstrapError> {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let Ok(mut child) = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    else {
        return Ok(None);
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let _status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let reap_deadline = Instant::now() + Duration::from_secs(1);
                while Instant::now() < reap_deadline {
                    if child.try_wait().ok().flatten().is_some() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                return Ok(None);
            }
            Err(source) => {
                let _ = child.kill();
                return Err(BootstrapError::Io {
                    path: "git".into(),
                    source,
                });
            }
        }
    };
    let output = child
        .wait_with_output()
        .map_err(|source| BootstrapError::Io {
            path: "git".into(),
            source,
        })?;
    let stdout = String::from_utf8(output.stdout).map_err(|source| BootstrapError::Encoding {
        path: "git stdout".into(),
        source,
    })?;
    String::from_utf8(output.stderr).map_err(|source| BootstrapError::Encoding {
        path: "git stderr".into(),
        source,
    })?;
    Ok(output
        .status
        .success()
        .then(|| strip(&universal_newlines(stdout)).into()))
}
/// Preserve the original helper's replace and cleanup effects. This helper is
/// not a descriptor-bound custody primitive and must not be used as one.
pub fn atomic_write(path: &Path, content: &str) -> Result<bool, BootstrapError> {
    let name = match path.components().next_back() {
        Some(Component::Normal(name)) => name,
        Some(Component::ParentDir) => std::ffi::OsStr::new(".."),
        _ => {
            return Err(BootstrapError::EmptyName {
                path: if path.as_os_str().is_empty() {
                    ".".into()
                } else {
                    path.into()
                },
            });
        }
    };
    let mut filename = name.to_os_string();
    filename.push(format!(".tmp.{}", std::process::id()));
    let temporary = path
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(filename);
    if fs::write(&temporary, content)
        .and_then(|()| fs::rename(&temporary, path))
        .is_ok()
    {
        return Ok(true);
    }
    match fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(BootstrapError::Io {
                path: temporary,
                source,
            });
        }
    }
    Ok(false)
}
pub fn fallback_content(name: &str, domain: &str) -> Result<String, BootstrapError> {
    let content = match name {
        "GOAL.md" => {
            return Ok(include_str!("bootstrap_defaults/GOAL.md").replacen(
                "{{DOMAIN}}",
                if domain.is_empty() {
                    "{{DOMAIN}}"
                } else {
                    domain
                },
                1,
            ));
        }
        "STATE.md" => include_str!("bootstrap_defaults/STATE.md"),
        "QUEUE.md" => include_str!("bootstrap_defaults/QUEUE.md"),
        "HANDOFF.md" => include_str!("bootstrap_defaults/HANDOFF.md"),
        "README.md" => include_str!("bootstrap_defaults/README.md"),
        _ => return Err(BootstrapError::UnknownFallback(name.into())),
    };
    Ok(content.into())
}
/// Keep the source dictionary's first-insertion order when a knob is replaced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigKnobs(Vec<(String, String)>);
impl ConfigKnobs {
    fn insert(&mut self, key: String, value: String) {
        if let Some((_, current)) = self.0.iter_mut().find(|(name, _)| name == &key) {
            *current = value;
        } else {
            self.0.push((key, value));
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.iter()
            .find_map(|(name, value)| (name == key).then_some(value))
    }
}
impl Serialize for ConfigKnobs {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in self.iter() {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}
impl<'de> Deserialize<'de> for ConfigKnobs {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = ConfigKnobs;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("an ordered string-to-string knob object")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                let mut result = ConfigKnobs::default();
                while let Some((key, value)) = map.next_entry()? {
                    result.insert(key, value);
                }
                Ok(result)
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

pub fn parse_knobs(goal: &str) -> ConfigKnobs {
    let mut result = ConfigKnobs::default();
    let mut in_block = false;
    for line in goal.split([
        '\n', '\r', '\u{b}', '\u{c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}',
        '\u{2029}',
    ]) {
        if line.trim_end_matches(crate::policy::whitespace) == "```config-knobs" {
            in_block = true;
            continue;
        }
        if line.trim_end_matches(crate::policy::whitespace) == "```" {
            in_block = false;
            continue;
        }
        if !in_block {
            continue;
        }
        let line = line.trim_start_matches(crate::policy::whitespace);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if !strip(key).is_empty() {
                result.insert(strip(key).into(), strip(value).into());
            }
        }
    }
    result
}

/// Values that vary between a real invocation and a deterministic fixture.
/// Filesystem mutations remain in the workflow; process, clock and entropy
/// access stay behind this host seam.
pub trait BootstrapHost {
    fn cwd(&mut self) -> Result<PathBuf, BootstrapError>;
    /// Observe Git commands with a bounded wait; timeout is treated as unavailable.
    fn git_output(&mut self, args: &[OsString], cwd: Option<&Path>) -> Option<String>;
    fn utc_stamp(&mut self) -> Result<String, BootstrapError>;
    fn random_suffix(&mut self) -> Result<String, BootstrapError>;
    fn has_program(&mut self, name: &str) -> bool;
    /// Match `shutil.copy2`: copy contents, permissions and modification time.
    fn copy2(&mut self, source: &Path, destination: &Path) -> Result<(), BootstrapError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

fn output(exit_code: i32, stdout: String, stderr: String) -> BootstrapOutput {
    BootstrapOutput {
        exit_code,
        stdout,
        stderr,
    }
}

pub fn validate_mission_id(mission_id: &str) -> Result<(), BootstrapError> {
    if mission_id.is_empty() {
        return Err(BootstrapError::Usage(
            "a mission id is required (try --help)".into(),
        ));
    }
    if mission_id.contains('/') {
        return Err(BootstrapError::Usage(format!(
            "mission id must not contain '/': {mission_id}"
        )));
    }
    if mission_id.starts_with('.') {
        return Err(BootstrapError::Usage(format!(
            "mission id must not start with '.': {mission_id}"
        )));
    }
    if mission_id.contains("..") {
        return Err(BootstrapError::Usage(format!(
            "mission id must not contain '..': {mission_id}"
        )));
    }
    if !mission_id
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    {
        return Err(BootstrapError::Usage(format!(
            "mission id must be a path-safe slug (letters, digits, '.', '_', '-' only): {mission_id}"
        )));
    }
    Ok(())
}

pub fn mission_slug(mission_id: &str) -> String {
    mission_id
        .to_lowercase()
        .chars()
        .map(|c| match c {
            '.' | '_' => '-',
            c => c,
        })
        .take(32)
        .collect()
}

fn as_os_args(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn first_line(text: &str) -> Option<&str> {
    text.lines().next()
}

fn is_mission_candidate(path: &Path, slug: &str) -> bool {
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let marker = format!("-mission-{slug}-");
    let Some(index) = name.rfind(&marker) else {
        return false;
    };
    let suffix = &name[index + marker.len()..];
    // Python's glob prefix has a leading wildcard, which may be empty.
    suffix.chars().count() == 6
}

fn path_is_within(child: &Path, parent: &Path) -> bool {
    child.strip_prefix(parent).is_ok()
}

fn seed_domain(contents: &str, domain: &str) -> Option<String> {
    let mut changed = false;
    let mut result = String::with_capacity(contents.len() + domain.len());
    for part in contents.split_inclusive('\n') {
        let line = part.strip_suffix('\n').unwrap_or(part);
        let newline = if part.ends_with('\n') { "\n" } else { "" };
        let trimmed = line.trim_end_matches([' ', '\t']);
        if let Some((before, after)) = trimmed.split_once("{{DOMAIN}}") {
            if after.is_empty()
                && before
                    .rfind('=')
                    .is_some_and(|equals| before[..equals].trim_matches([' ', '\t']) == "DOMAIN")
            {
                result.push_str(before);
                result.push_str(domain);
                result.push_str(newline);
                changed = true;
                continue;
            }
        }
        result.push_str(line);
        result.push_str(newline);
    }
    changed.then_some(result)
}

fn placeholders(text: &str) -> BTreeSet<String> {
    let bytes = text.as_bytes();
    let mut found = BTreeSet::new();
    let mut cursor = 0;
    while cursor + 1 < bytes.len() {
        if bytes.get(cursor..cursor + 2) != Some(b"{{") {
            cursor += 1;
            continue;
        }
        let body_start = cursor + 2;
        let Some(relative_end) = text.get(body_start..).and_then(|tail| tail.find("}}")) else {
            break;
        };
        let end = body_start + relative_end;
        let body = &text[body_start..end];
        if !body.is_empty() && body.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
            found.insert(format!("{{{{{body}}}}}"));
            cursor = end + 2;
        } else {
            cursor += 2;
        }
    }
    found
}

fn usage_error(message: &str, quiet: bool) -> BootstrapOutput {
    let _ = quiet;
    output(2, String::new(), err(message))
}

/// Execute the source workflow through an explicit host seam. The caller owns
/// argv acquisition and provides the actual skill directory.
pub fn run_main<H: BootstrapHost>(
    argv: &[String],
    skill_root: &Path,
    host: &mut H,
) -> Result<BootstrapOutput, BootstrapError> {
    let parsed = match parse_args(argv) {
        Ok(parsed) => parsed,
        Err(BootstrapError::Usage(message)) => return Ok(usage_error(&message, false)),
        Err(error) => return Err(error),
    };
    let ParsedArgs::Values(args) = parsed else {
        return Ok(output(0, usage().into(), String::new()));
    };
    let BootstrapArgs {
        mission_id,
        repo_root: repo_root_arg,
        domain,
        force,
        dry_run,
        quiet,
    } = args;
    if let Err(error) = validate_mission_id(&mission_id) {
        return match error {
            BootstrapError::Usage(message) => Ok(usage_error(&message, quiet)),
            other => Err(other),
        };
    }

    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut note = |line: &str| {
        if !quiet {
            stdout.push_str(line);
            stdout.push('\n');
        }
    };
    let warning = |stderr: &mut String, line: &str| {
        stderr.push_str(&warn(line, quiet));
    };

    let mut repo_root = if !repo_root_arg.is_empty() {
        let supplied = PathBuf::from(&repo_root_arg);
        if !supplied.is_dir() {
            return Ok(usage_error(
                &format!("--repo-root does not exist or is not a directory: {repo_root_arg}"),
                quiet,
            ));
        }
        fs::canonicalize(&supplied).map_err(|source| BootstrapError::Io {
            path: supplied,
            source,
        })?
    } else if let Some(root) = host.git_output(&as_os_args(&["rev-parse", "--show-toplevel"]), None)
    {
        fs::canonicalize(&root).map_err(|source| BootstrapError::Io {
            path: root.into(),
            source,
        })?
    } else {
        let cwd = host.cwd()?;
        let resolved =
            fs::canonicalize(&cwd).map_err(|source| BootstrapError::Io { path: cwd, source })?;
        warning(
            &mut stderr,
            &format!(
                "no git repo detected: anchoring .agent-run/ to the current directory ({}).",
                display_path(&resolved)
            ),
        );
        resolved
    };

    let root_string = display_path(&repo_root);
    let common_args = [
        "-C",
        root_string.as_str(),
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
    ];
    if let Some(common_dir) = host.git_output(&as_os_args(&common_args), None) {
        let common = PathBuf::from(common_dir);
        if common.file_name().is_some_and(|name| name == ".git") {
            if let Some(parent) = common.parent() {
                repo_root = fs::canonicalize(parent).map_err(|source| BootstrapError::Io {
                    path: parent.into(),
                    source,
                })?;
            }
        }
    }

    let agent_run = repo_root.join(".agent-run");
    let runs_root = agent_run.join("runs");
    for path in [&agent_run, &runs_root] {
        if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Ok(usage_error(
                &format!("refusing: {} is a symlink", display_path(path)),
                quiet,
            ));
        }
        if path.exists() && !path.is_dir() {
            return Ok(usage_error(
                &format!(
                    "refusing: {} exists and is not a directory",
                    display_path(path)
                ),
                quiet,
            ));
        }
    }
    let slug = mission_slug(&mission_id);
    let mut candidates = fs::read_dir(&runs_root)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_mission_candidate(path, &slug))
        .collect::<Vec<_>>();
    candidates.sort();
    let mut mission_dir = None;
    for candidate in candidates {
        if !candidate.join("GOAL.md").is_file() || !candidate.join(".mission-id").is_file() {
            continue;
        }
        let existing = safe_read(&candidate.join(".mission-id"))?.unwrap_or_default();
        if first_line(&existing) == Some(mission_id.as_str()) {
            if mission_dir.is_some() {
                return Ok(usage_error(
                    &format!("multiple missions match {mission_id}; select one by path"),
                    quiet,
                ));
            }
            mission_dir = Some(candidate);
        }
    }
    let mission_dir = if let Some(path) = mission_dir {
        path
    } else {
        let stamp = host.utc_stamp()?;
        let suffix = host.random_suffix()?;
        if suffix.len() != 6
            || !suffix
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err(BootstrapError::Usage(
                "host random suffix must be six lowercase hexadecimal digits".into(),
            ));
        }
        runs_root.join(format!("{stamp}-mission-{slug}-{suffix}"))
    };
    if fs::symlink_metadata(&mission_dir).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Ok(usage_error(
            &format!(
                "refusing: mission path is a symlink: {}",
                display_path(&mission_dir)
            ),
            quiet,
        ));
    }

    let scratch_root = agent_run.join("scratch");
    if !dry_run {
        fs::create_dir_all(&scratch_root).map_err(|source| BootstrapError::Io {
            path: scratch_root.clone(),
            source,
        })?;
        let repo_display = display_path(&repo_root);
        let inside_args = [
            "-C",
            repo_display.as_str(),
            "rev-parse",
            "--is-inside-work-tree",
        ];
        let inside_worktree =
            host.git_output(&as_os_args(&inside_args), None).as_deref() == Some("true");
        if inside_worktree {
            let common_args = [
                "-C",
                repo_display.as_str(),
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            ];
            if let Some(common) = host.git_output(&as_os_args(&common_args), None) {
                let exclude = PathBuf::from(common).join("info").join("exclude");
                if let Some(parent) = exclude.parent() {
                    fs::create_dir_all(parent).map_err(|source| BootstrapError::Io {
                        path: parent.into(),
                        source,
                    })?;
                }
                let current = safe_read(&exclude)?.unwrap_or_default();
                if !current.lines().any(|line| line == "/.agent-run/") {
                    use std::io::Write;
                    let mut file = fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&exclude)
                        .map_err(|source| BootstrapError::Io {
                            path: exclude.clone(),
                            source,
                        })?;
                    file.write_all(b"\n/.agent-run/\n")
                        .map_err(|source| BootstrapError::Io {
                            path: exclude,
                            source,
                        })?;
                }
            }
        }
    }

    if mission_dir.exists() && !mission_dir.is_dir() {
        return Ok(usage_error(
            &format!(
                "mission path exists but is not a directory: {}",
                display_path(&mission_dir)
            ),
            quiet,
        ));
    }
    if !mission_dir.is_dir() {
        note(&format!(
            "Creating new mission directory: {}",
            display_path(&mission_dir)
        ));
        if !dry_run {
            fs::create_dir_all(&mission_dir).map_err(|source| BootstrapError::Io {
                path: mission_dir.clone(),
                source,
            })?;
        }
    }
    let mission = if mission_dir.is_dir() {
        let mission = fs::canonicalize(&mission_dir).map_err(|source| BootstrapError::Io {
            path: mission_dir.clone(),
            source,
        })?;
        let resolved_runs = fs::canonicalize(&runs_root).map_err(|source| BootstrapError::Io {
            path: runs_root.clone(),
            source,
        })?;
        if !path_is_within(&mission, &resolved_runs) {
            return Ok(usage_error(
                &format!(
                    "refusing: resolved mission path escaped the .agent-run/ boundary: {}",
                    display_path(&mission)
                ),
                quiet,
            ));
        }
        mission
    } else {
        mission_dir.clone()
    };
    let marker = mission.join(".mission-id");
    if marker.is_file() {
        let existing_id = safe_read(&marker)?.unwrap_or_default();
        if first_line(&existing_id) != Some(mission_id.as_str()) {
            return Ok(usage_error(
                &format!("mission identity mismatch: {}", display_path(&mission)),
                quiet,
            ));
        }
    } else if dry_run {
        note(&format!("  [dry-run] write {}", display_path(&marker)));
    } else {
        fs::write(&marker, format!("{mission_id}\n")).map_err(|source| BootstrapError::Io {
            path: marker.clone(),
            source,
        })?;
        note(&format!("  wrote {}", display_path(&marker)));
    }

    let rerun = mission.join("GOAL.md").is_file();
    note("");
    note("==============================================================");
    note("  autopilot bootstrap");
    note(&format!("  mission:    {}", display_path(&mission)));
    note(&format!("  skill:      {}", display_path(skill_root)));
    note(if rerun {
        "  phase:      B (substitute — GOAL.md present)"
    } else {
        "  phase:      A (scaffold — fresh mission)"
    });
    if dry_run {
        note("  mode:       DRY-RUN (no changes will be made)");
    }
    note("==============================================================");
    note("");
    note("[1/3] mission state files (template if present, else built-in default)");

    let templates = skill_root.join("templates");
    for filename in ["GOAL.md", "STATE.md", "QUEUE.md", "HANDOFF.md", "README.md"] {
        let template_name = filename.replace(".md", ".template.md");
        let source = templates.join(template_name);
        let destination = mission.join(filename);
        if source.is_file() {
            if destination.exists() && !force {
                warning(
                    &mut stderr,
                    &format!(
                        "exists, kept (not clobbered): {}",
                        display_path(&destination)
                    ),
                );
            } else if dry_run {
                note(&format!(
                    "  [dry-run] copy {} -> {}",
                    display_path(&source),
                    display_path(&destination)
                ));
            } else {
                if destination.exists() && force {
                    warning(
                        &mut stderr,
                        &format!(
                            "--force: overwriting existing file: {}",
                            display_path(&destination)
                        ),
                    );
                }
                host.copy2(&source, &destination)?;
                note(&format!(
                    "  copied {} -> {}",
                    filename.replace(".md", ".template.md"),
                    display_path(&destination)
                ));
            }
        } else if destination.exists() && !force {
            warning(
                &mut stderr,
                &format!(
                    "exists, kept (not clobbered): {}",
                    display_path(&destination)
                ),
            );
        } else if dry_run {
            note(&format!("  [dry-run] write {}", display_path(&destination)));
        } else {
            if destination.exists() && force {
                warning(
                    &mut stderr,
                    &format!(
                        "--force: overwriting existing file: {}",
                        display_path(&destination)
                    ),
                );
            }
            let content = fallback_content(filename, &domain)?;
            fs::write(&destination, content).map_err(|source| BootstrapError::Io {
                path: destination.clone(),
                source,
            })?;
            note(&format!("  wrote {}", display_path(&destination)));
        }
    }

    let goal_path = mission.join("GOAL.md");
    if !domain.is_empty() && goal_path.is_file() && !dry_run {
        let contents = fs::read_to_string(&goal_path)
            .map(universal_newlines)
            .map_err(|source| BootstrapError::Io {
                path: goal_path.clone(),
                source,
            })?;
        if let Some(seeded) = seed_domain(&contents, &domain) {
            fs::write(&goal_path, seeded).map_err(|source| BootstrapError::Io {
                path: goal_path.clone(),
                source,
            })?;
            note(&format!("  seeded DOMAIN knob in GOAL.md = {domain}"));
        }
    }

    note("");
    note("[2/3] substitute CONFIG KNOBS from GOAL.md");
    let mut knobs_filled = true;
    let goal = safe_read(&goal_path)?;
    let mut substitutions = Vec::<(String, String)>::new();
    if goal.is_none() && dry_run {
        note("  [dry-run] GOAL.md not on disk; skipping substitution preview.");
    } else if let Some(goal) = goal {
        let knobs = parse_knobs(&goal);
        if knobs.iter().next().is_none() {
            warning(
                &mut stderr,
                "no CONFIG KNOBS block found in GOAL.md (expected a ```config-knobs fence).",
            );
            knobs_filled = false;
        }
        for (key, value) in knobs.iter() {
            if !value.is_empty() && value != format!("{{{{{key}}}}}") && !value.contains("{{") {
                substitutions.push((key.into(), value.into()));
            }
        }
        let runaway = knobs.get("RUNAWAY_CAPS").unwrap_or("");
        if runaway.is_empty() || runaway == "{{RUNAWAY_CAPS}}" || runaway.contains("{{") {
            if let Some((_, value)) = substitutions
                .iter_mut()
                .find(|(key, _)| key == "RUNAWAY_CAPS")
            {
                *value = DEFAULT_RUNAWAY_CAPS.into();
            } else {
                substitutions.push(("RUNAWAY_CAPS".into(), DEFAULT_RUNAWAY_CAPS.into()));
            }
        }
        let unfilled: Vec<_> = REQUIRED_KNOBS
            .iter()
            .filter(|key| {
                knobs
                    .get(key)
                    .is_none_or(|value| value.is_empty() || value.contains("{{"))
            })
            .copied()
            .collect();
        if !unfilled.is_empty() {
            knobs_filled = false;
            warning(
                &mut stderr,
                &format!(
                    "required CONFIG KNOBS not yet filled in GOAL.md: {}",
                    unfilled.join(" ")
                ),
            );
        }
        if !substitutions.is_empty() {
            for target in SUBST_TARGETS {
                let target_path = mission.join(target);
                if !target_path.is_file() {
                    continue;
                }
                if dry_run {
                    note(&format!("  [dry-run] would substitute knobs in {target}"));
                    continue;
                }
                let mut content = fs::read_to_string(&target_path)
                    .map(universal_newlines)
                    .map_err(|source| BootstrapError::Io {
                        path: target_path.clone(),
                        source,
                    })?;
                for (key, value) in &substitutions {
                    content = content.replace(&format!("{{{{{key}}}}}"), value);
                }
                fs::write(&target_path, content).map_err(|source| BootstrapError::Io {
                    path: target_path,
                    source,
                })?;
                note(&format!("  substituted knobs in {target}"));
            }
        } else {
            note("  (no filled knobs to substitute yet)");
        }
    } else {
        warning(
            &mut stderr,
            "GOAL.md missing — cannot substitute. (Unexpected after scaffold.)",
        );
        knobs_filled = false;
    }

    note("");
    note("[3/3] scanning for unsubstituted {{...}} placeholders");
    let mut leftovers = BTreeSet::<(String, String)>::new();
    for target in SCAN_TARGETS {
        if let Some(content) = safe_read(&mission.join(target))? {
            for placeholder in placeholders(&content) {
                leftovers.insert(((*target).into(), placeholder));
            }
        }
    }
    note("");
    note("Environment checks (warnings only — a mission without these still works):");
    if host
        .git_output(&as_os_args(&["rev-parse", "--is-inside-work-tree"]), None)
        .as_deref()
        == Some("true")
    {
        note(
            "  ok   git repo detected (authorised worktree spikes use primary-root/.worktrees via scripts/worktree).",
        );
    } else {
        warning(
            &mut stderr,
            "no git repo detected: worktree isolation for build spikes is OFF.",
        );
    }
    let have_claude = host.has_program("claude");
    let have_codex = host.has_program("codex");
    if have_claude {
        note("  ok   claude CLI detected — reference operator (Workflow()/loop/Stop hook).");
    }
    if have_codex {
        note(
            "  ok   codex CLI detected — external driver + Ultra/native multi-agent (references/codex-operator.md).",
        );
    }
    if !have_claude && !have_codex {
        warning(
            &mut stderr,
            "no operator CLI detected (claude or codex). Manual/agent dispatch still works.",
        );
    }
    note("  info bounded waves + external model-family dispatch route through orchestrate.");
    note("");
    note("==============================================================");
    note("  RECEIPT");
    note("==============================================================");
    note(&format!("  mission root:    {}", display_path(&mission)));
    note("  state files:     GOAL STATE QUEUE HANDOFF README");
    note("  human entry:     README.md");
    note(&format!(
        "  knob source:     {}/GOAL.md  (the ONLY file you edit)",
        display_path(&mission)
    ));
    note("  delegated:       decision records -> implement/deliver RUN.json in this dir");
    note("                   bounded waves + model routing -> orchestrate");
    if dry_run {
        note("  mode:            DRY-RUN — nothing was changed.");
        note("==============================================================");
        return Ok(output(0, stdout, stderr));
    }
    if !knobs_filled {
        note("");
        note("  STATUS: SCAFFOLDED — CONFIG KNOBS not filled yet.");
        note("  NEXT:");
        note(&format!(
            "    1) Open  {}/GOAL.md  and fill the ```config-knobs block.",
            display_path(&mission)
        ));
        note(&format!("    2) Re-run:  {PROG} {mission_id}"));
        note("==============================================================");
        return Ok(output(3, stdout, stderr));
    }
    if !leftovers.is_empty() {
        stderr.push_str(&err(""));
        stderr.push_str(&err(&format!(
            "UNSUBSTITUTED placeholders remain ({}) — a knob was missed or mistyped:",
            leftovers.len()
        )));
        for (filename, placeholder) in leftovers {
            stderr.push_str(&err(&format!("    {placeholder}   in   {filename}")));
        }
        stderr.push_str(&err("Fix the CONFIG KNOBS block in GOAL.md, then re-run."));
        return Ok(output(4, stdout, stderr));
    }
    note("");
    note("  STATUS: READY — knobs substituted, no placeholders remain.");
    note(&format!(
        "  NEXT: read {}/README.md, launch one detected operator, steer via",
        display_path(&mission)
    ));
    note("  GOAL.md 'Active directives', stop via STATUS: STOP.");
    note("==============================================================");
    Ok(output(0, stdout, stderr))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    struct FixedHost;
    impl BootstrapHost for FixedHost {
        fn cwd(&mut self) -> Result<PathBuf, BootstrapError> {
            std::env::current_dir().map_err(|source| BootstrapError::Io {
                path: ".".into(),
                source,
            })
        }
        fn git_output(&mut self, _args: &[OsString], _cwd: Option<&Path>) -> Option<String> {
            None
        }
        fn utc_stamp(&mut self) -> Result<String, BootstrapError> {
            Ok("20261023-1219".into())
        }
        fn random_suffix(&mut self) -> Result<String, BootstrapError> {
            Ok("a1b2c3".into())
        }
        fn has_program(&mut self, _name: &str) -> bool {
            false
        }
        fn copy2(&mut self, source: &Path, destination: &Path) -> Result<(), BootstrapError> {
            fs::copy(source, destination)
                .map(|_| ())
                .map_err(|source| BootstrapError::Io {
                    path: destination.into(),
                    source,
                })
        }
    }

    fn isolated_fixture_root() -> tempfile::TempDir {
        for directory in ["/private/tmp", "/tmp"] {
            if let Ok(root) = tempfile::Builder::new()
                .prefix("workflow-bootstrap-")
                .tempdir_in(directory)
            {
                return root;
            }
        }
        panic!("no isolated temporary directory is available")
    }

    fn port_repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../")
            .canonicalize()
            .unwrap()
    }

    #[test]
    fn mission_identifier_policy_matches_source_diagnostics() {
        for (id, expected) in [
            ("", "a mission id is required (try --help)"),
            ("../escape", "mission id must not contain '/': ../escape"),
            (".hidden", "mission id must not start with '.': .hidden"),
            ("a..b", "mission id must not contain '..': a..b"),
            (
                "bad slug",
                "mission id must be a path-safe slug (letters, digits, '.', '_', '-' only): bad slug",
            ),
            (
                "领域",
                "mission id must be a path-safe slug (letters, digits, '.', '_', '-' only): 领域",
            ),
        ] {
            assert_eq!(validate_mission_id(id).unwrap_err().to_string(), expected);
        }
        for id in ["mission-id", "A_B", "abc.12"] {
            assert!(validate_mission_id(id).is_ok());
        }
        assert_eq!(mission_slug("A_B.c"), "a-b-c");
        assert_eq!(mission_slug(&"x".repeat(40)).len(), 32);
    }

    #[test]
    fn mission_scaffold_file_effects_match_source_python() {
        let temp = isolated_fixture_root();
        let temp_root = temp.path().canonicalize().unwrap();
        let rust_root = temp_root.join("rust");
        let python_root = temp_root.join("python");
        fs::create_dir_all(&rust_root).unwrap();
        fs::create_dir_all(&python_root).unwrap();
        assert!(!rust_root.join(".agent-run").exists());
        assert!(!python_root.join(".agent-run").exists());
        let skill_root = port_repo_root().join("skills/autopilot");
        let rust_args = vec![
            "--repo-root".into(),
            rust_root.to_string_lossy().into_owned(),
            "mission-id".into(),
            "Example domain".into(),
        ];
        let mut preview_args = rust_args.clone();
        preview_args.insert(0, "--dry-run".into());
        let preview = run_main(&preview_args, &skill_root, &mut FixedHost).unwrap();
        assert!(preview.stdout.starts_with(&format!(
            "Creating new mission directory: {}",
            rust_root
                .join(".agent-run/runs/20261023-1219-mission-mission-id-a1b2c3")
                .display()
        )));
        assert!(!rust_root.join(".agent-run").exists());
        let rust_result = run_main(&rust_args, &skill_root, &mut FixedHost).unwrap();
        assert_eq!(rust_result.exit_code, 3);
        assert!(rust_result.stdout.contains("STATUS: SCAFFOLDED"));
        let rust_mission =
            rust_root.join(".agent-run/runs/20261023-1219-mission-mission-id-a1b2c3");
        let mut rust_files = serde_json::Map::new();
        for name in [
            ".mission-id",
            "GOAL.md",
            "STATE.md",
            "QUEUE.md",
            "HANDOFF.md",
            "README.md",
        ] {
            let path = rust_mission.join(name);
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            rust_files.insert(
                name.into(),
                json!({"text":fs::read_to_string(path).unwrap(),"mode":mode}),
            );
        }

        let python_cases = json!([{
            "op":"main",
            "args":["--repo-root", python_root.to_string_lossy(), "mission-id", "Example domain"],
            "repo_root":python_root.to_string_lossy(),
        }]);
        let expected =
            crate::test_support::oracle_script("oracle_portable.py", "bootstrap", &python_cases)
                .expect("required Python differential");
        assert_eq!(expected[0]["exit"], 3);
        assert_eq!(expected[0]["files"], json!(rust_files));
        assert!(rust_result.stdout.starts_with(&format!(
            "Creating new mission directory: {}",
            rust_mission.display()
        )));
        assert_eq!(
            expected[0]["stdout"],
            rust_result
                .stdout
                .replace(&rust_mission.to_string_lossy().to_string(), "<MISSION>")
        );
        assert_eq!(expected[0]["stderr"], rust_result.stderr);
    }

    #[test]
    fn dry_run_and_rerun_effects_and_diagnostics_match_python() {
        let temp = isolated_fixture_root();
        let temp_root = temp.path().canonicalize().unwrap();
        let skill_root = port_repo_root().join("skills/autopilot");
        let rust_root = temp_root.join("rust");
        let python_root = temp_root.join("python");
        fs::create_dir_all(&rust_root).unwrap();
        fs::create_dir_all(&python_root).unwrap();
        assert!(!rust_root.join(".agent-run").exists());
        assert!(!python_root.join(".agent-run").exists());

        let dry_args = vec![
            "--dry-run".into(),
            "--repo-root".into(),
            rust_root.to_string_lossy().into_owned(),
            "dry-run".into(),
        ];
        let dry_result = run_main(&dry_args, &skill_root, &mut FixedHost).unwrap();
        assert!(dry_result
            .stdout
            .starts_with("Creating new mission directory: "));
        let dry_cases = json!([{
            "op":"main",
            "args":["--dry-run", "--repo-root", python_root.to_string_lossy(), "dry-run"],
            "repo_root":python_root.to_string_lossy(),
        }]);
        assert_eq!(dry_cases[0]["repo_root"].as_str(), python_root.to_str());
        let dry_expected =
            crate::test_support::oracle_script("oracle_portable.py", "bootstrap", &dry_cases)
                .expect("required Python differential");
        assert_eq!(dry_result.exit_code, dry_expected[0]["exit"]);
        assert_eq!(dry_expected[0]["files"], json!({}));
        assert!(!rust_root.join(".agent-run").exists());
        let dry_mission = rust_root.join(".agent-run/runs/20261023-1219-mission-dry-run-a1b2c3");
        assert_eq!(
            dry_expected[0]["stdout"],
            dry_result
                .stdout
                .replace(&dry_mission.to_string_lossy().to_string(), "<MISSION>")
        );

        let rust_mission = rust_root.join(".agent-run/runs/prefix-mission-rerun-id-a1b2c3");
        let python_mission = python_root.join(".agent-run/runs/prefix-mission-rerun-id-a1b2c3");
        let goal = "```config-knobs\nDOMAIN = Example\nMISSION = Mission title\nLOCKED_CONSTRAINTS = Stay scoped\nBUILD_CEILING = No build\nESCALATION_GATES = Ask first\nRUNAWAY_CAPS = {{RUNAWAY_CAPS}}\n```\n";
        for mission in [&rust_mission, &python_mission] {
            fs::create_dir_all(mission).unwrap();
            fs::write(mission.join(".mission-id"), "rerun-id\n").unwrap();
            fs::write(mission.join("GOAL.md"), goal).unwrap();
            fs::write(
                mission.join("README.md"),
                "{{MISSION}} / {{DOMAIN}} / {{RUNAWAY_CAPS}}\n",
            )
            .unwrap();
            fs::write(mission.join("STATE.md"), "{{LOCKED_CONSTRAINTS}}\n").unwrap();
            fs::write(mission.join("HANDOFF.md"), "{{ESCALATION_GATES}}\n").unwrap();
            fs::write(mission.join("QUEUE.md"), "leftover {{TYPO}}\n").unwrap();
        }
        let rerun_args = vec![
            "--repo-root".into(),
            rust_root.to_string_lossy().into_owned(),
            "rerun-id".into(),
        ];
        let rerun = run_main(&rerun_args, &skill_root, &mut FixedHost).unwrap();
        let rerun_expected = crate::test_support::oracle_script(
            "oracle_portable.py",
            "bootstrap",
            &json!([{
                "op":"main",
                "args":["--repo-root", python_root.to_string_lossy(), "rerun-id"],
                "repo_root":python_root.to_string_lossy(),
            }]),
        )
        .expect("required Python differential");
        let mut rust_files = serde_json::Map::new();
        for entry in fs::read_dir(&rust_mission).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                rust_files.insert(
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    json!({
                        "text":fs::read_to_string(&path).unwrap(),
                        "mode":fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    }),
                );
            }
        }
        assert_eq!(rerun.exit_code, 4);
        assert_eq!(rerun_expected[0]["exit"], 4);
        assert_eq!(rerun_expected[0]["files"], json!(rust_files));
        assert_eq!(
            rerun_expected[0]["stdout"],
            rerun
                .stdout
                .replace(&rust_mission.to_string_lossy().to_string(), "<MISSION>")
        );
        assert_eq!(
            rerun_expected[0]["stderr"],
            rerun
                .stderr
                .replace(&rust_mission.to_string_lossy().to_string(), "<MISSION>")
        );
    }

    #[test]
    fn argument_help_default_and_knob_differentials() {
        let mut cases = vec![json!({"op":"usage"})];
        let mut actual = vec![json!(usage())];
        for args in [
            vec![],
            vec!["mission", "domain"],
            vec!["--id", "mission", "domain"],
            vec!["--id", "mission", "--domain", "explicit", "extra"],
            vec!["--id", ""],
            vec!["mission", "--", "ignored"],
            vec!["--repo-root"],
            vec!["--id", "--force"],
            vec!["--id", "a", "--id", "b", "-q", "--dry-run", "--force"],
            vec!["--help"],
            vec!["--unknown"],
            vec!["a", "b", "c"],
            vec!["--domain", "domain", "mission"],
            vec!["--domain", "domain", "a", "b"],
            vec!["--", "ignored"],
            vec!["--id", "a", "--domain", "", "b"],
        ] {
            cases.push(json!({"op":"args","args":args}));
            actual.push(
                match parse_args(&args.iter().map(|s| (*s).into()).collect::<Vec<_>>()) {
                    Ok(ParsedArgs::Values(values)) => json!({"values":values,"rest":[]}),
                    Ok(ParsedArgs::Help) => json!({"exit":0,"stdout":usage(),"stderr":""}),
                    Err(e) => {
                        json!({"exit":e.exit_code(),"stdout":"","stderr":err(&e.to_string())})
                    }
                },
            );
        }
        for name in ["GOAL.md", "STATE.md", "QUEUE.md", "HANDOFF.md", "README.md"] {
            for domain in ["", "Example & tools | scripts\\paths", "领域 {{MISSION}}"] {
                cases.push(json!({"op":"fallback","name":name,"domain":domain}));
                actual.push(json!(fallback_content(name, domain).unwrap()));
            }
        }
        for goal in [
            "",
            "outside = ignore\n```config-knobs\n A = one=two\n#comment=ignored\nA = final\n = empty\n```\na=outside",
            " ```config-knobs\nA=no\n```",
            "```config-knobs \r\nA = yes\rB = also\u{85}C = line\n```",
            "```config-knobs\nA=1\n```other\nB=2\n```\nC=no\n```config-knobs\nD=4",
        ] {
            cases.push(json!({"op":"knobs","goal":goal}));
            actual.push(json!(parse_knobs(goal)));
        }
        if let Some(expected) =
            crate::test_support::oracle_script("oracle_portable.py", "bootstrap", &json!(cases))
        {
            assert_eq!(json!(actual), expected);
        }
    }
    #[test]
    fn file_helpers_replace_cleanup_and_preserve_read_errors() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        assert_eq!(safe_read(&path).unwrap(), None);
        assert!(atomic_write(&path, "first").unwrap());
        assert!(atomic_write(&path, "new\r\nline\rother").unwrap());
        assert_eq!(safe_read(&path).unwrap().unwrap(), "new\nline\nother");
        fs::write(&path, [0xff]).unwrap();
        assert!(matches!(
            safe_read(&path),
            Err(BootstrapError::Encoding { .. })
        ));
        assert!(!atomic_write(&root.path().join("missing/state"), "x").unwrap());
        let directory = root.path().join("directory");
        fs::create_dir(&directory).unwrap();
        assert!(!atomic_write(&directory, "x").unwrap());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
        assert_eq!(warn("check", true), warn("check", false));
        assert_eq!(err(""), "bootstrap-autopilot.py:\n");
        assert!(matches!(
            atomic_write(Path::new("/"), "refuse"),
            Err(BootstrapError::EmptyName { .. })
        ));
        let knobs = parse_knobs("```config-knobs\nZ=first\nA=next\nZ=last\n```");
        assert_eq!(
            knobs.iter().collect::<Vec<_>>(),
            vec![("Z", "last"), ("A", "next")]
        );
        assert_eq!(knobs.get("Z"), Some("last"));
        assert_eq!(
            serde_json::to_string(&knobs).unwrap(),
            "{\"Z\":\"last\",\"A\":\"next\"}"
        );
        assert_eq!(
            serde_json::from_str::<ConfigKnobs>("{\"Z\":\"last\",\"A\":\"next\"}").unwrap(),
            knobs
        );
    }
    #[test]
    fn git_queries_report_failure_without_mutating_repository() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            git_output(
                &["workflow-port-no-such-git-verb".into()],
                Some(root.path())
            )
            .unwrap(),
            None
        );
        let result = git_output(&["--version".into()], None).unwrap();
        if let Some(result) = result {
            assert!(result.starts_with("git version "));
        }
    }
}
