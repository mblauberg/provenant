//! Worktree contracts and pure safety policy. Git mutation remains owner work.
use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};
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

pub fn validate_name(name: &str) -> Result<(), PolicyError> {
    let valid = (1..=64).contains(&name.len())
        && name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if valid {
        Ok(())
    } else {
        Err(PolicyError::InvalidName)
    }
}
pub fn full_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    Flag,
    Required,
    Optional,
}
const FIELDS: &[(&str, FieldKind)] = &[
    ("bare", FieldKind::Flag),
    ("detached", FieldKind::Flag),
    ("worktree", FieldKind::Required),
    ("HEAD", FieldKind::Required),
    ("branch", FieldKind::Required),
    ("locked", FieldKind::Optional),
    ("prunable", FieldKind::Optional),
];
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
impl WorktreeRecord {
    pub fn get(&self, name: &str) -> Option<&PorcelainValue> {
        self.fields.iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }
}
fn malformed(reason: impl Into<String>) -> PolicyError {
    PolicyError::Malformed(reason.into())
}
fn append(
    records: &mut Vec<WorktreeRecord>,
    current: &mut WorktreeRecord,
) -> Result<(), PolicyError> {
    if current.fields.is_empty() {
        return Err(malformed("empty record"));
    }
    if current.get("worktree").is_none() {
        return Err(malformed("record has no worktree path"));
    }
    if current.get("bare") == Some(&PorcelainValue::Flag) {
        if ["HEAD", "branch", "detached"]
            .iter()
            .any(|k| current.get(k).is_some())
        {
            return Err(malformed("bare record has checkout state"));
        }
    } else if current.get("HEAD").is_none() {
        return Err(malformed("record has no HEAD"));
    } else if current.get("branch").is_some() == current.get("detached").is_some() {
        return Err(malformed(
            "record needs exactly one branch or detached state",
        ));
    }
    records.push(WorktreeRecord {
        fields: std::mem::take(&mut current.fields),
    });
    Ok(())
}
pub fn parse_records(raw: &[u8]) -> Result<Vec<WorktreeRecord>, PolicyError> {
    let Some(raw) = raw.strip_suffix(b"\0\0") else {
        return Err(PolicyError::Incomplete);
    };
    let mut records = Vec::new();
    let mut current = WorktreeRecord { fields: Vec::new() };
    for field in raw.split(|&b| b == 0) {
        if field.is_empty() {
            append(&mut records, &mut current)?;
            continue;
        }
        let split = field.iter().position(|&b| b == b' ');
        let key = split.map_or(field, |i| &field[..i]);
        let value = split.map_or(&[][..], |i| &field[i + 1..]);
        let name = String::from_utf8_lossy(key);
        let Some((_, kind)) = FIELDS.iter().find(|(n, _)| *n == name) else {
            return Err(malformed(format!(
                "unknown field {}",
                provenant_core::py::python_repr(&name)
            )));
        };
        if current.get(&name).is_some() {
            return Err(malformed(format!(
                "duplicate field {}",
                provenant_core::py::python_repr(&name)
            )));
        }
        if *kind == FieldKind::Flag && split.is_some() {
            return Err(malformed(format!(
                "flag field {} has a value",
                provenant_core::py::python_repr(&name)
            )));
        }
        if *kind == FieldKind::Required && (split.is_none() || value.is_empty()) {
            return Err(malformed(format!(
                "value field {} has no value",
                provenant_core::py::python_repr(&name)
            )));
        }
        current.fields.push((
            name.into_owned(),
            if value.is_empty() {
                PorcelainValue::Flag
            } else {
                PorcelainValue::Text(OsString::from_vec(value.to_vec()))
            },
        ));
    }
    if !current.fields.is_empty() {
        append(&mut records, &mut current)?;
    }
    if records.is_empty() {
        return Err(malformed("no worktree records"));
    }
    Ok(records)
}

pub const GENERATED_PREFIXES: &[&str] = &[
    ".agent-fabric/",
    ".agent-run/",
    ".pytest_cache/",
    ".review-snapshots/",
    ".venv/",
    "node_modules/",
];
pub fn ignored_path_is_generated(path: &str) -> bool {
    let normalized = path.trim_end_matches('/');
    GENERATED_PREFIXES.iter().any(|prefix| {
        normalized == prefix.trim_end_matches('/')
            || format!("/{normalized}/").contains(&format!("/{prefix}"))
    }) || format!("/{normalized}/").contains("/__pycache__/")
        || [".pyc", ".pyo", ".pyd"]
            .iter()
            .any(|s| normalized.ends_with(s))
}
pub fn residue(stdout: &str) -> Vec<String> {
    crate::pending::python_splitlines(stdout)
        .into_iter()
        .filter(|line| {
            let mut chars = line.chars();
            let code: String = chars.by_ref().take(2).collect();
            chars.next();
            !(code == "!!" && ignored_path_is_generated(chars.as_str()))
        })
        .map(str::to_owned)
        .collect()
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
/// The inexpensive pre-observation refusals, in source order.
pub fn claim_input_refusal(commit: Option<&str>, base: Option<&str>) -> Option<ClaimReceipt> {
    let reason = if commit.is_none_or(str::is_empty) {
        "missing claimed commit SHA"
    } else if !commit.is_some_and(full_object_id) {
        "claimed commit SHA must be a full Git object ID"
    } else if base.is_none_or(str::is_empty) {
        "missing pre-dispatch base revision"
    } else if !base.is_some_and(full_object_id) {
        "base revision must be a full Git object ID"
    } else {
        return None;
    };
    Some(ClaimReceipt::Rejected {
        reason: reason.into(),
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::os::unix::ffi::OsStrExt;
    fn record_json(record: &WorktreeRecord) -> Value {
        Value::Object(
            record
                .fields
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        match v {
                            PorcelainValue::Flag => json!(true),
                            PorcelainValue::Text(v) => json!(v.as_bytes()),
                        },
                    )
                })
                .collect(),
        )
    }
    #[test]
    fn porcelain_policy_matches_source_for_malformed_and_lossless_paths() {
        let sha = "a".repeat(40);
        let valid = format!("worktree /repo\0HEAD {sha}\0branch refs/heads/main\0\0").into_bytes();
        let mut cases = vec![
            valid.clone(),
            b"worktree /bare\0bare\0locked \0\0".to_vec(),
            b"\0\0".to_vec(),
            b"worktree /repo\0\0".to_vec(),
            b"worktree /repo\0bare\0detached\0\0".to_vec(),
        ];
        for extra in [
            b"bare x".as_slice(),
            b"unknown x",
            b"branch",
            b"HEAD dup",
            b"locked",
            b"prunable why",
            b"\xff odd",
        ] {
            let mut raw = valid[..valid.len() - 1].to_vec();
            raw.extend_from_slice(extra);
            raw.extend_from_slice(b"\0\0");
            cases.push(raw);
        }
        for len in [0, 1, valid.len() - 1] {
            cases.push(valid[..len].to_vec());
        }
        cases
            .push(b"worktree /repo/\xff\n path\0HEAD not-validated\0detached\0locked\0\0".to_vec());
        let mut large = Vec::new();
        for i in 0..500 {
            large.extend_from_slice(
                format!("worktree /repo/{i}\0HEAD {sha}\0detached\0\0").as_bytes(),
            );
        }
        cases.push(large);
        for case in cases {
            // These malformed-record refusals are policy fixtures, independent
            // of the subprocess/capture seams used by the Python tests.
            let result = parse_records(&case);
            if case == b"\0\0" {
                assert_eq!(
                    result.unwrap_err().to_string(),
                    "git worktree list output was malformed: empty record"
                );
            } else if case.ends_with(b"\0\0")
                && (case.starts_with(b"worktree /bare")
                    || case.starts_with(b"worktree /repo/\xff")
                    || case.len() > 10000
                    || case == valid)
            {
                assert!(result.is_ok());
            } else if !case.ends_with(b"\0\0") {
                assert_eq!(result.unwrap_err(), PolicyError::Incomplete);
            }
        }
    }
    #[test]
    fn real_git_porcelain_matches_source_without_parser_seams() {
        let dir = tempfile::tempdir().unwrap();
        let Some(expected): Option<Vec<(Vec<u8>, Value)>> = crate::oracle::python(
            r#"
import os,json,sys,subprocess
from pathlib import Path
from scripts.worktree import worktree_records
p=Path(json.load(sys.stdin)['root']); repo=p/'repo'; repo.mkdir()
def git(root,*args): return subprocess.run(['git','-C',str(root),*args],check=True,capture_output=True,timeout=10).stdout
git(repo,'init','-q');git(repo,'config','user.name','Fixture');git(repo,'config','user.email','fixture@example.test');git(repo,'commit','-q','--allow-empty','-m','fixture')
for name in ['space path','line\nbreak'] + ([os.fsdecode(b'invalid-\xff')] if sys.platform != 'darwin' else []): git(repo,'worktree','add','-q','--detach',str(p/name),'HEAD')
bare=p/'bare'; git(p,'clone','-q','--bare',str(repo),str(bare))
result=[]
for root in [repo,bare]:
 raw=git(root,'worktree','list','--porcelain','-z')
 rows=[{k:list(os.fsencode(v)) if isinstance(v,str) else v for k,v in row.items()} for row in worktree_records(root)]
 result.append([list(raw),rows])
print(json.dumps(result))
"#,
            &json!({"root":dir.path()}),
        ) else {
            return;
        };
        for (raw, expected) in expected {
            assert_eq!(
                json!(parse_records(&raw)
                    .unwrap()
                    .iter()
                    .map(record_json)
                    .collect::<Vec<_>>()),
                expected
            );
        }
    }

    #[test]
    fn generated_residue_and_claim_refusals_match_policy() {
        let paths = [
            "node_modules",
            "x/node_modules/a",
            "x/__pycache__/a",
            "a.pyc",
            "a.pyd/",
            ".venv/a",
            ".agent-runner/a",
            "vendor",
            "node_modulesx",
        ];
        let mut claims = vec![];
        for commit in [
            None,
            Some(""),
            Some("bad"),
            Some(&"a".repeat(40)),
            Some(&"A".repeat(64)),
        ] {
            for base in [None, Some("bad"), Some(&"b".repeat(40))] {
                claims.push((commit.map(str::to_owned), base.map(str::to_owned)));
            }
        }
        let Some(expected): Option<(Vec<bool>,Vec<Value>)> = crate::oracle::python(
            "import json,sys; from pathlib import Path; from scripts.worktree import ignored_path_is_generated,verify_claim; x=json.load(sys.stdin); print(json.dumps([[ignored_path_is_generated(p) for p in x['paths']],[verify_claim(Path('/absent'),Path('/absent'),c,base_revision=b) if not c or len(c) not in (40,64) or not b or len(b) not in (40,64) else None for c,b in x['claims']]]))", &json!({"paths":paths,"claims":claims})
        ) else { return; };
        assert_eq!(paths.map(ignored_path_is_generated).to_vec(), expected.0);
        for ((commit, base), expected) in claims.iter().zip(expected.1) {
            assert_eq!(
                serde_json::to_value(claim_input_refusal(commit.as_deref(), base.as_deref()))
                    .unwrap(),
                expected
            );
        }
        assert_eq!(
            residue("!! node_modules/\n?? src.py\n!! user-data/\n M a\n"),
            ["?? src.py", "!! user-data/", " M a"]
        );
    }
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
