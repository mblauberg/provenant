//! End-to-end exec and installed-root behaviour of the built binary.

#![allow(
    clippy::disallowed_methods,
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used
)]
mod common;

use common::*;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;

fn real(path: &std::path::Path) -> String {
    fs::canonicalize(path).unwrap().display().to_string()
}

#[test]
fn installed_binary_resolves_its_release_and_routes_owner_and_check() {
    let sandbox = Sandbox::new("installed-release");
    let release = sandbox.release("release");
    let other = sandbox.release("other-release");
    let binary = sandbox.install_binary(&release);
    let installed_release = sandbox.installed_release();
    let caller = sandbox.provenant_checkout("caller");

    let root = command(&binary, &sandbox.home())
        .env("PROVENANT_ROOT", &other)
        .arg("root")
        .output()
        .unwrap();
    assert_eq!(root.status.code(), Some(0), "{}", stderr(&root));
    assert_eq!(stdout(&root), format!("{}\n", real(&installed_release)));

    let owner = command(&binary, &sandbox.home())
        .env("HARNESS_PYTHON", harness_python())
        .env("PROVENANT_ROOT", &other)
        .args(["gate", "--owner-arg"])
        .output()
        .unwrap();
    assert_eq!(owner.status.code(), Some(0), "{}", stderr(&owner));
    assert_eq!(
        field(&stdout(&owner), "argv0"),
        format!("{}/scripts/gate", real(&installed_release))
    );
    assert_eq!(args(&stdout(&owner)), ["--owner-arg"]);

    let check = command(&binary, &sandbox.home())
        .current_dir(&caller)
        .env("GIT_DIR", "/elsewhere/.git")
        .env("GIT_AUTHOR_NAME", "retained")
        .args(["check", "--check-arg"])
        .output()
        .unwrap();
    assert_eq!(check.status.code(), Some(0), "{}", stderr(&check));
    assert_eq!(
        field(&stdout(&check), "argv0"),
        format!("{}/scripts/check-harness", real(&caller))
    );
    assert_eq!(field(&stdout(&check), "gitdir"), "<unset>");
    assert_eq!(field(&stdout(&check), "gitname"), "retained");
    assert_eq!(args(&stdout(&check)), ["--check-arg"]);
}

#[test]
fn check_rejects_a_git_root_without_provenant_checkout_markers_as_usage() {
    let sandbox = Sandbox::new("check-not-provenant");
    let release = sandbox.release("release");
    let binary = sandbox.install_binary(&release);
    let caller = sandbox.other_git_checkout();
    let output = command(&binary, &sandbox.home())
        .current_dir(&caller)
        .arg("check")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("requires a Provenant Git checkout"));
}

#[test]
fn installed_release_python_routes_are_unavailable_without_an_owner() {
    let sandbox = Sandbox::new("python-unavailable");
    let release = sandbox.release("release");
    let binary = sandbox.install_binary(&release);
    let installed_release = sandbox.installed_release();
    assert!(installed_release.join("release.json").is_file());
    assert!(!installed_release.join(".venv").exists());
    assert!(!installed_release.join(".git").exists());

    let cases: &[&[&str]] = &[
        &["digest"],
        &["worktree"],
        &["mcp"],
        &["lease", "acquire", "seat-1"],
        &[],
    ];
    for (index, arguments) in cases.iter().enumerate() {
        let mut route = command(&binary, &sandbox.home());
        route.args(*arguments);
        if arguments.is_empty() {
            route.env("AGENT_FABRIC_SEAT", "chair");
        }
        let output = route.output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(3),
            "route {index}: {}",
            stderr(&output)
        );
        assert!(stdout(&output).is_empty(), "route {index}");
        assert!(
            stderr(&output).contains("owner unavailable in installed release"),
            "route {index}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn installed_release_python_owner_accepts_an_explicit_interpreter() {
    let sandbox = Sandbox::new("python-explicit");
    let release = sandbox.release("release");
    let binary = sandbox.install_binary(&release);
    let output = command(&binary, &sandbox.home())
        .env("HARNESS_PYTHON", harness_python())
        .args(["digest", "fixture-arg"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        field(&stdout(&output), "argv0"),
        format!("{}/scripts/utility.py", real(&sandbox.installed_release()))
    );
    assert_eq!(args(&stdout(&output)), ["digest", "fixture-arg"]);
}

#[test]
fn an_uninstalled_binary_requires_and_uses_explicit_provenant_root() {
    let sandbox = Sandbox::new("uninstalled");
    let release = sandbox.release("release");
    let binary = sandbox.uninstalled_binary("build");
    let _installed_binary = sandbox.install_binary(&release);
    let installed_release = sandbox.installed_release();

    let missing = command(&binary, &sandbox.home())
        .env("HARNESS_PYTHON", harness_python())
        .arg("root")
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(3));
    assert!(stdout(&missing).is_empty());
    assert!(stderr(&missing).contains("uninstalled binary requires an explicit PROVENANT_ROOT"));

    let selected = command(&binary, &sandbox.home())
        .env("HARNESS_PYTHON", harness_python())
        .env("PROVENANT_ROOT", &installed_release)
        .arg("root")
        .output()
        .unwrap();
    assert_eq!(selected.status.code(), Some(0), "{}", stderr(&selected));
    assert_eq!(stdout(&selected), format!("{}\n", real(&installed_release)));

    let owner = command(&binary, &sandbox.home())
        .env("HARNESS_PYTHON", harness_python())
        .env("PROVENANT_ROOT", &installed_release)
        .arg("gate")
        .output()
        .unwrap();
    assert_eq!(owner.status.code(), Some(0), "{}", stderr(&owner));
    assert_eq!(
        field(&stdout(&owner), "argv0"),
        format!("{}/scripts/gate", real(&installed_release))
    );
}

#[test]
fn stdin_reaches_the_owner_unread() {
    let sandbox = Sandbox::new("stdin");
    let release = sandbox.release("release");
    write_executable(&release.join("scripts/fabric-hosts"), "#!/bin/sh\ncat\n");
    let binary = sandbox.install_binary(&release);
    let payload: Vec<u8> = (0..=255u8).cycle().take(300_000).collect();
    let mut child = command(&binary, &sandbox.home())
        .env("HARNESS_PYTHON", harness_python())
        .arg("peer")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn({
        let payload = payload.clone();
        move || std::io::Write::write_all(&mut stdin, &payload).unwrap()
    });
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap();
    assert_eq!(output.stdout, payload);
}

#[test]
fn non_utf8_arguments_and_environment_arrive_byte_for_byte() {
    let sandbox = Sandbox::new("bytes");
    let release = sandbox.release("release");
    write_executable(
        &release.join("scripts/gate"),
        "#!/bin/sh\nprintf '%s|%s' \"$1\" \"$ODD\"\n",
    );
    let binary = sandbox.install_binary(&release);
    let odd = std::ffi::OsStr::from_bytes(b"a\xffb\n");
    let output = command(&binary, &sandbox.home())
        .env("HARNESS_PYTHON", harness_python())
        .env("ODD", odd)
        .arg("gate")
        .arg(odd)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(output.stdout, b"a\xffb\n|a\xffb\n");
}

#[test]
fn exec_failures_exit_3_without_a_panic() {
    let sandbox = Sandbox::new("execfail");
    let release = sandbox.release("release");
    fs::set_permissions(
        release.join("scripts/gate"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    write_executable(
        &release.join("scripts/fabric-hosts"),
        "#!/no/such/interpreter\n",
    );
    let binary = sandbox.install_binary(&release);
    for (verb, why) in [
        ("gate", "Permission denied"),
        ("peer", "No such file or directory"),
    ] {
        let output = command(&binary, &sandbox.home())
            .env("HARNESS_PYTHON", harness_python())
            .arg(verb)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(3), "{verb}");
        let err = stderr(&output);
        assert!(err.starts_with("provenant: cannot execute "), "{err}");
        assert!(err.contains(&format!(": {why}; repair: ")), "{err}");
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn lease_maps_to_landing_verbs_and_passes_exit_3_through() {
    let sandbox = Sandbox::new("lease");
    let release = sandbox.release("release");
    let binary = sandbox.install_binary(&release);
    let output = command(&binary, &sandbox.home())
        .env("HARNESS_PYTHON", harness_python())
        .env("PROVENANT_TEST_EXIT", "3")
        .args(["lease", "acquire", "s1", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(args(&stdout(&output)), ["landing-acquire", "s1", "--json"]);
}

#[test]
fn a_shebangless_owner_is_not_run_through_a_shell() {
    let sandbox = Sandbox::new("enoexec");
    let release = sandbox.release("release");
    write_executable(
        &release.join("scripts/model-route"),
        "echo ran-through-a-shell\n",
    );
    let binary = sandbox.install_binary(&release);
    let output = command(&binary, &sandbox.home())
        .env("HARNESS_PYTHON", harness_python())
        .arg("route")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty(), "{}", stdout(&output));
    assert!(stderr(&output).starts_with("provenant: cannot execute "));
    assert!(stderr(&output).contains("Exec format error"));
}
