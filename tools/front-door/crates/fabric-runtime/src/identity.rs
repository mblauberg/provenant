//! Process-derived identity and repository layout, including copied-worktree fencing.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};
use thiserror::Error;

pub type Environment = BTreeMap<OsString, OsString>;
const REDIRECTS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_GLOBAL",
    "GIT_NAMESPACE",
];
const GIT_OUTPUT_LIMIT: u64 = 1024 * 1024;

pub fn without_git_redirects(env: &Environment) -> Environment {
    env.iter()
        .filter(|(key, _)| {
            let key = key.to_string_lossy();
            !REDIRECTS.contains(&key.as_ref())
                && !["GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_"]
                    .iter()
                    .any(|prefix| {
                        key.strip_prefix(prefix)
                            .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
                    })
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub project: PathBuf,
    pub cwd: PathBuf,
    pub agent_id: String,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registered_projects: Option<Vec<PathBuf>>,
}

fn absolute(path: &Path) -> io::Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            _ => result.push(component.as_os_str()),
        }
    }
    Ok(result)
}

fn canonical(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path).or_else(|_| absolute(path))
}

fn run_git(cwd: &Path, args: &[&OsStr], env: &Environment) -> io::Result<Vec<u8>> {
    let mut merged: Environment = std::env::vars_os().collect();
    merged.extend(env.clone());
    let mut child = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(without_git_redirects(&merged))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut bytes = Vec::new();
    let read_result = match child.stdout.take() {
        Some(stdout) => stdout.take(GIT_OUTPUT_LIMIT + 1).read_to_end(&mut bytes),
        None => Err(io::Error::other("git stdout unavailable")),
    };
    if read_result.is_err() || bytes.len() as u64 > GIT_OUTPUT_LIMIT {
        // A read-only Git query, never a provider owner or a process group.
        // Always reap even if the bounded read failed.
        let kill_result = child.kill();
        let wait_result = child.wait();
        read_result?;
        kill_result?;
        wait_result?;
        return Err(io::Error::other("git output exceeded maxBuffer"));
    }
    if !child.wait()?.success() {
        return Err(io::Error::other("git query failed"));
    }
    Ok(bytes)
}

fn strip_line_ending(mut value: Vec<u8>) -> Vec<u8> {
    if value.last() == Some(&b'\n') {
        value.pop();
    }
    if cfg!(windows) && value.last() == Some(&b'\r') {
        value.pop();
    }
    value
}

#[cfg(unix)]
fn os_string(bytes: Vec<u8>) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    OsString::from_vec(bytes)
}
#[cfg(not(unix))]
fn os_string(bytes: Vec<u8>) -> OsString {
    String::from_utf8_lossy(&bytes).into_owned().into()
}

fn git_path(cwd: &Path, field: &str, env: &Environment) -> io::Result<PathBuf> {
    let bytes = strip_line_ending(run_git(
        cwd,
        &[
            OsStr::new("rev-parse"),
            OsStr::new("--path-format=absolute"),
            OsStr::new(field),
        ],
        env,
    )?);
    if bytes.is_empty() {
        return Err(io::Error::other("git returned no path"));
    }
    canonical(Path::new(&os_string(bytes)))
}

fn valid_linked_metadata(top: &Path, git: &Path, common: &Path) -> bool {
    if git == common {
        return true;
    }
    let dot_git = top.join(".git");
    let back_pointer = git.join("gitdir");
    let valid = || -> io::Result<bool> {
        if !fs::symlink_metadata(&dot_git)?.file_type().is_file()
            || !fs::symlink_metadata(&back_pointer)?.file_type().is_file()
        {
            return Ok(false);
        }
        let raw = strip_line_ending(read_bounded(&back_pointer, GIT_OUTPUT_LIMIT)?);
        if raw.is_empty() || raw.contains(&0) {
            return Ok(false);
        }
        let target = PathBuf::from(os_string(raw));
        let target = if target.is_absolute() {
            target
        } else {
            git.join(target)
        };
        Ok(canonical(&target)? == canonical(&dot_git)?)
    };
    valid().unwrap_or(false)
}

fn read_bounded(path: &Path, cap: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(cap + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(io::Error::other("metadata exceeds read limit"));
    }
    Ok(bytes)
}

pub fn project_root(cwd: &Path, env: &Environment) -> io::Result<PathBuf> {
    let current = canonical(&absolute(cwd)?)?;
    let query = || -> io::Result<PathBuf> {
        let top = git_path(&current, "--show-toplevel", env)?;
        let git = git_path(&current, "--absolute-git-dir", env)?;
        let common = git_path(&current, "--git-common-dir", env)?;
        if !current.starts_with(&top) {
            return Ok(current.clone());
        }
        if let Ok(bytes) = run_git(
            &current,
            &[
                OsStr::new("worktree"),
                OsStr::new("list"),
                OsStr::new("--porcelain"),
                OsStr::new("-z"),
            ],
            env,
        ) {
            // Each record contains one worktree field; NUL framing preserves newlines.
            let records: Vec<PathBuf> = bytes
                .split(|b| *b == 0)
                .filter_map(|field| field.strip_prefix(b"worktree "))
                .filter_map(|path| canonical(Path::new(&os_string(path.to_vec()))).ok())
                .collect();
            if records.contains(&top) && valid_linked_metadata(&top, &git, &common) {
                if let Some(first) = records.first() {
                    return Ok(if first == &common { top } else { first.clone() });
                }
            }
        }
        Ok(top)
    };
    Ok(query().unwrap_or(current))
}

pub fn identify(env: &Environment, cwd: &Path) -> io::Result<Identity> {
    let provider = env
        .get(OsStr::new("AGENT_FABRIC_SEAT"))
        .or_else(|| env.get(OsStr::new("AGENT_FABRIC_CLIENT_LABEL")))
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "agent".into());
    let agent_id = env
        .get(OsStr::new("AGENT_FABRIC_LABEL"))
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| provider.clone());
    let cwd = canonical(cwd)?;
    Ok(Identity {
        project: project_root(&cwd, env)?,
        cwd,
        agent_id,
        provider,
        registered_projects: None,
    })
}

pub fn database_path(env: &Environment, home: &Path) -> io::Result<PathBuf> {
    let state = env
        .get(OsStr::new("AGENT_FABRIC_STATE_DIRECTORY"))
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/state/agent-harness/fabric"));
    absolute(&state.join("fabric.sqlite3"))
}

pub fn run_root(cwd: &Path, env: &Environment) -> io::Result<PathBuf> {
    let current = canonical(cwd)?;
    Ok(match git_path(&current, "--git-common-dir", env) {
        Ok(common) => common.parent().unwrap_or(&common).join(".agent-run"),
        Err(_) => current.join(".agent-run"),
    })
}

#[derive(Debug, Error)]
pub enum CommitShaError {
    #[error(
        "\"{input}\" is not a commit SHA of at least 7 hex digits; pass the full 40-digit commit SHA (git rev-parse <branch>)"
    )]
    Invalid { input: String },
    #[error(
        "\"{input}\" is unknown or ambiguous in {cwd}; pass the full 40-digit commit SHA (git rev-parse <branch>)"
    )]
    Unresolved { input: String, cwd: PathBuf },
}
impl CommitShaError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid { .. } => "expected_sha_invalid",
            Self::Unresolved { .. } => "expected_sha_unresolved",
        }
    }
}
fn lower_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

pub fn resolve_commit_sha(cwd: &Path, input: &str) -> Result<String, CommitShaError> {
    let sha = crate::pending::trim_ecmascript(input);
    if lower_sha(sha) {
        return Ok(sha.into());
    }
    if !(7..=40).contains(&sha.len()) || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(CommitShaError::Invalid {
            input: input.into(),
        });
    }
    let resolve = || -> Option<String> {
        let prefix = format!("--disambiguate={}", sha.to_ascii_lowercase());
        let output = run_git(
            cwd,
            &[OsStr::new("rev-parse"), OsStr::new(&prefix)],
            &Environment::new(),
        )
        .ok()?;
        let output = String::from_utf8_lossy(&output);
        let matches: Vec<_> = crate::pending::trim_ecmascript(&output)
            .split('\n')
            .collect();
        if matches.len() != 1 || !lower_sha(matches[0]) {
            return None;
        }
        let object = format!("{}^{{commit}}", matches[0]);
        let full = String::from_utf8(strip_line_ending(
            run_git(
                cwd,
                &[
                    OsStr::new("rev-parse"),
                    OsStr::new("--verify"),
                    OsStr::new("--quiet"),
                    OsStr::new(&object),
                ],
                &Environment::new(),
            )
            .ok()?,
        ))
        .ok()?;
        lower_sha(&full).then_some(full)
    };
    resolve().ok_or_else(|| CommitShaError::Unresolved {
        input: input.into(),
        cwd: cwd.into(),
    })
}

#[cfg(test)]
mod tests;
