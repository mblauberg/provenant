//! Git evidence reads. Packet streaming/publication awaits the supervisor safe API.
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Component, Path},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("diff-from must be a non-empty Git revision")]
    InvalidRevision,
    #[error("diff path must be a relative path: {0}")]
    InvalidPath(String),
    #[error("git_evidence_untracked: checkout contains untracked paths without packet content")]
    Untracked,
    #[error("git_evidence_changed: checkout HEAD or status changed during capture; retry")]
    Changed,
    #[error("Git evidence read in {repository} failed with status {status}: {stderr}")]
    Git {
        repository: std::path::PathBuf,
        status: i32,
        stderr: String,
    },
    #[error("Git evidence read in {repository}: {source}")]
    Io {
        repository: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Closed environment; caller routing, injected config and replacement refs are dropped.
pub fn sanitized_environment(
    source: &BTreeMap<OsString, OsString>,
) -> BTreeMap<OsString, OsString> {
    let mut environment = BTreeMap::new();
    for name in ["PATH", "HOME", "TMPDIR"] {
        if let Some(value) = source.get(OsStr::new(name)) {
            environment.insert(name.into(), value.clone());
        }
    }
    for (name, value) in [
        ("LC_ALL", "C"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ("GIT_NO_REPLACE_OBJECTS", "1"),
        ("GIT_GRAFT_FILE", "/dev/null"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("GIT_TERMINAL_PROMPT", "0"),
    ] {
        environment.insert(name.into(), value.into());
    }
    environment
}

/// A byte-preserving, bounded utility seam. The current supervisor's public
/// bounded runner returns replacement-decoded text; it cannot bind this API yet.
/// No local process lifecycle/capture implementation duplicates that owner.
pub trait GitRead {
    fn output(
        &mut self,
        repository: &Path,
        arguments: &[&OsStr],
        environment: &BTreeMap<OsString, OsString>,
    ) -> Result<GitReadOutput, std::io::Error>;
}
#[derive(Debug, Default)]
pub struct GitReadOutput {
    pub returncode: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub fn git_output(
    repository: &Path,
    args: &[&OsStr],
    source: &BTreeMap<OsString, OsString>,
    executor: &mut impl GitRead,
) -> Result<Vec<u8>, Error> {
    let output = executor
        .output(repository, args, &sanitized_environment(source))
        .map_err(|source| Error::Io {
            repository: repository.into(),
            source,
        })?;
    if output.returncode != 0 {
        return Err(Error::Git {
            repository: repository.into(),
            status: output.returncode,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(output.stdout)
}

pub fn validate_selection(revision: &str, paths: &[String]) -> Result<(), Error> {
    if revision.is_empty() || revision.contains('\0') || revision.starts_with('-') {
        return Err(Error::InvalidRevision);
    }
    for path in paths {
        if path.is_empty()
            || path.contains('\0')
            || path.starts_with('-')
            || Path::new(path).is_absolute()
        {
            return Err(Error::InvalidPath(provenant_core::py::python_repr(path)));
        }
    }
    Ok(())
}

pub fn untracked_paths(status: &[u8]) -> Vec<String> {
    let records: Vec<&[u8]> = if status.contains(&0) {
        status.split(|&b| b == 0).collect()
    } else {
        status.split(|&b| b == b'\n' || b == b'\r').collect()
    };
    records
        .into_iter()
        .filter_map(|record| record.strip_prefix(b"?? "))
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect()
}

pub fn check_untracked(status: &[u8], selected: &[String]) -> Result<(), Error> {
    let untracked = untracked_paths(status);
    let selected_components: Vec<Vec<_>> = selected
        .iter()
        .map(|s| {
            Path::new(s)
                .components()
                .filter(|c| *c != Component::CurDir)
                .collect()
        })
        .collect();
    if (!untracked.is_empty() && selected.is_empty())
        || untracked.iter().any(|path| {
            let components: Vec<_> = Path::new(path)
                .components()
                .filter(|c| *c != Component::CurDir)
                .collect();
            selected_components
                .iter()
                .any(|s| components.starts_with(s))
        })
    {
        return Err(Error::Untracked);
    }
    Ok(())
}

/// The source packet header, with the original field types and wire values.
/// Filesystem path-to-JSON encoding is still an owner integration prerequisite.
#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PacketMetadata {
    pub schema_version: u64,
    pub record_type: String,
    pub repository: String,
    pub git_root: String,
    pub head: String,
    pub diff_base: String,
    pub working_tree: WorkingTree,
    pub diff_from: String,
    pub paths: Vec<String>,
    pub encoding: String,
}
#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WorkingTree {
    Clean,
    Dirty,
}
/// Encode the exact retained header/status framing. Diff streaming and atomic
/// publication must complete before any concrete materialise command is exposed.
pub fn packet_prefix(
    metadata: &PacketMetadata,
    status: &[u8],
) -> Result<Vec<u8>, serde_json::Error> {
    let value = serde_json::to_value(metadata)?;
    let mut bytes = crate::pending::ascii_json(&value)?.into_bytes();
    bytes.extend_from_slice(b"\n--- status ---\n");
    bytes.extend_from_slice(
        String::from_utf8_lossy(status)
            .replace('\0', "\n")
            .as_bytes(),
    );
    bytes.extend_from_slice(b"--- diff ---\n");
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closes_environment_including_empty_values() {
        let source = BTreeMap::from([
            ("PATH".into(), "".into()),
            ("HOME".into(), "/home/test".into()),
            ("GIT_DIR".into(), "/wrong".into()),
            ("GIT_CONFIG_COUNT".into(), "bad".into()),
            ("SECRET".into(), "hidden".into()),
        ]);
        let env = sanitized_environment(&source);
        assert_eq!(env.len(), 11);
        assert_eq!(env.get(OsStr::new("PATH")), Some(&OsString::new()));
        assert!(!env.contains_key(OsStr::new("GIT_DIR")));
        assert!(!env.contains_key(OsStr::new("SECRET")));
        assert_eq!(env[OsStr::new("GIT_NO_LAZY_FETCH")], "1");
    }

    #[test]
    fn selection_is_literal_and_component_scoped() {
        let status = b" M tracked\0?? dir/new\0?? other\0?? invalid\xff\0";
        assert!(check_untracked(status, &[]).is_err());
        for path in ["dir", "dir/new", "."] {
            assert!(check_untracked(status, &[path.into()]).is_err());
        }
        for path in ["di", "*.py", "tracked"] {
            assert!(check_untracked(status, &[path.into()]).is_ok());
        }
        assert_eq!(untracked_paths(status).last().unwrap(), "invalid�");
        assert_eq!(untracked_paths(b"?? one\n?? two\r\n"), ["one", "two"]);
    }

    #[test]
    fn refuses_option_injection_and_requires_explicit_revision() {
        for revision in ["", "-HEAD", "a\0b"] {
            assert!(validate_selection(revision, &[]).is_err());
        }
        for path in ["", "-x", "/absolute", "a\0b"] {
            assert!(validate_selection("HEAD", &[path.into()]).is_err());
        }
        assert!(validate_selection("HEAD~1", &["*.py".into(), "../relative".into()]).is_ok());
    }

    #[test]
    fn read_request_closes_environment_without_owning_an_unbounded_child() {
        struct Fixture;
        impl GitRead for Fixture {
            fn output(
                &mut self,
                repository: &Path,
                arguments: &[&OsStr],
                environment: &BTreeMap<OsString, OsString>,
            ) -> Result<GitReadOutput, std::io::Error> {
                assert_eq!(repository, Path::new("/fixture"));
                assert_eq!(arguments, [OsStr::new("status")]);
                assert!(!environment.contains_key(OsStr::new("GIT_DIR")));
                Ok(GitReadOutput {
                    stdout: b"raw\xff".to_vec(),
                    ..Default::default()
                })
            }
        }
        assert_eq!(
            git_output(
                Path::new("/fixture"),
                &[OsStr::new("status")],
                &BTreeMap::from([("GIT_DIR".into(), "/wrong".into())]),
                &mut Fixture
            )
            .unwrap(),
            b"raw\xff"
        );
    }
    #[test]
    fn closed_environment_matches_python_source() {
        use serde_json::json;
        let source = json!({"PATH":"/bin", "HOME":"", "TMPDIR":"/tmp", "GIT_DIR":"/wrong", "GIT_CONFIG_COUNT":"broken", "LC_ALL":"bad", "TOKEN":"excluded"});
        let Some(expected): Option<BTreeMap<String, String>> = crate::oracle::python(
            "import json,sys; from scripts.git_evidence import sanitized_git_environment; print(json.dumps(sanitized_git_environment(json.load(sys.stdin))))", &source
        ) else { return; };
        let source: BTreeMap<String, String> = serde_json::from_value(source).unwrap();
        let source = source
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        let actual: BTreeMap<String, String> = sanitized_environment(&source)
            .into_iter()
            .map(|(k, v)| (k.into_string().unwrap(), v.into_string().unwrap()))
            .collect();
        assert_eq!(actual, expected);
    }
    #[test]
    fn packet_header_and_status_framing_match_exact_python_bytes() {
        let metadata = PacketMetadata {
            schema_version: 1,
            record_type: "provenant-git-evidence".into(),
            repository: "/fixture/é\nroot".into(),
            git_root: "/fixture/root".into(),
            head: "a".repeat(40),
            diff_base: "b".repeat(40),
            working_tree: WorkingTree::Dirty,
            diff_from: "HEAD~1".into(),
            paths: vec!["🦀\nfile".into(), "quote\"".into()],
            encoding: "utf-8-replacement".into(),
        };
        let status = b" M invalid\xff\0?? unicode-\xf0\x9f\xa6\x80\0";
        let Some(expected):Option<Vec<u8>>=crate::oracle::python("import json,sys; x=json.load(sys.stdin); print(json.dumps(list(json.dumps(x['metadata'],sort_keys=True,separators=(',',':')).encode('utf-8')+b'\\n--- status ---\\n'+bytes(x['status']).decode('utf-8','replace').replace('\\x00','\\n').encode('utf-8')+b'--- diff ---\\n')))",&serde_json::json!({"metadata":metadata,"status":status.as_slice()})) else {return;};
        assert_eq!(packet_prefix(&metadata, status).unwrap(), expected);
    }
}
