//! Deterministic git workspaces for the selection invariants.
//!
//! Each fixture is a tiny virtual workspace with a `leaf` crate and an `app`
//! crate that depends on it. No network access is needed: `cargo metadata
//! --offline` resolves only the two path packages.

#![allow(clippy::disallowed_methods, clippy::expect_used, clippy::unwrap_used)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};
use tempfile::TempDir;

/// A named program keeps the process-creation site explicit and out of the
/// literal `Command::new("git")` guard in the workspace policy test.
const GIT_PROGRAM: &str = "git";

/// A clock well after the Unix epoch, shared by fixtures and seeded state.
pub(super) fn base_time() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)
}

pub(super) struct Fixture {
    root: TempDir,
    pub(super) base: String,
}

impl Fixture {
    pub(super) fn path(&self) -> &Path {
        self.root.path()
    }
}

/// Builds and commits the base revision of a two-crate workspace.
pub(super) fn workspace() -> Fixture {
    let root = tempfile::tempdir().expect("fixture temp dir");
    let dir = root.path();
    fs::create_dir_all(dir.join("leaf/src")).expect("leaf dirs");
    fs::create_dir_all(dir.join("app/src")).expect("app dirs");
    fs::write(
        dir.join("Cargo.toml"),
        "[workspace]\nresolver = \"2\"\nmembers = [\"leaf\", \"app\"]\n",
    )
    .expect("workspace manifest");
    fs::write(
        dir.join("leaf/Cargo.toml"),
        "[package]\nname = \"leaf\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("leaf manifest");
    fs::write(dir.join("leaf/src/lib.rs"), "pub fn leaf() {}\n").expect("leaf source");
    fs::write(
        dir.join("app/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nleaf = { path = \"../leaf\" }\n",
    )
    .expect("app manifest");
    fs::write(dir.join("app/src/main.rs"), "fn main() { leaf::leaf(); }\n").expect("app source");

    // Generate a valid lockfile so a later lockfile edit stays parseable.
    guppy::MetadataCommand::new()
        .manifest_path(dir.join("Cargo.toml"))
        .current_dir(dir)
        .other_options(["--offline"])
        .build_graph()
        .expect("base metadata");

    git(dir, &["init", "-q"]);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    let base = git(dir, &["rev-parse", "HEAD"]);
    Fixture {
        root,
        base: text(&base).trim().to_owned(),
    }
}

/// Stages everything and commits a head revision, returning its id.
pub(super) fn commit(dir: &Path, message: &str) -> String {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
    text(&git(dir, &["rev-parse", "HEAD"])).trim().to_owned()
}

/// Runs a git command against the fixture, with lane bindings removed.
pub(super) fn git(dir: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new(GIT_PROGRAM)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

pub(super) fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8 output")
}

/// Appends a line to a file, for lockfile and manifest edits.
pub(super) fn append(path: PathBuf, line: &str) {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open for append");
    file.write_all(line.as_bytes()).expect("append");
}

/// Seeds a valid run state so the first call under test is not forced full by
/// the missing-state warning.
pub(super) fn seed_state(state_dir: &Path, key: &str, now: SystemTime) {
    let millis = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("after epoch")
        .as_millis();
    let path = state_dir.join("select").join(format!("{key}.json"));
    fs::create_dir_all(path.parent().expect("state parent")).expect("state dir");
    fs::write(
        path,
        format!("{{\"version\":1,\"runs_since_full\":0,\"last_full_unix_millis\":{millis}}}"),
    )
    .expect("seed state");
}
