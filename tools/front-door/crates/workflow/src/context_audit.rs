//! Read-only audit of repository context, logs, handoffs and run scaffolds.

use provenant_records::checked_json;
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

const SKIP_PARTS: &[&str] = &[
    ".git",
    ".worktrees",
    "node_modules",
    ".venv",
    "venv",
    ".backups",
    "archive",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Copy)]
pub struct FileMetadata {
    pub kind: ObjectKind,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

/// Filesystem operations vary at this policy boundary and can be replaced by
/// a fixture without patching global `Path` methods.
pub trait AuditFileSystem {
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>>;
    fn symlink_metadata(&self, path: &Path) -> io::Result<FileMetadata>;
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct OsAuditFileSystem;

impl AuditFileSystem for OsAuditFileSystem {
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect()
    }

    fn symlink_metadata(&self, path: &Path) -> io::Result<FileMetadata> {
        let metadata = fs::symlink_metadata(path)?;
        let kind = if metadata.file_type().is_symlink() {
            ObjectKind::Symlink
        } else if metadata.is_file() {
            ObjectKind::File
        } else if metadata.is_dir() {
            ObjectKind::Directory
        } else {
            ObjectKind::Other
        };
        Ok(FileMetadata {
            kind,
            size: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        fs::canonicalize(path)
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }
}

#[derive(Debug, Error)]
pub enum AuditError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid {field} in {path}: {detail}")]
    InvalidRunField {
        path: PathBuf,
        field: &'static str,
        detail: String,
    },
    #[error("invalid UTF-8 in {path}")]
    InvalidUtf8 { path: PathBuf },
    #[error(transparent)]
    NumberOutOfRange(#[from] checked_json::NumberOutOfRange),
    #[error("cannot compare a naive expiry with an aware current time in {path}")]
    NaiveExpiryComparison { path: PathBuf },
    #[error("cannot render context audit JSON: {0}")]
    ReportSerialization(#[from] serde_json::Error),
    #[error("timestamp is outside Python's supported datetime range")]
    TimestampOutOfRange,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct AuditOptions {
    pub max_markdown_kb: i64,
    pub max_log_mb: i64,
    pub max_state_lines: i64,
    pub stale_handoff_days: i64,
    pub stale_log_days: i64,
}

impl Default for AuditOptions {
    fn default() -> Self {
        Self {
            max_markdown_kb: 15,
            max_log_mb: 5,
            max_state_lines: 120,
            stale_handoff_days: 30,
            stale_log_days: 30,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Finding {
    pub severity: String,
    pub code: String,
    pub path: String,
    pub detail: String,
}

struct Patterns {
    scratch_names: Regex,
    freshness: Regex,
    handoff_field: Regex,
    canonical_key: Regex,
    new_run: Regex,
}

impl Patterns {
    fn new() -> Result<Self, AuditError> {
        let invalid = |pattern: &'static str| AuditError::InvalidRunField {
            path: PathBuf::new(),
            field: "built-in regular expression",
            detail: pattern.to_owned(),
        };
        Ok(Self {
            scratch_names: Regex::new(
                r"(?i)^(?:\.temp|temp[-_.]|scratch[-_.]|.*\.(?:tmp|scratch|bak|orig)|.*\.bak[-_.].*)",
            )
            .map_err(|_| invalid("scratch names"))?,
            freshness: Regex::new(
                r"(?im)^(?:updated|last updated|last verified|as of)\s*:\s*(\S+)",
            )
            .map_err(|_| invalid("freshness"))?,
            handoff_field: Regex::new(
                r"(?im)^(?:-\s*)?(?:\*\*)?(Status|Effort|Leg|Supersedes|Consumed-at)(?:\*\*)?\s*:\s*(.+?)\s*$",
            )
            .map_err(|_| invalid("handoff fields"))?,
            canonical_key: Regex::new(
                r"(?im)^(?:-\s*)?(?:\*\*)?Canonical key(?:\*\*)?\s*:\s*([A-Za-z0-9._-]+)\s*$",
            )
            .map_err(|_| invalid("canonical key"))?,
            new_run: Regex::new(
                r"^\d{8}-\d{4}-(dispatch|batch|orch|delivery|mission|review|wf)-",
            )
            .map_err(|_| invalid("run directory"))?,
        })
    }
}

fn normalise_absolute(path: &Path, cwd: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let mut result = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = result.pop();
            }
            Component::Prefix(prefix) => result.push(prefix.as_os_str()),
            Component::RootDir | Component::Normal(_) => result.push(component.as_os_str()),
        }
    }
    result
}

fn below(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root).is_ok()
}

pub fn contained(path: &Path, root: &Path, kind: ObjectKind, fs: &impl AuditFileSystem) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        if fs
            .symlink_metadata(&current)
            .is_ok_and(|metadata| metadata.kind == ObjectKind::Symlink)
        {
            return false;
        }
    }
    let Ok(resolved) = fs.canonicalize(&current) else {
        return false;
    };
    if !below(&resolved, root) {
        return false;
    }
    let Ok(observed) = fs.symlink_metadata(&current) else {
        return false;
    };
    fs.canonicalize(&current)
        .is_ok_and(|again| again == resolved)
        && observed.kind == kind
}

fn skipped(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root).is_ok_and(|relative| {
        relative
            .components()
            .any(|part| SKIP_PARTS.contains(&part.as_os_str().to_string_lossy().as_ref()))
    })
}

fn walk_files_at(
    current: &Path,
    root: &Path,
    fs: &impl AuditFileSystem,
    output: &mut Vec<PathBuf>,
) {
    let Ok(entries) = fs.read_dir(current) else {
        return;
    };
    let mut dirs = Vec::new();
    for path in entries {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if SKIP_PARTS.contains(&name.as_ref()) {
            continue;
        }
        if contained(&path, root, ObjectKind::Directory, fs) {
            dirs.push(path);
        } else if contained(&path, root, ObjectKind::File, fs) {
            output.push(path);
        }
    }
    for directory in dirs {
        walk_files_at(&directory, root, fs, output);
    }
}

pub fn walked_files(root: &Path, fs: &impl AuditFileSystem) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk_files_at(root, root, fs, &mut files);
    files
}

fn walk_dirs_at(
    current: &Path,
    root: &Path,
    name: &str,
    fs: &impl AuditFileSystem,
    output: &mut Vec<PathBuf>,
) {
    let Ok(entries) = fs.read_dir(current) else {
        return;
    };
    let dirs: Vec<_> = entries
        .into_iter()
        .filter(|path| {
            let part = path.file_name().unwrap_or_default().to_string_lossy();
            !SKIP_PARTS.contains(&part.as_ref()) && contained(path, root, ObjectKind::Directory, fs)
        })
        .collect();
    for directory in &dirs {
        if directory.file_name().is_some_and(|part| part == name) {
            output.push(directory.clone());
        }
    }
    for directory in dirs {
        walk_dirs_at(&directory, root, name, fs, output);
    }
}

pub fn walked_dirs(root: &Path, name: &str, fs: &impl AuditFileSystem) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    walk_dirs_at(root, root, name, fs, &mut dirs);
    dirs
}

fn decode_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

fn has_freshness(text: &str, patterns: &Patterns) -> bool {
    let Some(value) = patterns
        .freshness
        .captures(text)
        .and_then(|captures| captures.get(1))
        .map(|value| value.as_str())
    else {
        return false;
    };
    if value.starts_with('<') {
        return false;
    }
    let normalized = value.replace('Z', "+00:00");
    provenant_core::py::python_fromisoformat(&normalized)
        .is_ok_and(|parsed| parsed.offset_microseconds.is_some())
}

fn is_hot_markdown(path: &Path, root: &Path) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    path.file_name().is_some_and(|name| {
        matches!(
            name.to_string_lossy().as_ref(),
            "SKILL.md" | "AGENTS.md" | "CLAUDE.md" | "STATE.md"
        )
    }) || relative.components().any(|part| {
        matches!(
            part.as_os_str().to_string_lossy().as_ref(),
            "docs" | "handoffs"
        )
    })
}

fn python_timestamp_micros(time: SystemTime) -> Result<i64, AuditError> {
    let seconds = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs_f64(),
        Err(error) => -error.duration().as_secs_f64(),
    };
    provenant_core::py::from_timestamp(seconds)
        .map(|timestamp| timestamp.as_microsecond())
        .map_err(|_| AuditError::TimestampOutOfRange)
}

fn age_days(now: SystemTime, modified: SystemTime) -> Result<i64, AuditError> {
    let delta =
        i128::from(python_timestamp_micros(now)?) - i128::from(python_timestamp_micros(modified)?);
    Ok(delta.div_euclid(86_400_000_000) as i64)
}

fn finding(severity: &str, code: &str, path: String, detail: String) -> Finding {
    Finding {
        severity: severity.to_owned(),
        code: code.to_owned(),
        path,
        detail,
    }
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn read_text(path: &Path, fs: &impl AuditFileSystem) -> io::Result<String> {
    fs.read(path).map(|bytes| decode_text(&bytes))
}

fn error_io(path: &Path, source: io::Error) -> AuditError {
    AuditError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn check_run_dirs(
    root: &Path,
    now: SystemTime,
    patterns: &Patterns,
    fs: &impl AuditFileSystem,
    findings: &mut Vec<Finding>,
) -> Result<(), AuditError> {
    let mut run_dirs = BTreeSet::<PathBuf>::new();
    for run_root in walked_dirs(root, ".agent-run", fs) {
        if contained(&run_root, root, ObjectKind::Directory, fs) && !skipped(&run_root, root) {
            for path in fs
                .read_dir(&run_root)
                .map_err(|error| error_io(&run_root, error))?
            {
                if contained(&path, root, ObjectKind::Directory, fs)
                    && path.file_name().is_some_and(|name| name == "runs")
                {
                    for child in fs.read_dir(&path).map_err(|error| error_io(&path, error))? {
                        if contained(&child, root, ObjectKind::Directory, fs) {
                            run_dirs.insert(child);
                        }
                    }
                }
            }
        }
    }
    for work_root in walked_dirs(root, ".work", fs) {
        let wf_root = work_root.join("wf");
        if !contained(&wf_root, root, ObjectKind::Directory, fs) || skipped(&work_root, root) {
            continue;
        }
        for workflow_dir in fs
            .read_dir(&wf_root)
            .map_err(|error| error_io(&wf_root, error))?
        {
            if contained(&workflow_dir, root, ObjectKind::Directory, fs) {
                for path in fs
                    .read_dir(&workflow_dir)
                    .map_err(|error| error_io(&workflow_dir, error))?
                {
                    if contained(&path, root, ObjectKind::Directory, fs) {
                        run_dirs.insert(path);
                    }
                }
            }
        }
    }
    for run_dir in run_dirs {
        if !contained(&run_dir, root, ObjectKind::Directory, fs) {
            continue;
        }
        let rel = relative(root, &run_dir);
        let mut required = vec![
            "MANIFEST.md",
            "RUN_RECEIPT.json",
            "SYNTHESIS.md",
            "FINAL_GATE.md",
        ];
        let mut new_kind = None;
        if run_dir
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "runs")
        {
            new_kind = patterns
                .new_run
                .captures(&run_dir.file_name().unwrap_or_default().to_string_lossy())
                .and_then(|captures| captures.get(1))
                .map(|value| value.as_str().to_owned());
        }
        match new_kind.as_deref() {
            Some("dispatch" | "batch") => required = vec!["RUN_RECEIPT.json"],
            Some("delivery") => required = vec!["RUN.json"],
            Some("mission") => required = vec!["GOAL.md"],
            _ => {}
        }
        if run_dir
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "implement")
        {
            required.push("RUN.json");
        }
        let missing: Vec<_> = required
            .iter()
            .copied()
            .filter(|name| !contained(&run_dir.join(name), root, ObjectKind::File, fs))
            .collect();
        if !missing.is_empty() {
            findings.push(finding(
                "error",
                "incomplete-run-index",
                rel.clone(),
                format!("missing {}", missing.join(", ")),
            ));
        }
        let run_path = run_dir.join("RUN.json");
        if !contained(&run_path, root, ObjectKind::File, fs) {
            continue;
        }
        let raw_run = match fs.read(&run_path) {
            Ok(raw) => raw,
            Err(_) => continue,
        };
        let run_text = String::from_utf8(raw_run).map_err(|_| AuditError::InvalidUtf8 {
            path: run_path.clone(),
        })?;
        let run: Value = match checked_json::from_str(&run_text) {
            Ok(value) => value,
            Err(checked_json::DecodeError::Json(_)) => Value::Null,
            Err(checked_json::DecodeError::NumberOutOfRange(error)) => return Err(error.into()),
        };
        if run.as_object().is_none()
            || !provenant_core::py::python_json_eq(
                run.get("schema_version").unwrap_or(&Value::Null),
                &Value::from(1),
            )
            || run.get("contract").and_then(Value::as_str) != Some("delivery-run")
        {
            continue;
        }
        let artifacts = run
            .get("artifacts")
            .cloned()
            .unwrap_or_else(|| Value::Array(vec![]));
        let artifacts: Vec<Value> = match artifacts {
            Value::Array(items) => items,
            Value::Object(items) => items
                .into_iter()
                .map(|(key, _)| Value::String(key))
                .collect(),
            Value::String(items) => items
                .chars()
                .map(|ch| Value::String(ch.to_string()))
                .collect(),
            Value::Null => {
                return Err(AuditError::InvalidRunField {
                    path: run_path.clone(),
                    field: "artifacts",
                    detail: "expected an iterable".to_owned(),
                });
            }
            Value::Bool(_) | Value::Number(_) => {
                return Err(AuditError::InvalidRunField {
                    path: run_path.clone(),
                    field: "artifacts",
                    detail: "expected an iterable".to_owned(),
                });
            }
        };
        let mut declared = BTreeSet::new();
        for (index, artifact) in artifacts.iter().enumerate() {
            let Some(artifact) = artifact.as_object() else {
                continue;
            };
            let Some(path) = artifact.get("path").and_then(Value::as_str) else {
                continue;
            };
            declared.insert(path.to_owned());
            if artifact.get("class").and_then(Value::as_str) != Some("scratch")
                || !artifact
                    .get("expires_at")
                    .is_some_and(provenant_core::py::python_truth)
            {
                continue;
            }
            let expiry_text =
                provenant_core::py::python_str(&artifact["expires_at"]).replace('Z', "+00:00");
            let expiry = match provenant_core::py::python_fromisoformat(&expiry_text) {
                Ok(expiry) if expiry.offset_microseconds.is_some() => expiry
                    .timestamp()
                    .map_err(|_| AuditError::InvalidRunField {
                        path: run_path.clone(),
                        field: "expires_at",
                        detail: "timestamp is out of range".to_owned(),
                    })?
                    .as_microsecond(),
                Ok(_) => {
                    return Err(AuditError::NaiveExpiryComparison {
                        path: run_path.clone(),
                    });
                }
                _ => {
                    findings.push(finding(
                        "error",
                        "invalid-scratch-expiry",
                        format!("{rel}/RUN.json"),
                        format!("artifact {index} expiry is invalid"),
                    ));
                    continue;
                }
            };
            let now_micros = python_timestamp_micros(now)?;
            if expiry <= now_micros {
                findings.push(finding(
                    "warning",
                    "expired-run-scratch",
                    format!("{rel}/{path}"),
                    "manifest-owned scratch is expired; removal still requires cleanup authority"
                        .to_owned(),
                ));
            }
        }
        for candidate in walked_files(&run_dir, fs) {
            if candidate == run_path {
                continue;
            }
            let candidate_name = candidate.file_name().unwrap_or_default().to_string_lossy();
            if patterns.scratch_names.is_match(&candidate_name) {
                let candidate_rel = relative(&run_dir, &candidate);
                if !declared.contains(&candidate_rel) {
                    findings.push(finding(
                        "warning",
                        "orphan-run-scratch",
                        format!("{rel}/{candidate_rel}"),
                        "scratch-like file is absent from the delivery artifact manifest"
                            .to_owned(),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn check_handoffs(
    root: &Path,
    patterns: &Patterns,
    fs: &impl AuditFileSystem,
    findings: &mut Vec<Finding>,
) {
    let mut active_handoffs = BTreeMap::<(String, String), String>::new();
    for path in walked_files(root, fs).into_iter().filter(|path| {
        path.file_name().is_some_and(|name| {
            name.to_string_lossy().starts_with("HANDOFF-")
                && name.to_string_lossy().ends_with(".md")
        })
    }) {
        if skipped(&path, root) {
            continue;
        }
        let rel_path = path.strip_prefix(root).unwrap_or(&path);
        let rel = relative(root, &path);
        let text = match read_text(&path, fs) {
            Ok(text) => text.chars().take(4096).collect::<String>(),
            Err(_) => continue,
        };
        let mut fields = BTreeMap::new();
        for captures in patterns.handoff_field.captures_iter(&text) {
            if let (Some(key), Some(value)) = (captures.get(1), captures.get(2)) {
                fields.insert(
                    key.as_str().to_lowercase(),
                    value.as_str().trim().to_lowercase(),
                );
            }
        }
        let required = ["status", "effort", "leg", "supersedes", "consumed-at"];
        let missing: Vec<_> = required
            .iter()
            .filter(|key| !fields.contains_key(**key))
            .copied()
            .collect();
        if !missing.is_empty() {
            let mut missing = missing;
            missing.sort_unstable();
            findings.push(finding(
                "error",
                "handoff-metadata-missing",
                rel.clone(),
                format!("missing {}", missing.join(", ")),
            ));
        }
        let status = fields.get("status").map(String::as_str);
        if !matches!(status, Some("active" | "consumed")) {
            continue;
        }
        let archived = rel_path
            .components()
            .any(|part| part.as_os_str() == "archive");
        if status == Some("consumed") && !archived {
            findings.push(finding(
                "error",
                "consumed-handoff-live",
                rel.clone(),
                "consumed handoff must move to the archive".to_owned(),
            ));
        }
        if status == Some("consumed")
            && fields
                .get("consumed-at")
                .is_none_or(|value| matches!(value.as_str(), "" | "pending" | "none"))
        {
            findings.push(finding(
                "error",
                "consumed-handoff-date-missing",
                rel.clone(),
                "consumed handoff must record Consumed-at".to_owned(),
            ));
        }
        if status == Some("active") && archived {
            findings.push(finding(
                "error",
                "active-handoff-archived",
                rel.clone(),
                "active handoff must stay in the live handoff directory".to_owned(),
            ));
        }
        let effort = fields.get("effort").map_or("none", String::as_str);
        let leg = fields.get("leg").map_or("none", String::as_str);
        if status == Some("active") && !archived && (effort, leg) != ("none", "none") {
            let key = (effort.to_owned(), leg.to_owned());
            if let Some(previous) = active_handoffs.get(&key) {
                findings.push(finding(
                    "error",
                    "duplicate-active-handoff",
                    rel,
                    format!("same effort/leg as {previous}"),
                ));
            } else {
                active_handoffs.insert(key, rel);
            }
        }
    }
}

/// Audit a root at an explicit clock instant. Findings are sorted by the
/// source's `(severity, code, path)` key and the filesystem is never modified.
pub fn audit(
    root: &Path,
    options: AuditOptions,
    now: SystemTime,
    fs: &impl AuditFileSystem,
) -> Result<Vec<Finding>, AuditError> {
    let process_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let lexical_root = normalise_absolute(root, &process_cwd);
    let root = fs.canonicalize(&lexical_root).unwrap_or(lexical_root);
    let patterns = Patterns::new()?;
    let mut findings = Vec::new();
    let mut canonical_keys = BTreeMap::<String, String>::new();
    for path in walked_files(&root, fs) {
        if !path.is_file() || skipped(&path, &root) {
            continue;
        }
        let rel = relative(&root, &path);
        let metadata = match fs.symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        let suffix = path
            .extension()
            .map(|extension| extension.to_string_lossy().to_lowercase());
        if suffix.as_deref() == Some("md")
            && is_hot_markdown(&path, &root)
            && i128::from(metadata.size) > i128::from(options.max_markdown_kb) * 1024
        {
            findings.push(finding(
                "warning",
                "large-agent-doc",
                rel.clone(),
                format!(
                    "{} bytes exceeds {} KiB split/merge signal",
                    metadata.size, options.max_markdown_kb
                ),
            ));
        }
        if matches!(suffix.as_deref(), Some("log" | "jsonl"))
            && i128::from(metadata.size) > i128::from(options.max_log_mb) * 1024 * 1024
        {
            findings.push(finding(
                "warning",
                "large-raw-log",
                rel.clone(),
                format!(
                    "{} bytes exceeds {} MiB rotation/retention signal",
                    metadata.size, options.max_log_mb
                ),
            ));
        }
        if matches!(suffix.as_deref(), Some("log" | "jsonl")) {
            if let Some(modified) = metadata.modified {
                let age = age_days(now, modified)?;
                if age > options.stale_log_days {
                    findings.push(finding(
                        "warning",
                        "stale-raw-log",
                        rel.clone(),
                        format!("{age} days old; apply the owning retention policy"),
                    ));
                }
            }
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if path.parent() == Some(root.as_path()) && patterns.scratch_names.is_match(&name) {
            findings.push(finding(
                "warning",
                "root-scratch",
                rel.clone(),
                "unscoped repo-root scratch candidate; inspect ownership before removal".to_owned(),
            ));
        }
        if name == "STATE.md" {
            let text = match read_text(&path, fs) {
                Ok(text) => text,
                Err(_) => continue,
            };
            let lines = crate::pending::splitlines(&text);
            if i128::try_from(lines.len()).unwrap_or(i128::MAX)
                > i128::from(options.max_state_lines)
            {
                findings.push(finding(
                    "warning",
                    "state-over-cap",
                    rel.clone(),
                    format!("{} lines exceeds {}", lines.len(), options.max_state_lines),
                ));
            }
            if !has_freshness(
                &lines
                    .iter()
                    .take(12)
                    .copied()
                    .collect::<Vec<_>>()
                    .join("\n"),
                &patterns,
            ) {
                findings.push(finding(
                    "warning",
                    "state-freshness-missing",
                    rel.clone(),
                    "no anchored Updated/Last verified/As of field in first 12 lines".to_owned(),
                ));
            }
        }
        if suffix.as_deref() == Some("md")
            && path
                .strip_prefix(&root)
                .is_ok_and(|relative| relative.components().any(|part| part.as_os_str() == "docs"))
        {
            if let Ok(text) = read_text(&path, fs) {
                if let Some(key) = patterns
                    .canonical_key
                    .captures(&text.chars().take(4096).collect::<String>())
                    .and_then(|captures| captures.get(1))
                    .map(|value| value.as_str().to_lowercase())
                {
                    if let Some(previous) = canonical_keys.get(&key) {
                        findings.push(finding(
                            "error",
                            "duplicate-canonical-key",
                            rel.clone(),
                            format!("same canonical key as {previous}"),
                        ));
                    } else {
                        canonical_keys.insert(key, rel.clone());
                    }
                }
            }
        }
        if path.components().any(|part| part.as_os_str() == "handoffs") {
            if let Some(modified) = metadata.modified {
                let age = age_days(now, modified)?;
                if age > options.stale_handoff_days {
                    findings.push(finding(
                        "warning",
                        "stale-live-handoff",
                        rel,
                        format!("{age} days old; archive if consumed"),
                    ));
                }
            }
        }
    }
    check_run_dirs(&root, now, &patterns, fs, &mut findings)?;
    check_handoffs(&root, &patterns, fs, &mut findings);
    findings.sort_by(|left, right| {
        (&left.severity, &left.code, &left.path).cmp(&(&right.severity, &right.code, &right.path))
    });
    Ok(findings)
}

pub fn audit_os(
    root: &Path,
    options: AuditOptions,
    now: SystemTime,
) -> Result<Vec<Finding>, AuditError> {
    audit(root, options, now, &OsAuditFileSystem)
}

pub fn has_errors(findings: &[Finding]) -> bool {
    findings.iter().any(|finding| finding.severity == "error")
}

pub fn exit_code(findings: &[Finding], warnings_as_errors: bool) -> i32 {
    i32::from(has_errors(findings) || (warnings_as_errors && !findings.is_empty()))
}

pub fn text_report(findings: &[Finding]) -> String {
    if findings.is_empty() {
        return "clean: no context-hygiene signals".to_owned();
    }
    findings
        .iter()
        .map(|item| {
            format!(
                "{}: {}: {}: {}",
                item.severity, item.code, item.path, item.detail
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditCliOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Serialize)]
struct JsonAuditReport<'a> {
    root: String,
    findings: &'a [Finding],
}

fn ensure_ascii_json(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_ascii() {
            escaped.push(character);
            continue;
        }
        let codepoint = u32::from(character);
        if codepoint <= 0xffff {
            escaped.push_str(&format!("\\u{codepoint:04x}"));
        } else {
            let shifted = codepoint - 0x1_0000;
            let high = 0xd800 + (shifted >> 10);
            let low = 0xdc00 + (shifted & 0x3ff);
            escaped.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
        }
    }
    escaped
}

/// Render the source CLI's report and status policy without binding the
/// library to argv or process-global stdout.
pub fn cli_output(
    root_arg: &Path,
    options: AuditOptions,
    json_output: bool,
    _strict: bool,
    warnings_as_errors: bool,
    now: SystemTime,
    fs: &impl AuditFileSystem,
) -> Result<AuditCliOutput, AuditError> {
    let root_text = root_arg.to_string_lossy().into_owned();
    let root_resolved = match fs.canonicalize(root_arg) {
        Ok(root)
            if fs
                .symlink_metadata(&root)
                .is_ok_and(|metadata| metadata.kind == ObjectKind::Directory) =>
        {
            root
        }
        _ => {
            return Ok(AuditCliOutput {
                exit_code: 2,
                stdout: String::new(),
                stderr: format!("context audit root is not a directory: {root_text}\n"),
            });
        }
    };
    let findings = audit(&root_resolved, options, now, fs)?;
    let stdout = if json_output {
        let report = JsonAuditReport {
            root: root_resolved.to_string_lossy().into_owned(),
            findings: &findings,
        };
        format!(
            "{}\n",
            ensure_ascii_json(&serde_json::to_string_pretty(&report)?)
        )
    } else if findings.is_empty() {
        "clean: no context-hygiene signals\n".to_owned()
    } else {
        format!("{}\n", text_report(&findings))
    };
    Ok(AuditCliOutput {
        exit_code: exit_code(&findings, warnings_as_errors),
        stdout,
        stderr: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn oracle(cases: Value) -> Option<Value> {
        crate::test_support::oracle_script("oracle_session.py", "context_audit", &cases)
    }

    #[test]
    fn reports_context_growth_and_cli_severity() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let docs = root.join("docs");
        fs::create_dir_all(docs.join("handoffs")).unwrap();
        fs::write(
            docs.join("STATE.md"),
            format!("# State\n{}", "x\n".repeat(121)),
        )
        .unwrap();
        fs::write(docs.join("large.md"), "x".repeat(16_000)).unwrap();
        fs::write(root.join(".temp-review.txt"), "keep me").unwrap();
        fs::write(root.join("SKILL.md.bak-2026-07-10"), "backup").unwrap();
        let handoff = docs.join("handoffs/HANDOFF-old.md");
        fs::write(
            &handoff,
            "Status: active\nEffort: E1\nLeg: L2\nSupersedes: none\nConsumed-at: pending\n",
        )
        .unwrap();
        let now = UNIX_EPOCH + std::time::Duration::from_secs(1_900_000_000);
        let findings = audit_os(&root, AuditOptions::default(), now).unwrap();
        let codes: BTreeSet<_> = findings
            .iter()
            .map(|finding| finding.code.as_str())
            .collect();
        assert!(codes.contains("large-agent-doc"));
        assert!(codes.contains("root-scratch"));
        assert!(codes.contains("state-over-cap"));
        assert!(codes.contains("state-freshness-missing"));
        assert!(codes.contains("stale-live-handoff"));
        assert_eq!(exit_code(&findings, false), 0);
        assert_eq!(exit_code(&findings, true), 1);
        assert_eq!(
            fs::read_to_string(root.join(".temp-review.txt")).unwrap(),
            "keep me"
        );
    }

    #[test]
    fn cli_output_keeps_advisory_and_structural_exit_policies() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::write(root.join("scratch.tmp"), "keep").unwrap();
        let now = UNIX_EPOCH + std::time::Duration::from_secs(1_900_000_000);
        let advisory = cli_output(
            &root,
            AuditOptions::default(),
            false,
            false,
            false,
            now,
            &OsAuditFileSystem,
        )
        .unwrap();
        assert_eq!(advisory.exit_code, 0);
        assert!(advisory.stdout.contains("root-scratch"));
        if let Some(expected) = oracle(json!([{
            "op":"cli",
            "argv":[root.to_string_lossy().to_string(),"--json"]
        }])) {
            let actual = cli_output(
                &root,
                AuditOptions::default(),
                true,
                false,
                false,
                now,
                &OsAuditFileSystem,
            )
            .unwrap();
            assert_eq!(
                json!({"exit_code":actual.exit_code,"stdout":actual.stdout,"stderr":actual.stderr}),
                expected[0]
            );
        }
        let strict = cli_output(
            &root,
            AuditOptions::default(),
            false,
            true,
            false,
            now,
            &OsAuditFileSystem,
        )
        .unwrap();
        assert_eq!(strict.exit_code, 0);
        let warnings = cli_output(
            &root,
            AuditOptions::default(),
            false,
            false,
            true,
            now,
            &OsAuditFileSystem,
        )
        .unwrap();
        assert_eq!(warnings.exit_code, 1);
        fs::create_dir_all(root.join(".agent-run/runs/20260923-1000-dispatch-broken-a1b2c3"))
            .unwrap();
        let structural = cli_output(
            &root,
            AuditOptions::default(),
            true,
            false,
            false,
            now,
            &OsAuditFileSystem,
        )
        .unwrap();
        assert_eq!(structural.exit_code, 1);
        assert!(structural.stdout.starts_with("{\n  \"root\""));
        let missing = cli_output(
            &root.join("missing"),
            AuditOptions::default(),
            false,
            false,
            false,
            now,
            &OsAuditFileSystem,
        )
        .unwrap();
        assert_eq!(missing.exit_code, 2);
        assert!(missing
            .stderr
            .contains("context audit root is not a directory"));
    }

    #[test]
    fn json_cli_report_escapes_non_ascii_like_python_json_dumps() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("café🦀");
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let now = UNIX_EPOCH + std::time::Duration::from_secs(1_900_000_000);
        let actual = cli_output(
            &root,
            AuditOptions::default(),
            true,
            false,
            false,
            now,
            &OsAuditFileSystem,
        )
        .unwrap();
        assert!(actual.stdout.contains("\\u00e9\\ud83e\\udd80"));
        assert!(!actual.stdout.contains("café🦀"));
        if let Some(expected) = oracle(json!([{
            "op":"cli",
            "argv":[root.to_string_lossy().to_string(),"--json"]
        }])) {
            assert_eq!(actual.stdout, expected[0]["stdout"].as_str().unwrap());
        }
    }

    #[test]
    fn run_scaffolds_and_handoff_metadata_match_python_policy() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let run = root.join(".agent-run/runs/20260923-1000-dispatch-broken-a1b2c3");
        fs::create_dir_all(&run).unwrap();
        let handoffs = root.join("docs/handoffs");
        fs::create_dir_all(&handoffs).unwrap();
        fs::write(
            handoffs.join("HANDOFF-partial.md"),
            "Status: active\nEffort: E1\nLeg: L2\nSupersedes: none\nConsumed-at: pending\n",
        )
        .unwrap();
        let now = UNIX_EPOCH + std::time::Duration::from_secs(1_900_000_000);
        let findings = audit_os(&root, AuditOptions::default(), now).unwrap();
        assert!(findings
            .iter()
            .any(|finding| finding.code == "incomplete-run-index"));
        let fixture = json!([{"op":"audit","root":root,"now":"2030-03-17T17:46:40+00:00"}]);
        if let Some(expected) = oracle(fixture) {
            assert_eq!(json!(findings), expected[0]);
        }
    }

    #[test]
    fn manifest_handoffs_and_canonical_keys_match_python_policy() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let docs = root.join("docs");
        let handoffs = docs.join("handoffs");
        fs::create_dir_all(&handoffs).unwrap();
        fs::write(docs.join("one.md"), "Canonical key: current-architecture\n").unwrap();
        fs::write(docs.join("two.md"), "Canonical key: current-architecture\n").unwrap();
        let active =
            "Status: active\nEffort: E1\nLeg: L2\nSupersedes: none\nConsumed-at: pending\n";
        fs::write(handoffs.join("HANDOFF-one.md"), active).unwrap();
        fs::write(handoffs.join("HANDOFF-two.md"), active).unwrap();
        fs::write(handoffs.join("HANDOFF-partial.md"), "Status: active\n").unwrap();
        let run = root.join(".agent-run/runs/20260923-1000-delivery-DEL-1-a1b2c3");
        fs::create_dir_all(&run).unwrap();
        fs::write(
            run.join("RUN.json"),
            json!({
                "schema_version": 1,
                "contract": "delivery-run",
                "artifacts": [{
                    "id": "scratch-one",
                    "path": "scratch-one.tmp",
                    "class": "scratch",
                    "owner": "run",
                    "retention": "until-expiry",
                    "expires_at": "2026-01-01T00:00:00Z"
                }]
            })
            .to_string(),
        )
        .unwrap();
        fs::write(run.join("scratch-one.tmp"), "owned").unwrap();
        fs::write(run.join("scratch-orphan.tmp"), "unknown").unwrap();
        let now = UNIX_EPOCH + std::time::Duration::from_secs(1_900_000_000);
        let findings = audit_os(&root, AuditOptions::default(), now).unwrap();
        let codes: BTreeSet<_> = findings
            .iter()
            .map(|finding| finding.code.as_str())
            .collect();
        for code in [
            "duplicate-canonical-key",
            "duplicate-active-handoff",
            "handoff-metadata-missing",
            "expired-run-scratch",
            "orphan-run-scratch",
        ] {
            assert!(codes.contains(code), "missing {code}");
        }
        let fixture = json!([{
            "op":"audit",
            "root":root,
            "now":"2030-03-17T17:46:40+00:00"
        }]);
        if let Some(expected) = oracle(fixture) {
            assert_eq!(json!(findings), expected[0]);
        }
    }

    #[cfg(unix)]
    #[test]
    fn pruned_walk_skips_worktrees_and_outside_symlink_targets() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let outside_temp = tempfile::tempdir().unwrap();
        let outside = outside_temp.path();
        fs::create_dir_all(outside.join("private-run")).unwrap();
        fs::write(outside.join("private-run/MANIFEST.md"), "private").unwrap();
        fs::write(outside.join("secret.md"), "Canonical key: private\n").unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::create_dir_all(root.join(".worktrees/peer/docs/handoffs")).unwrap();
        fs::write(root.join(".worktrees/peer/AGENTS.md"), "x".repeat(20_000)).unwrap();
        fs::write(
            root.join(".worktrees/peer/docs/handoffs/HANDOFF-old.md"),
            "old",
        )
        .unwrap();
        symlink(outside, root.join(".agent-run")).unwrap();
        symlink(outside.join("secret.md"), root.join("docs/linked.md")).unwrap();
        let findings = audit_os(
            &root,
            AuditOptions::default(),
            UNIX_EPOCH + std::time::Duration::from_secs(1_900_000_000),
        )
        .unwrap();
        assert!(findings.is_empty());
    }

    #[test]
    fn state_freshness_uses_aware_python_isoformat_values() {
        let patterns = Patterns::new().unwrap();
        assert!(has_freshness("Updated: 2026-07-10T12:00:00Z", &patterns));
        assert!(!has_freshness("Updated: 2026-07-10", &patterns));
        assert!(!has_freshness("Updated: <YYYY-MM-DDTHH:MM:SSZ>", &patterns));
    }
}
