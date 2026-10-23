//! Fixture checkouts with recording owners, built from std only.
#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub const BINARY: &str = env!("CARGO_BIN_EXE_provenant");

/// Prints what an owner was given, then exits with `PROVENANT_TEST_EXIT`.
const RECORDER: &str = r#"#!/bin/sh
printf 'argv0=%s\n' "$0"
for a in "$@"; do printf 'arg=%s\n' "$a"; done
printf 'cwd=%s\n' "$(pwd -P)"
printf 'pid=%s\n' "$$"
printf 'root=%s\n' "${AGENT_FABRIC_PRODUCT_ROOT-<unset>}"
printf 'home=%s\n' "${AGENTS_HOME-<unset>}"
printf 'instance=%s\n' "${AGENT_FABRIC_INSTANCE_ROOT-<unset>}"
printf 'gitdir=%s\n' "${GIT_DIR-<unset>}"
printf 'gitname=%s\n' "${GIT_AUTHOR_NAME-<unset>}"
printf 'stale=%s\n' "${PROVENANT_STALE_CHECKED-<unset>}"
exit "${PROVENANT_TEST_EXIT:-0}"
"#;

const PYTHON_RECORDER: &str = r#"#!/usr/bin/env python3
import os
import sys

print("argv0=" + sys.argv[0])
for arg in sys.argv[1:]:
    print("arg=" + arg)
print("cwd=" + os.getcwd())
print("pid=" + str(os.getpid()))
for field, name in [("root", "AGENT_FABRIC_PRODUCT_ROOT"), ("home", "AGENTS_HOME"),
                    ("instance", "AGENT_FABRIC_INSTANCE_ROOT"), ("gitdir", "GIT_DIR"),
                    ("gitname", "GIT_AUTHOR_NAME"), ("stale", "PROVENANT_STALE_CHECKED")]:
    print(field + "=" + os.environ.get(name, "<unset>"))
sys.exit(int(os.environ.get("PROVENANT_TEST_EXIT", "0")))
"#;

pub const OWNERS: &[&str] = &[
    "runtime/fabric/bin/fabric-mcp",
    "runtime/fabric/bin/fabric",
    "scripts/model-route",
    "scripts/worktree",
    "scripts/clean.py",
    "scripts/provenant_lint.py",
    "runtime/fabric/src/landing-cli.ts",
    "scripts/check-harness",
    "scripts/gate",
    "scripts/fabric-hosts",
    "scripts/provenant-telemetry",
    "scripts/provenant-quota",
    "scripts/provenant-profile",
    "skills/orchestrate/scripts/capabilities.py",
    "skills/orchestrate/scripts/dispatch_run.py",
    "skills/orchestrate/scripts/batch_run.py",
    "skills/orchestrate/scripts/run_controls.py",
];

pub struct Sandbox {
    pub dir: PathBuf,
}

impl Sandbox {
    pub fn new(label: &str) -> Self {
        let dir = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("fd-it-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("home")).unwrap();
        Sandbox { dir }
    }

    pub fn home(&self) -> PathBuf {
        self.dir.join("home")
    }

    /// A checkout whose owners all record their invocation.
    pub fn checkout(&self, name: &str) -> PathBuf {
        let root = self.dir.join(name);
        for owner in OWNERS {
            let recorder = if owner.ends_with(".py") {
                PYTHON_RECORDER
            } else {
                RECORDER
            };
            write_executable(&root.join(owner), recorder);
        }
        write_executable(
            &root.join("scripts/lib/harness-python.sh"),
            include_str!("../../../../scripts/lib/harness-python.sh"),
        );
        fs::create_dir_all(root.join(".venv/bin")).unwrap();
        symlink(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.venv/bin/python"),
            root.join(".venv/bin/python"),
        )
        .unwrap();
        root
    }

    /// The binary copied to `<dir>/<relative>`, mode 0755.
    pub fn install_binary(&self, relative: &str) -> PathBuf {
        let target = self.dir.join(relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(BINARY, &target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        target
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

pub fn write_executable(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A command with a minimal environment plus its caller's writable temp root.
pub fn command(binary: &Path, home: &Path) -> Command {
    let mut command = Command::new(binary);
    let temp = if cfg!(target_os = "macos") {
        PathBuf::from("/private/tmp")
    } else {
        std::env::temp_dir()
    };
    let python = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.venv/bin")
        .canonicalize()
        .expect("harness Python environment is installed");
    command
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", python.display()))
        .env("HOME", home)
        .env("TMPDIR", temp);
    command
}

pub fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

pub fn stdout(output: &Output) -> String {
    text(&output.stdout)
}

pub fn stderr(output: &Output) -> String {
    text(&output.stderr)
}

/// Value of `key=` in recorder output.
pub fn field(output: &str, key: &str) -> String {
    output
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no {key} in {output}"))
        .to_string()
}

pub fn args(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| line.strip_prefix("arg="))
        .map(String::from)
        .collect()
}
