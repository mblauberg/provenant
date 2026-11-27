//! Peer projection: locate a home-relative project, or mirror a read-only
//! snapshot of a project that the peer cannot locate directly.
//!
//! A mirror is a real Git snapshot of the working tree (tracked and untracked,
//! non-ignored). It is taken with a private scratch index, so the live index,
//! refs and working tree are never touched. The snapshot is bundled as exactly
//! one ref and the bundle's object ID is verified before the peer binds it.
//! Readers that see a mirror are told by [`PEER_SOURCE_SNAPSHOT`] that they are
//! looking at a snapshot rather than live edits.

use provenant_core::CancelToken;
use provenant_os::exec::{ExecError, ExecOptions, Executor, StreamPolicy, Termination};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use thiserror::Error;

/// The setting that selects how a peer resolves a project for a call.
pub const PROJECTION_KEY: &str = "domains.fabric.project_projection";
/// Names the snapshot source on a mirrored read.
pub const PEER_SOURCE_SNAPSHOT: &str = "PEER_SOURCE_SNAPSHOT";
/// A writer whose project cannot be located on the peer: an operation error,
/// not a policy refusal.
pub const PEER_PROJECT_UNLOCATABLE: &str = "PEER_PROJECT_UNLOCATABLE";

const GIT_TIMEOUT: Duration = Duration::from_secs(30);
const BUNDLE_HEADER: &[u8] = b"# v2 git bundle\n";
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Values of [`PROJECTION_KEY`]. Unknown values resolve to `auto`, which is
/// the shipped default and keeps a forward-compatible reader permissive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionMode {
    /// Use the home-relative `ProjectLocator`, then mirror for readers.
    Auto,
    /// Only the home-relative `ProjectLocator`; never mirror.
    Locate,
    /// Mirror a snapshot for readers; writers still need a locatable project.
    Mirror,
}

impl ProjectionMode {
    pub fn parse(value: &str) -> Self {
        match value {
            "locate" => Self::Locate,
            "mirror" => Self::Mirror,
            _ => Self::Auto,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Locate => "locate",
            Self::Mirror => "mirror",
        }
    }

    /// Only read-only peer actions may cross through a mirror. A writer that
    /// cannot locate the project is an operation error in every mode.
    pub fn allows_mirror(self, reader: bool) -> bool {
        reader && matches!(self, Self::Auto | Self::Mirror)
    }
}

#[derive(Debug, Error)]
pub enum MirrorError {
    #[error("Git executable is unavailable")]
    GitUnavailable,
    #[error("Git {operation} failed: {detail}")]
    Git {
        operation: &'static str,
        detail: String,
    },
    #[error("snapshot execution failed: {0}")]
    Exec(#[from] ExecError),
    #[error("snapshot filesystem work failed: {0}")]
    Io(#[from] io::Error),
    #[error("bundle verification failed: {0}")]
    Verify(String),
}

/// A verified, read-only snapshot ready to be bound by a peer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MirrorSnapshot {
    pub origin_host: String,
    pub project_id: String,
    /// The snapshot commit object ID; the bundle carries exactly this ref.
    pub commit: String,
    /// The bundle bytes, already verified to carry `commit`.
    pub bundle: Vec<u8>,
    /// Where the bundle was written within the mirror state.
    pub path: PathBuf,
}

/// A bounded, safe directory segment. Host IDs and project IDs are hex, but a
/// mirror is also written for labels from host config, so narrow every segment
/// to a known-safe alphabet before it becomes a path component.
fn segment(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if out.is_empty() {
        out.push_str("unknown");
    }
    out.truncate(128);
    out
}

/// `<state>/mirrors/<origin-host-id>/<project-id>`.
pub fn mirror_dir(state: &Path, origin_host: &str, project_id: &str) -> PathBuf {
    state
        .join("mirrors")
        .join(segment(origin_host))
        .join(segment(project_id))
}

/// Verify that a v2 Git bundle advertises the expected object ID before a peer
/// binds it. The header is text; everything after its blank line is the pack
/// and is deliberately never decoded here.
pub fn verify_bundle(bundle: &[u8], expected: &str) -> Result<(), MirrorError> {
    let fail = |detail: &str| MirrorError::Verify(detail.to_owned());
    if !bundle.starts_with(BUNDLE_HEADER) {
        return Err(fail("not a v2 git bundle"));
    }
    let mut cursor = BUNDLE_HEADER.len();
    let mut found = false;
    loop {
        let relative = bundle[cursor..]
            .iter()
            .position(|byte| *byte == b'\n')
            .ok_or_else(|| fail("unterminated bundle header"))?;
        let end = cursor + relative;
        let line = &bundle[cursor..end];
        if line.is_empty() {
            break;
        }
        let line = std::str::from_utf8(line).map_err(|_| fail("non-UTF-8 bundle header"))?;
        if line
            .split_once(' ')
            .is_some_and(|(object, _reference)| object == expected)
        {
            found = true;
        }
        cursor = end + 1;
    }
    if found {
        Ok(())
    } else {
        Err(fail(&format!("bundle does not advertise {expected}")))
    }
}

fn git_environment(worktree: &Path, index: &Path) -> Vec<(OsString, OsString)> {
    vec![
        (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
        (OsString::from("HOME"), OsString::from("/tmp")),
        (OsString::from("LANG"), OsString::from("C")),
        (OsString::from("LC_ALL"), OsString::from("C")),
        (
            OsString::from("GIT_WORK_TREE"),
            worktree.as_os_str().to_owned(),
        ),
        (
            OsString::from("GIT_INDEX_FILE"),
            index.as_os_str().to_owned(),
        ),
        (OsString::from("GIT_CONFIG_NOSYSTEM"), OsString::from("1")),
        (
            OsString::from("GIT_CONFIG_GLOBAL"),
            OsString::from("/dev/null"),
        ),
        (OsString::from("GIT_OPTIONAL_LOCKS"), OsString::from("0")),
        (
            OsString::from("GIT_AUTHOR_NAME"),
            OsString::from("Provenant"),
        ),
        (
            OsString::from("GIT_AUTHOR_EMAIL"),
            OsString::from("mirror@provenant.invalid"),
        ),
        (
            OsString::from("GIT_AUTHOR_DATE"),
            OsString::from("@0 +0000"),
        ),
        (
            OsString::from("GIT_COMMITTER_NAME"),
            OsString::from("Provenant"),
        ),
        (
            OsString::from("GIT_COMMITTER_EMAIL"),
            OsString::from("mirror@provenant.invalid"),
        ),
        (
            OsString::from("GIT_COMMITTER_DATE"),
            OsString::from("@0 +0000"),
        ),
    ]
}

fn git(
    executor: &dyn Executor,
    git: &Path,
    worktree: &Path,
    index: &Path,
    operation: &'static str,
    args: &[OsString],
    cancel: &CancelToken,
) -> Result<String, MirrorError> {
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(git.as_os_str().to_owned());
    argv.extend_from_slice(args);
    let env = git_environment(worktree, index);
    let mut options = ExecOptions::new(&argv, worktree, GIT_TIMEOUT);
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture { limit: 1 << 20 };
    options.stderr = StreamPolicy::Discard;
    options.cancel = Some(cancel);
    let outcome = executor.execute(options).map_err(MirrorError::Exec)?;
    if outcome.timed_out || outcome.cancelled {
        return Err(MirrorError::Git {
            operation,
            detail: "bounded Git call did not finish".to_owned(),
        });
    }
    if !matches!(outcome.termination, Termination::Exit(0)) {
        return Err(MirrorError::Git {
            operation,
            detail: format!("exit {:?}", outcome.termination),
        });
    }
    String::from_utf8(outcome.stdout.bytes)
        .map(|text| text.trim().to_owned())
        .map_err(|error| MirrorError::Git {
            operation,
            detail: error.to_string(),
        })
}

/// Capture the working tree into a deterministic snapshot commit and bundle
/// exactly that ref into the mirror state. The live index and refs are never
/// modified: all Git writes use a private scratch index.
#[allow(clippy::too_many_arguments)]
pub fn mirror_snapshot(
    executor: &dyn Executor,
    git_executable: &Path,
    worktree: &Path,
    scratch: &Path,
    state: &Path,
    origin_host: &str,
    project_id: &str,
    cancel: &CancelToken,
) -> Result<MirrorSnapshot, MirrorError> {
    if !git_executable.is_absolute() {
        return Err(MirrorError::GitUnavailable);
    }
    fs::create_dir_all(scratch)?;
    let index = scratch.join(format!(
        "index-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));

    git(
        executor,
        git_executable,
        worktree,
        &index,
        "add",
        &[OsString::from("add"), OsString::from("-A")],
        cancel,
    )?;
    let tree = git(
        executor,
        git_executable,
        worktree,
        &index,
        "write-tree",
        &[OsString::from("write-tree")],
        cancel,
    )?;
    let commit = git(
        executor,
        git_executable,
        worktree,
        &index,
        "commit-tree",
        &[
            OsString::from("commit-tree"),
            OsString::from(&tree),
            OsString::from("-m"),
            OsString::from("Provenant peer snapshot"),
        ],
        cancel,
    )?;
    if commit.is_empty() {
        return Err(MirrorError::Git {
            operation: "commit-tree",
            detail: "no object ID returned".to_owned(),
        });
    }

    let directory = mirror_dir(state, origin_host, project_id);
    fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{commit}.bundle"));
    let bundle_arg = OsString::from(path.as_os_str());
    git(
        executor,
        git_executable,
        worktree,
        &index,
        "bundle",
        &[
            OsString::from("bundle"),
            OsString::from("create"),
            bundle_arg,
            OsString::from(&commit),
        ],
        cancel,
    )?;
    let bundle = fs::read(&path)?;
    verify_bundle(&bundle, &commit)?;
    let _ = fs::remove_file(&index);
    Ok(MirrorSnapshot {
        origin_host: origin_host.to_owned(),
        project_id: project_id.to_owned(),
        commit,
        bundle,
        path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_os::exec::{CapturedStream, ExecOutcome};

    #[test]
    fn projection_modes_round_trip_and_default_forward() {
        for (text, mode) in [
            ("auto", ProjectionMode::Auto),
            ("locate", ProjectionMode::Locate),
            ("mirror", ProjectionMode::Mirror),
        ] {
            assert_eq!(ProjectionMode::parse(text), mode);
            assert_eq!(mode.as_str(), text);
        }
        assert_eq!(ProjectionMode::parse("new-mode"), ProjectionMode::Auto);
    }

    #[test]
    fn only_readers_may_use_a_mirror() {
        assert!(ProjectionMode::Auto.allows_mirror(true));
        assert!(ProjectionMode::Mirror.allows_mirror(true));
        assert!(!ProjectionMode::Mirror.allows_mirror(false));
        assert!(!ProjectionMode::Locate.allows_mirror(true));
        assert!(!ProjectionMode::Auto.allows_mirror(false));
    }

    #[test]
    fn mirror_path_is_bounded_and_segmented() {
        let path = mirror_dir(Path::new("/state"), "host/../id", "proj");
        assert_eq!(path, Path::new("/state/mirrors/host-..-id/proj"));
        assert_eq!(
            mirror_dir(Path::new("/state"), "", ""),
            Path::new("/state/mirrors/unknown/unknown")
        );
    }

    #[test]
    fn bundle_verification_accepts_expected_object_and_rejects_others() {
        let bundle = b"# v2 git bundle\nabc123 refs/heads/main\n\nPACKDATA".to_vec();
        assert!(verify_bundle(&bundle, "abc123").is_ok());
        assert!(verify_bundle(&bundle, "other").is_err());
        assert!(verify_bundle(b"not a bundle", "abc123").is_err());
        assert!(verify_bundle(b"# v2 git bundle\nabc123", "abc123").is_err());
    }

    /// A deterministic executor that returns a configured commit for
    /// `commit-tree`, a plausible bundle for `bundle create`, and empty output
    /// for the scratch-index commands. The call sequence is the seam under
    /// test; real Git is exercised by the host's integration fixtures.
    struct RecordingExecutor {
        tree: String,
        commit: String,
        calls: std::sync::Mutex<Vec<Vec<String>>>,
    }
    impl Executor for RecordingExecutor {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            let argv: Vec<String> = options
                .argv
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            self.calls.lock().unwrap().push(argv.clone());
            let stdout = if argv.iter().any(|arg| arg == "write-tree") {
                self.tree.clone()
            } else if argv.iter().any(|arg| arg == "commit-tree") {
                self.commit.clone()
            } else {
                String::new()
            };
            if argv.iter().any(|arg| arg == "bundle") {
                let path = argv
                    .iter()
                    .find(|arg| arg.ends_with(".bundle"))
                    .expect("bundle create writes a .bundle file");
                fs::write(
                    path,
                    format!("# v2 git bundle\n{} refs/heads/mirror\n\nPACK", self.commit),
                )
                .expect("write fixture bundle");
            }
            Ok(ExecOutcome {
                termination: Termination::Exit(0),
                stdout: CapturedStream {
                    bytes: stdout.into_bytes(),
                    ..Default::default()
                },
                stderr: CapturedStream::default(),
                timed_out: false,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 0,
            })
        }
    }

    #[test]
    fn snapshot_uses_a_scratch_index_and_verifies_the_bundle() {
        let root = std::env::temp_dir().join(format!("mirror-test-{}", std::process::id()));
        let worktree = root.join("worktree");
        let state = root.join("state");
        fs::create_dir_all(&worktree).expect("create worktree");
        let executor = RecordingExecutor {
            tree: "tree-oid".to_owned(),
            commit: "commit-oid".to_owned(),
            calls: std::sync::Mutex::new(Vec::new()),
        };
        let snapshot = mirror_snapshot(
            &executor,
            Path::new("/usr/bin/git"),
            &worktree,
            &root.join("scratch"),
            &state,
            "host-a",
            "project-b",
            &CancelToken::new(),
        )
        .expect("snapshot is bundled and verified");
        assert_eq!(snapshot.commit, "commit-oid");
        assert_eq!(
            snapshot.path,
            mirror_dir(&state, "host-a", "project-b").join("commit-oid.bundle")
        );
        assert!(snapshot.bundle.starts_with(BUNDLE_HEADER));
        let calls = executor.calls.lock().unwrap();
        assert!(
            calls
                .iter()
                .any(|argv| argv.iter().any(|a| a == "write-tree"))
        );
        assert!(
            calls
                .iter()
                .any(|argv| argv.iter().any(|a| a == "commit-tree"))
        );
        assert!(calls.iter().any(|argv| argv.iter().any(|a| a == "bundle")));
        drop(calls);
        fs::remove_dir_all(&root).expect("remove scratch");
    }
}
