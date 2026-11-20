//! Invariants for the affected-crate selector.
//!
//! Tests here pin observable behaviour at the public `select` boundary. They
//! never run in this lane; CI owns the test run. Clippy compiles them through
//! `--all-targets`.

#![allow(clippy::disallowed_methods, clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::{append, base_time, commit, git, seed_state, text, workspace};
use provenant_select::{Policy, Reason, select};
use std::{fs, path::Path, time::Duration};

const KEY: &str = "fixture";

#[test]
fn first_selection_is_full_then_later_selection_is_incremental() {
    let fixture = workspace();
    let dir = fixture.path();
    fs::write(
        dir.join("app/src/main.rs"),
        "fn main() { leaf::leaf(); let _ = 1; }\n",
    )
    .unwrap();
    let head = commit(dir, "change app");
    let state = tempfile::tempdir().unwrap();
    let run = || {
        select(
            dir,
            &fixture.base,
            &head,
            &Policy::default(),
            state.path(),
            KEY,
            base_time(),
        )
        .unwrap()
    };
    let first = run();
    assert!(first.full);
    assert_eq!(first.reason, Reason::NoPriorRun);
    assert_eq!(first.packages, ["app", "leaf"]);
    let second = run();
    assert!(!second.full);
    assert_eq!(second.reason, Reason::Changed);
    assert_eq!(second.packages, ["app"]);
}

#[test]
fn leaf_change_selects_leaf_and_dependents() {
    let fixture = workspace();
    let dir = fixture.path();
    fs::write(
        dir.join("leaf/src/lib.rs"),
        "pub fn leaf() { let _ = 1; }\n",
    )
    .expect("edit leaf");
    let head = commit(dir, "change leaf");
    let state = tempfile::tempdir().expect("state dir");
    seed_state(state.path(), KEY, base_time());

    let selection = select(
        dir,
        &fixture.base,
        &head,
        &Policy::default(),
        state.path(),
        KEY,
        base_time(),
    )
    .expect("select");

    assert_eq!(selection.packages, ["app", "leaf"]);
    assert!(!selection.full);
    assert_eq!(selection.reason, Reason::Changed);
}

#[test]
fn dependency_kind_change_selects_the_changed_member() {
    let fixture = workspace();
    let dir = fixture.path();
    // Moving `leaf` from a normal to a dev dependency changes app's manifest
    // dependency set without changing the lockfile's package set.
    fs::write(
        dir.join("app/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dev-dependencies]\nleaf = { path = \"../leaf\" }\n",
    )
    .expect("edit app manifest");
    let head = commit(dir, "change dependency kind");
    let state = tempfile::tempdir().expect("state dir");
    seed_state(state.path(), KEY, base_time());

    let selection = select(
        dir,
        &fixture.base,
        &head,
        &Policy::default(),
        state.path(),
        KEY,
        base_time(),
    )
    .expect("select");

    assert!(!selection.full);
    assert_eq!(selection.packages, ["app"]);
    assert_eq!(selection.reason, Reason::Changed);
}

#[test]
fn lockfile_or_toolchain_change_selects_full() {
    type Trigger = fn(&Path);
    type Acceptor = fn(&str) -> bool;

    // Each trigger gets a fresh workspace so edits do not accumulate.
    let cases: &[(&str, Trigger)] = &[
        ("lockfile", |dir| {
            append(dir.join("Cargo.lock"), "# touched\n")
        }),
        ("toolchain", |dir: &Path| {
            // A `rust-toolchain*` file that rustup ignores, so metadata still runs.
            fs::write(dir.join("rust-toolchain.test"), "channel = \"test\"\n")
                .expect("toolchain file");
        }),
        ("cargo config", |dir| {
            fs::create_dir_all(dir.join(".cargo")).expect("cargo dir");
            fs::write(dir.join(".cargo/config.toml"), "[build]\n").expect("cargo config");
        }),
        ("workspace manifest", |dir| {
            append(
                dir.join("Cargo.toml"),
                "\n[workspace.metadata]\nfull = true\n",
            )
        }),
    ];
    let expected: &[(&str, Acceptor)] = &[
        ("lockfile", |reason| reason.contains("lockfile changed")),
        ("toolchain", |reason| reason.contains("toolchain changed")),
        ("cargo config", |reason| {
            reason.contains("cargo config changed")
        }),
        ("workspace manifest", |reason| {
            reason.contains("workspace manifest changed")
        }),
    ];

    for ((name, apply), (_, accepts)) in cases.iter().zip(expected.iter()) {
        let fixture = workspace();
        let dir = fixture.path();
        apply(dir);
        let head = commit(dir, name);
        let state = tempfile::tempdir().expect("state dir");
        seed_state(state.path(), KEY, base_time());

        let selection = select(
            dir,
            &fixture.base,
            &head,
            &Policy::default(),
            state.path(),
            KEY,
            base_time(),
        )
        .expect("select");

        assert!(selection.full, "{name} must force a full run");
        assert!(
            accepts(&selection.reason.to_string()),
            "{name} reported an unexpected reason: {}",
            selection.reason
        );
    }
}

#[test]
fn unowned_path_selects_full() {
    let fixture = workspace();
    let dir = fixture.path();
    // A root-level file belongs to no workspace member in a virtual workspace.
    fs::write(dir.join("README.md"), "# docs\n").expect("unowned file");
    let head = commit(dir, "unowned file");
    let state = tempfile::tempdir().expect("state dir");
    seed_state(state.path(), KEY, base_time());

    let selection = select(
        dir,
        &fixture.base,
        &head,
        &Policy::default(),
        state.path(),
        KEY,
        base_time(),
    )
    .expect("select");

    assert!(selection.full);
    assert_eq!(
        selection.reason,
        Reason::UnownedPath {
            path: "README.md".to_owned()
        }
    );
}

#[test]
fn periodic_full_by_count_and_age() {
    let fixture = workspace();
    let dir = fixture.path();
    fs::write(
        dir.join("leaf/src/lib.rs"),
        "pub fn leaf() { let _ = 1; }\n",
    )
    .expect("edit leaf");
    let head = commit(dir, "change leaf");
    let policy = Policy::default();

    // Nine runs after a full run are incremental; the tenth is forced full.
    let count_state = tempfile::tempdir().expect("count state");
    seed_state(count_state.path(), KEY, base_time());
    for run in 1..policy.full_every {
        let now = base_time() + Duration::from_secs(u64::from(run));
        let selection = select(
            dir,
            &fixture.base,
            &head,
            &policy,
            count_state.path(),
            KEY,
            now,
        )
        .expect("select");
        assert!(!selection.full, "run {run} should be incremental");
    }
    let due = select(
        dir,
        &fixture.base,
        &head,
        &policy,
        count_state.path(),
        KEY,
        base_time() + Duration::from_secs(u64::from(policy.full_every)),
    )
    .expect("select");
    assert!(due.full);
    assert_eq!(
        due.reason,
        Reason::RunCount {
            runs: u64::from(policy.full_every),
            limit: policy.full_every,
        }
    );

    // Age is independent of the counter: one run after the interval is full.
    let age_state = tempfile::tempdir().expect("age state");
    seed_state(age_state.path(), KEY, base_time());
    let aged = select(
        dir,
        &fixture.base,
        &head,
        &policy,
        age_state.path(),
        KEY,
        base_time() + policy.full_after + Duration::from_secs(1),
    )
    .expect("select");
    assert!(aged.full);
    assert_eq!(
        aged.reason,
        Reason::Age {
            limit_secs: policy.full_after.as_secs()
        }
    );
}

#[test]
fn selection_is_deterministic() {
    let fixture = workspace();
    let dir = fixture.path();
    fs::write(
        dir.join("leaf/src/lib.rs"),
        "pub fn leaf() { let _ = 1; }\n",
    )
    .expect("edit leaf");
    let head = commit(dir, "change leaf");

    // Identical inputs, including identical seeded state, give identical output.
    let mut results = Vec::new();
    for _ in 0..3 {
        let state = tempfile::tempdir().expect("state dir");
        seed_state(state.path(), KEY, base_time());
        results.push(
            select(
                dir,
                &fixture.base,
                &head,
                &Policy::default(),
                state.path(),
                KEY,
                base_time(),
            )
            .expect("select"),
        );
    }
    assert_eq!(results[0], results[1]);
    assert_eq!(results[1], results[2]);
}

#[test]
fn global_paths_select_full() {
    let fixture = workspace();
    let dir = fixture.path();
    fs::create_dir_all(dir.join("ci")).expect("ci dir");
    fs::write(dir.join("ci/pipeline.yml"), "steps: []\n").expect("global file");
    let head = commit(dir, "ci change");
    let state = tempfile::tempdir().expect("state dir");
    seed_state(state.path(), KEY, base_time());
    let policy = Policy {
        global_paths: vec!["ci/**".to_owned()],
        ..Policy::default()
    };

    let selection = select(
        dir,
        &fixture.base,
        &head,
        &policy,
        state.path(),
        KEY,
        base_time(),
    )
    .expect("select");

    assert!(selection.full);
    assert_eq!(
        selection.reason,
        Reason::GlobalPath {
            path: "ci/pipeline.yml".to_owned()
        }
    );
}

#[test]
fn corrupt_state_means_full() {
    let fixture = workspace();
    let dir = fixture.path();
    fs::write(
        dir.join("leaf/src/lib.rs"),
        "pub fn leaf() { let _ = 1; }\n",
    )
    .expect("edit leaf");
    let head = commit(dir, "change leaf");
    let state = tempfile::tempdir().expect("state dir");
    let state_file = state.path().join("select").join(format!("{KEY}.json"));
    fs::create_dir_all(state_file.parent().expect("state parent")).expect("state dir");
    fs::write(&state_file, b"not json").expect("corrupt state");

    let selection = select(
        dir,
        &fixture.base,
        &head,
        &Policy::default(),
        state.path(),
        KEY,
        base_time(),
    )
    .expect("select must not refuse on corrupt state");

    assert!(selection.full);
    assert!(
        matches!(selection.reason, Reason::StateUnreadable { .. }),
        "unexpected reason: {}",
        selection.reason
    );
    // The bad file is repaired, so a later call sees a well-formed state.
    let repaired = fs::read(&state_file).expect("repaired state");
    serde_json::from_slice::<serde_json::Value>(&repaired).expect("valid state json");

    // The base commit is still reachable and the fixture git is intact.
    assert!(
        !text(&git(dir, &["rev-parse", &fixture.base]))
            .trim()
            .is_empty()
    );
}
