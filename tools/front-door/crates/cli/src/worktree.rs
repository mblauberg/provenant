//! Worktree contracts and pure safety policy. Git mutation remains owner work.
use std::{ffi::OsString,path::PathBuf};
use thiserror::Error;
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("worktree name must be 1-64 safe filename characters without slashes")]
    InvalidName,
    #[error("git worktree list output was incomplete")]
    Incomplete,
    #[error("git worktree list output was malformed: {0}")]
    Malformed(String),
    #[error("detached worktrees require a name")]
    DetachedName,
}

/// Values are either the source's True or lossless surrogateescaped text. This
/// is an internal OS contract; a JSON path codec is still needed at the edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PorcelainValue {
    Flag,
    Text(OsString),
}
/// Insertion order is retained, including optional fields and flag-vs-text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRecord {
    pub fields: Vec<(String, PorcelainValue)>,
}
#[derive(Debug, clap::Parser)]
#[command(name = "worktree", args_override_self = true)]
pub struct Arguments {
    #[command(subcommand)]
    pub command: Operation,
}
#[derive(Debug, clap::Subcommand)]
pub enum Operation {
    Create(Create),
    List(Repository),
    Check(Repository),
    ValidateContext {
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long)]
        allow_non_git: bool,
    },
    VerifyClaim {
        #[arg(long)]
        expected_worktree: PathBuf,
        #[arg(long)]
        claimed_worktree: PathBuf,
        #[arg(long)]
        claimed_commit: Option<String>,
        #[arg(long)]
        base_revision: Option<String>,
        #[arg(long)]
        expected_common: Option<PathBuf>,
    },
    Remove {
        name: String,
        #[arg(long)]
        repo: Option<PathBuf>,
        #[arg(long)]
        force: bool,
    },
}
#[derive(Debug, clap::Args)]
pub struct Repository {
    #[arg(long)]
    pub repo: Option<PathBuf>,
}
#[derive(Debug, clap::Args)]
#[command(group(clap::ArgGroup::new("mode").required(true).args(["detach","existing_branch","new_branch"])))]
pub struct Create {
    pub name: Option<String>,
    #[arg(long)]
    pub repo: Option<PathBuf>,
    #[arg(long)]
    pub no_node_modules: bool,
    #[arg(long)]
    pub detach: Option<String>,
    #[arg(long)]
    pub existing_branch: Option<String>,
    #[arg(long)]
    pub new_branch: Option<String>,
    #[arg(long, default_value = "HEAD")]
    pub start_point: String,
}
/// Exact verify_claim result variants. Accepted records have no reason and
/// rejected records have none of the acceptance fields.
#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ClaimReceipt {
    Rejected {
        reason: String,
    },
    Accepted {
        base_revision: String,
        head_revision: String,
        clean: bool,
        claimed_commit: String,
        claimed_worktree: String,
        common_git_dir: String,
        acceptance_owner: String,
        acceptance_mode: String,
    },
}

/// Exact records returned by create/remove/check; JSON path rendering is pending.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Created {
    pub status: String,
    pub name: String,
    pub primary_root: String,
    pub worktree_root: String,
    pub common_git_dir: String,
    pub head_revision: String,
    pub branch: Option<String>,
    pub detached: bool,
    pub node_modules: String,
    pub node_modules_reason: Option<String>,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Removed {
    pub status: String,
    pub name: String,
    pub primary_root: String,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Checked {
    pub status: String,
    pub primary_root: String,
    pub findings: Vec<String>,
}

pub const TRUSTED_PATH: &str = "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/local/sbin:/opt/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
const HARDENED_SETTINGS: &[(&str, &str)] = &[
    ("core.hooksPath", "/dev/null"),
    ("core.fsmonitor", "false"),
    ("submodule.recurse", "false"),
    ("fetch.recurseSubmodules", "false"),
    ("gc.auto", "0"),
    ("maintenance.auto", "false"),
];
/// The process owner's filter discovery remains explicit. Git's name-only
/// output is decoded with replacement in the source, unlike porcelain paths.
pub fn git_environment(
    hardened: bool,
    filter_names: Option<&[String]>,
) -> std::collections::BTreeMap<String, String> {
    use std::collections::{BTreeMap, BTreeSet};
    let mut environment: BTreeMap<String, String> = [
        ("PATH", TRUSTED_PATH),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_TERMINAL_PROMPT", "0"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect();
    if !hardened {
        return environment;
    }
    environment.insert("GIT_NO_REPLACE_OBJECTS".into(), "1".into());
    environment.insert("GIT_NO_LAZY_FETCH".into(), "1".into());
    let mut settings: Vec<(String, String)> = HARDENED_SETTINGS
        .iter()
        .map(|(k, v)| ((*k).into(), (*v).into()))
        .collect();
    if let Some(names) = filter_names {
        settings.extend(
            names
                .iter()
                .filter(|name| !name.ends_with(".required"))
                .map(|name| (name.clone(), String::new())),
        );
        let drivers: BTreeSet<_> = names
            .iter()
            .map(|name| {
                name.rsplit_once('.')
                    .map_or(name.as_str(), |(driver, _)| driver)
            })
            .collect();
        settings.extend(
            drivers
                .into_iter()
                .map(|driver| (format!("{driver}.required"), "false".into())),
        );
    }
    environment.insert("GIT_CONFIG_COUNT".into(), settings.len().to_string());
    for (i, (key, value)) in settings.into_iter().enumerate() {
        environment.insert(format!("GIT_CONFIG_KEY_{i}"), key);
        environment.insert(format!("GIT_CONFIG_VALUE_{i}"), value);
    }
    environment
}
#[cfg(test)]
mod environment_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn closed_and_hardened_environment_match_source_with_real_filters() {
        let dir = tempfile::tempdir().unwrap();
        let Some(expected): Option<(
            std::collections::BTreeMap<String, String>,
            std::collections::BTreeMap<String, String>,
            Vec<String>,
        )> = crate::oracle::python(
            r#"
import json,sys,subprocess
from pathlib import Path
from scripts.worktree import git_environment
p=Path(json.load(sys.stdin)['root'])
def git(*a):return subprocess.run(['git','-C',str(p),*a],check=True,capture_output=True,timeout=10).stdout
git('init','-q');git('config','filter.z.clean','cat');git('config','filter.a.required','true');git('config','filter.z.process','cat');git('config','filter.a.smudge','cat')
names=git('config','--null','--name-only','--get-regexp',r'^filter\..*\.(clean|smudge|process|required)$').decode(errors='replace').split('\0')
print(json.dumps([git_environment(),git_environment(p,hardened=True),[n for n in names if n]]))
"#,
            &json!({"root":dir.path()}),
        ) else {
            return;
        };
        assert_eq!(git_environment(false, None), expected.0);
        assert_eq!(git_environment(true, Some(&expected.2)), expected.1);
    }
}
