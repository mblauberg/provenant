//! Installed-release fixtures built through the real package and install APIs.
#![allow(dead_code)]

use provenant_config::{
    Harness, InstallContext, InstallReporter, Release, ReleaseSmoke, Snapshots, Targets, install,
    package_release, plan_install,
};
use provenant_core::{CancelToken, Clock, PathResolver, RootInputs, WaitBudget};
use provenant_records::{SourceDigest, SourceInput, VersionInfo, source_digest};
use provenant_supervisor::{clock::SystemClock, fs::LocalFs};
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub const BINARY: &str = env!("CARGO_BIN_EXE_provenant");
const SOURCE_DIGEST: SourceDigest = SourceDigest::from_bytes([0x11; 32]);

/// Prints the selected executable and arguments, then exits with the requested status.
const RECORDER: &str = r##"#!/bin/sh
printf 'argv0=%s\n' "$0"
for a in "$@"; do printf 'arg=%s\n' "$a"; done
printf 'cwd=%s\n' "$(pwd -P)"
printf 'pid=%s\n' "$$"
printf 'gitdir=%s\n' "${GIT_DIR-<unset>}"
printf 'gitname=%s\n' "${GIT_AUTHOR_NAME-<unset>}"
exit "${PROVENANT_TEST_EXIT:-0}"
"##;

const PYTHON_RECORDER: &str = r##"#!/usr/bin/env python3
import os
import sys

print("argv0=" + sys.argv[0])
for arg in sys.argv[1:]:
    print("arg=" + arg)
print("cwd=" + os.getcwd())
print("pid=" + str(os.getpid()))
print("gitdir=" + os.environ.get("GIT_DIR", "<unset>"))
print("gitname=" + os.environ.get("GIT_AUTHOR_NAME", "<unset>"))
sys.exit(int(os.environ.get("PROVENANT_TEST_EXIT", "0")))
"##;

pub const OWNERS: &[&str] = &[
    "runtime/fabric/bin/fabric-mcp",
    "runtime/fabric/bin/fabric",
    "scripts/model-route",
    "scripts/worktree",
    "scripts/clean.py",
    "scripts/reap.py",
    "scripts/utility.py",
    "scripts/provenant_lint.py",
    "scripts/check-harness",
    "scripts/fabric-hosts",
    "scripts/gate",
    "scripts/lib/harness-python.sh",
    "scripts/provenant-pr",
    "scripts/provenant-profile",
    "scripts/provenant-quota",
    "skills/orchestrate/scripts/dispatch_run.py",
    "skills/orchestrate/scripts/batch_run.py",
    "skills/orchestrate/scripts/run_controls.py",
];

struct Resolver;
impl PathResolver for Resolver {
    fn resolve_existing_ancestor(&self, path: &Path) -> io::Result<PathBuf> {
        Ok(path.into())
    }
}

struct Smoke(VersionInfo);
impl ReleaseSmoke for Smoke {
    fn check(&self, _: &Release, _: &CancelToken) -> io::Result<VersionInfo> {
        Ok(self.0.clone())
    }
}

struct Reporter;
impl InstallReporter for Reporter {
    fn preview(&mut self, _: &[provenant_config::Action]) -> io::Result<()> {
        Ok(())
    }
}

pub struct Sandbox {
    pub dir: PathBuf,
}

impl Sandbox {
    pub fn new(label: &str) -> Self {
        let temp = if cfg!(target_os = "macos") {
            PathBuf::from("/private/tmp")
        } else {
            std::env::temp_dir()
        };
        let dir = fs::canonicalize(temp)
            .unwrap()
            .join(format!("fd-it-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("home")).unwrap();
        Sandbox { dir }
    }

    pub fn home(&self) -> PathBuf {
        self.dir.join("home")
    }

    /// A staged release fixture. `install_binary` packages and installs it.
    pub fn release(&self, name: &str) -> PathBuf {
        let root = self.dir.join(name);
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(root.join("bin/provenant"), fs::read(BINARY).unwrap()).unwrap();
        fs::set_permissions(
            root.join("bin/provenant"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        for owner in OWNERS {
            let contents = if owner.ends_with(".py") {
                PYTHON_RECORDER
            } else if *owner == "scripts/lib/harness-python.sh" {
                include_str!("../../../../scripts/lib/harness-python.sh")
            } else {
                RECORDER
            };
            write_executable(&root.join(owner), contents);
        }
        assert!(!root.join(".venv").exists());
        assert!(!root.join(".git").exists());
        root
    }

    /// Packages the staged tree, then installs that release into isolated roots.
    pub fn install_binary(&self, source: &Path) -> PathBuf {
        let files: Vec<_> = OWNERS
            .iter()
            .map(|name| {
                let path = source.join(name);
                let bytes = fs::read(&path).unwrap();
                let executable = fs::metadata(&path).unwrap().permissions().mode() & 0o111 != 0;
                let mut encoded = vec![u8::from(executable)];
                encoded.extend_from_slice(&bytes);
                (name.to_string(), encoded)
            })
            .collect();
        let inputs: Vec<_> = files
            .iter()
            .map(|(name, bytes)| SourceInput { name, bytes })
            .collect();
        let version = VersionInfo {
            source_digest: SOURCE_DIGEST,
            asset_digest: source_digest(&inputs).unwrap(),
            target: "fixture-target".into(),
            toolchain: "fixture-toolchain".into(),
            record_schema_version: 1,
            config_schema_version: 1,
        };
        let release = package_release(
            source,
            &version,
            &LocalFs,
            &Smoke(version.clone()),
            &CancelToken::new(),
        )
        .unwrap();

        let home = self.home();
        let mut roots = RootInputs::new(home.clone(), self.dir.join("checkout"));
        roots.environment_state_dir = Some(self.dir.join("state"));
        let roots = roots.resolve(&Resolver).unwrap();
        let targets = Targets::new(roots, &home, &[Harness::Codex]);
        let snapshots = Snapshots::capture(&release, &targets, &LocalFs).unwrap();
        let plan = plan_install(release, targets.clone(), snapshots).unwrap();
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let context = InstallContext {
            fs: &LocalFs,
            clock: &clock,
            budget: WaitBudget::cli(clock.mono_now(), 1).unwrap(),
            cancel: &cancel,
        };
        install(&plan, &context, &mut Reporter, &Smoke(version), false).unwrap();
        let installed = self.installed_release();
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(installed.join("release.json")).unwrap()).unwrap();
        assert_eq!(manifest["schema_version"], 1);
        self.home().join(".local/bin/provenant")
    }

    pub fn installed_release(&self) -> PathBuf {
        self.dir
            .join("state/releases")
            .join(SOURCE_DIGEST.to_string())
    }

    /// A caller checkout with the two Provenant markers used by `check`.
    pub fn provenant_checkout(&self, name: &str) -> PathBuf {
        let root = self.dir.join(name);
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::write(root.join("HARNESS.md"), "# Test harness\n").unwrap();
        write_executable(&root.join("scripts/check-harness"), RECORDER);
        let git = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(git.status.success(), "{}", text(&git.stderr));
        root
    }

    /// A Git checkout without the Provenant marker files.
    pub fn other_git_checkout(&self) -> PathBuf {
        let root = self.dir.join("other-git");
        fs::create_dir_all(&root).unwrap();
        let git = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(git.status.success(), "{}", text(&git.stderr));
        root
    }

    /// A build binary without adjacent release metadata.
    pub fn uninstalled_binary(&self, name: &str) -> PathBuf {
        let target = self.dir.join(name).join("bin/provenant");
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

pub fn harness_python() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.venv/bin/python")
        .canonicalize()
        .expect("harness Python environment is installed")
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
