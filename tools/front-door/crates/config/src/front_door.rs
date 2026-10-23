//! Front-door source identity and build policy; execution belongs to supervisor.
use crate::{Result, invalid, io, pending};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub const CRATE_RELATIVE: &str = "tools/front-door";
pub const HASHED_FILES: [&str; 4] = [
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo/config.toml",
];
pub const RUSTUP_FIX: &str = "fix: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh";
pub const DEPLOYMENT_TARGET: &str = "15.0";
pub const SMOKE_TIMEOUT_SECONDS: u64 = 30;
const IDENTITY: &[u8] = b"provenant-front-door source-sha256=";

fn files(directory: &Path, result: &mut Vec<PathBuf>, skip_build_directories: bool) -> Result<()> {
    for entry in fs::read_dir(directory).map_err(|e| io(directory, e))? {
        let path = entry.map_err(|e| io(directory, e))?.path();
        if path.is_file() {
            result.push(path);
        } else if path.is_dir()
            && !path.is_symlink()
            && (!skip_build_directories
                || !matches!(
                    path.file_name().and_then(|s| s.to_str()),
                    Some("target" | ".git")
                ))
        {
            files(&path, result, skip_build_directories)?;
        }
    }
    Ok(())
}

fn editor_litter(relative: &Path) -> bool {
    relative.components().any(|part| {
        let part = part.as_os_str().to_string_lossy();
        part.starts_with('.')
    }) || relative
        .file_name()
        .is_some_and(|name| name.to_string_lossy().ends_with('~'))
}

/// R2 extends the original framing to every workspace member and build input.
/// Target directories and documentation are deliberately outside that identity.
pub fn source_digest(krate: &Path) -> Result<String> {
    if !krate.join("Cargo.toml").is_file() || !krate.join("src").is_dir() {
        return Err(invalid(format!(
            "no Rust front door sources under {}",
            krate.display()
        )));
    }
    let mut names = HASHED_FILES
        .iter()
        .map(|s| krate.join(s))
        .filter(|p| p.is_file())
        .collect::<Vec<_>>();
    let mut rest = std::collections::BTreeMap::<String, PathBuf>::new();
    let mut packages = vec![krate.to_path_buf()];
    let members = krate.join("crates");
    if members.is_dir() {
        for entry in fs::read_dir(&members).map_err(|e| io(&members, e))? {
            let member = entry.map_err(|e| io(&members, e))?.path();
            if !member.join("Cargo.toml").is_file() {
                continue;
            }
            packages.push(member);
        }
    }
    for package in packages {
        for relative in [Path::new("Cargo.toml"), Path::new("build.rs")] {
            let path = package.join(relative);
            if path.is_file() {
                let name = path
                    .strip_prefix(krate)
                    .map_err(|e| invalid(e.to_string()))?
                    .to_str()
                    .ok_or_else(|| invalid("source digest path is not UTF-8"))?
                    .to_owned();
                rest.insert(name, path);
            }
        }
        if package.join("src").is_dir() {
            let mut source_files = vec![];
            files(&package.join("src"), &mut source_files, false)?;
            for path in source_files {
                let relative = path
                    .strip_prefix(package.join("src"))
                    .map_err(|e| invalid(e.to_string()))?;
                if editor_litter(relative) {
                    continue;
                }
                let name = path
                    .strip_prefix(krate)
                    .map_err(|e| invalid(e.to_string()))?
                    .to_str()
                    .ok_or_else(|| invalid("source digest path is not UTF-8"))?
                    .to_owned();
                rest.insert(name, path);
            }
        }
        let mut rust_files = vec![];
        files(&package, &mut rust_files, true)?;
        for path in rust_files.into_iter().filter(|path| {
            path.file_name()
                .is_some_and(|n| n.as_encoded_bytes().ends_with(b".rs"))
        }) {
            let relative = path
                .strip_prefix(&package)
                .map_err(|e| invalid(e.to_string()))?;
            if relative
                .components()
                .any(|part| matches!(part.as_os_str().to_str(), Some("target" | ".git")))
            {
                continue;
            }
            let name = path
                .strip_prefix(krate)
                .map_err(|e| invalid(e.to_string()))?
                .to_str()
                .ok_or_else(|| invalid("source digest path is not UTF-8"))?
                .to_owned();
            rest.insert(name, path);
        }
    }
    let hashed_names: std::collections::BTreeSet<_> = names
        .iter()
        .map(|path| {
            path.strip_prefix(krate)
                .map_err(|e| invalid(e.to_string()))?
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid("source digest path is not UTF-8"))
        })
        .collect::<Result<_>>()?;
    names.extend(
        rest.into_iter()
            .filter(|(name, _)| !hashed_names.contains(name))
            .map(|(_, path)| path),
    );
    let mut digest = Sha256::new();
    for path in names {
        let name = path
            .strip_prefix(krate)
            .map_err(|e| invalid(e.to_string()))?
            .to_str()
            .ok_or_else(|| invalid("source digest path is not UTF-8"))?;
        let bytes = fs::read(&path).map_err(|e| io(&path, e))?;
        digest.update(format!("{name}\0{}\0", bytes.len()).as_bytes());
        digest.update(bytes);
    }
    Ok(pending::hex(&digest.finalize()))
}

pub fn embedded_digest(bytes: &[u8]) -> Option<&str> {
    for line in bytes.split(|b| *b == b'\n') {
        let Some(stamped) = line.strip_prefix(IDENTITY) else {
            continue;
        };
        if stamped == b"unknown"
            || (stamped.len() == 64
                && stamped
                    .iter()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b)))
        {
            return std::str::from_utf8(stamped).ok();
        }
    }
    None
}

/// Identity validation precedes the separately supervised smoke run.
pub fn validate_prebuilt(binary: &Path, digest: &str) -> Result<()> {
    if !binary.is_absolute() || binary.is_symlink() || !binary.is_file() {
        return Err(invalid(format!(
            "PROVENANT_FRONT_DOOR_BINARY must be an absolute regular file: {}",
            binary.display()
        )));
    }
    let bytes = fs::read(binary).map_err(|e| {
        invalid(format!(
            "cannot read prebuilt front door {}: {}",
            binary.display(),
            io(binary, e)
        ))
    })?;
    let stamped = embedded_digest(&bytes);
    if stamped != Some(digest) {
        return Err(invalid(format!(
            "prebuilt front door {} was built from different sources (embeds {}, checkout is {digest}); rebuild it from this checkout",
            binary.display(),
            stamped.unwrap_or("None")
        )));
    }
    Ok(())
}

pub fn validate_mode(mode: &str) -> Result<()> {
    if mode != "rust" {
        return Err(invalid(format!(
            "PROVENANT_FRONT_DOOR must be rust, got {}",
            provenant_core::py::python_repr(mode)
        )));
    }
    Ok(())
}

/// Executable lookup capability, including ACLs and real process credentials.
/// Mode bits alone are not an X_OK check.
pub trait ExecutableAccess {
    fn executable(&self, path: &Path) -> bool;
}

/// Production executable access through the supervisor's safe syscall wrapper.
pub struct SystemExecutableAccess;

impl ExecutableAccess for SystemExecutableAccess {
    fn executable(&self, path: &Path) -> bool {
        provenant_supervisor::sys::is_executable(path)
    }
}

pub fn cargo_path(path: &std::ffi::OsStr, home: &Path) -> Option<PathBuf> {
    cargo_path_with_access(path, home, &SystemExecutableAccess)
}

pub fn cargo_path_with_access(
    path: &std::ffi::OsStr,
    home: &Path,
    access: &impl ExecutableAccess,
) -> Option<PathBuf> {
    for directory in std::env::split_paths(path) {
        let candidate = directory.join("cargo");
        if candidate.is_file() && access.executable(&candidate) {
            return Some(candidate);
        }
    }
    let fallback = home.join(".cargo/bin/cargo");
    (fallback.is_file() && access.executable(&fallback)).then_some(fallback)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildPlan {
    pub command: Vec<PathBuf>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, std::ffi::OsString>,
    pub binary: PathBuf,
}

/// The command runner must set stdin to null and route cargo stdout to stderr.
/// It must apply the workspace toolchain pin through this working directory.
pub fn build_plan(
    krate: &Path,
    target: &Path,
    digest: &str,
    cargo: &Path,
    mut environment: BTreeMap<String, std::ffi::OsString>,
    darwin: bool,
) -> Result<BuildPlan> {
    let krate = crate::resolve(krate)?;
    let target = crate::resolve(target)?;
    environment.insert("CARGO_TARGET_DIR".into(), target.as_os_str().into());
    environment.insert("PROVENANT_FD_SOURCE_SHA256".into(), digest.into());
    if darwin {
        environment.insert("MACOSX_DEPLOYMENT_TARGET".into(), DEPLOYMENT_TARGET.into());
    }
    Ok(BuildPlan {
        command: vec![
            cargo.into(),
            "build".into(),
            "--release".into(),
            "--locked".into(),
            "--manifest-path".into(),
            krate.join("Cargo.toml"),
        ],
        cwd: krate,
        environment,
        binary: target.join("release/provenant"),
    })
}

/// Only these two environment names cross the smoke-test boundary.
pub fn smoke_environment(
    path: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> BTreeMap<String, std::ffi::OsString> {
    BTreeMap::from([
        (
            "PATH".into(),
            path.unwrap_or(std::ffi::OsStr::new("/usr/bin:/bin")).into(),
        ),
        (
            "HOME".into(),
            home.unwrap_or(std::ffi::OsStr::new("/")).into(),
        ),
    ])
}

pub fn validate_smoke(binary: &Path, returncode: i32, stdout: &[u8]) -> Result<()> {
    if returncode != 0 || !stdout.starts_with(b"usage: provenant") {
        return Err(invalid(format!(
            "smoke run of {} failed: `help` exited {returncode}",
            binary.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("Cargo.toml"), "manifest").unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        dir
    }
    #[test]
    fn digest_covers_workspace_libraries_and_build_inputs() {
        let dir = fixture();
        let root = dir.path();
        fs::create_dir_all(root.join("crates/example/src")).unwrap();
        fs::write(root.join("crates/example/Cargo.toml"), "member").unwrap();
        fs::write(root.join("crates/example/src/lib.rs"), "library").unwrap();
        fs::write(root.join("Cargo.lock"), "lock").unwrap();
        fs::write(root.join("rust-toolchain.toml"), "pin").unwrap();
        fs::write(root.join("build.rs"), "build").unwrap();
        let base = source_digest(root).unwrap();
        for name in [
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            "src/main.rs",
            "build.rs",
            "crates/example/Cargo.toml",
            "crates/example/src/lib.rs",
        ] {
            let path = root.join(name);
            let original = fs::read(&path).unwrap();
            fs::write(&path, b"changed").unwrap();
            assert_ne!(source_digest(root).unwrap(), base, "{name}");
            fs::write(&path, original).unwrap();
            assert_eq!(source_digest(root).unwrap(), base);
        }
        fs::create_dir_all(root.join("target/release")).unwrap();
        fs::write(root.join("target/release/provenant"), "binary").unwrap();
        assert_eq!(source_digest(root).unwrap(), base);
    }

    #[test]
    fn workspace_digest_matches_python_for_all_source_classes() {
        let dir = fixture();
        let root = dir.path();
        fs::create_dir_all(root.join(".cargo")).unwrap();
        fs::write(root.join(".cargo/config.toml"), "config").unwrap();
        fs::write(root.join("build.rs"), "fn main() {}").unwrap();
        fs::create_dir_all(root.join("tests/nested")).unwrap();
        fs::write(root.join("tests/nested/case.rs"), "case").unwrap();
        fs::write(root.join("tests/notes.txt"), "ignored").unwrap();
        fs::write(root.join("tests/.rs"), "hidden Rust target").unwrap();
        fs::write(root.join("src/.rs"), "hidden Rust source").unwrap();
        fs::create_dir_all(root.join("crates/member/src")).unwrap();
        fs::write(root.join("crates/member/Cargo.toml"), "member manifest").unwrap();
        fs::write(root.join("crates/member/src/lib.rs"), "member source").unwrap();
        fs::create_dir_all(root.join("crates/a\\b/src")).unwrap();
        fs::write(
            root.join("crates/a\\b/Cargo.toml"),
            "backslash member manifest",
        )
        .unwrap();
        fs::write(
            root.join("crates/a\\b/src/lib.rs"),
            "backslash member source",
        )
        .unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::write(root.join("target/debug/noise.rs"), "excluded").unwrap();
        fs::write(root.join("src/.editor.rs"), "litter").unwrap();
        fs::write(root.join("src/backup.rs~"), "litter").unwrap();
        fs::create_dir_all(root.join("src/target/nested")).unwrap();
        fs::write(root.join("src/target/nested/kept.rs"), "source subtree").unwrap();
        fs::write(root.join("src/literal\\name.rs"), "literal backslash").unwrap();

        let Some(output) = crate::oracle::python(
            "scripts/lib/front_door_build.py",
            "print(m.source_digest(p))",
            root,
        ) else {
            return;
        };
        assert_eq!(
            source_digest(root).unwrap(),
            String::from_utf8(output).unwrap().trim()
        );
    }
    #[test]
    fn embedded_identity_is_a_whole_line() {
        let digest = "c".repeat(64);
        let line = format!("provenant-front-door source-sha256={digest}");
        assert_eq!(
            embedded_digest(format!("\0\n{line}\n\0").as_bytes()),
            Some(digest.as_str())
        );
        assert_eq!(embedded_digest(format!("prefix{line}\n").as_bytes()), None);
        assert_eq!(embedded_digest(format!("{line}\r\n").as_bytes()), None);
        assert_eq!(
            embedded_digest(b"provenant-front-door source-sha256=unknown"),
            Some("unknown")
        );
    }
    #[test]
    fn build_plan_pins_cwd_and_overrides_ambient_target() {
        let dir = fixture();
        let target = dir.path().join("explicit");
        let plan = build_plan(
            dir.path(),
            &target,
            "digest",
            Path::new("/cargo"),
            BTreeMap::from([("CARGO_TARGET_DIR".into(), "/ambient".into())]),
            true,
        )
        .unwrap();
        assert_eq!(plan.cwd, dir.path().canonicalize().unwrap());
        assert_eq!(
            plan.environment["CARGO_TARGET_DIR"],
            dir.path()
                .canonicalize()
                .unwrap()
                .join("explicit")
                .as_os_str()
        );
        assert_eq!(plan.environment["MACOSX_DEPLOYMENT_TARGET"], "15.0");
        assert_eq!(
            plan.command[1..4],
            [
                PathBuf::from("build"),
                "--release".into(),
                "--locked".into()
            ]
        );
    }
    #[test]
    fn prebuilt_requires_regular_absolute_matching_sources() {
        let dir = fixture();
        let digest = source_digest(dir.path()).unwrap();
        let binary = dir.path().join("binary");
        fs::write(
            &binary,
            format!("provenant-front-door source-sha256={digest}\n"),
        )
        .unwrap();
        validate_prebuilt(&binary, &digest).unwrap();
        assert!(validate_prebuilt(&binary, "stale").is_err());
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&binary, &link).unwrap();
        assert!(validate_prebuilt(&link, &digest).is_err());
        assert!(validate_prebuilt(Path::new("relative"), &digest).is_err());
    }
    #[test]
    fn smoke_requires_zero_exit_and_usage_and_scrubs_environment() {
        assert!(validate_smoke(Path::new("/binary"), 0, b"usage: provenant help").is_ok());
        assert!(validate_smoke(Path::new("/binary"), 3, b"usage: provenant").is_err());
        assert!(validate_smoke(Path::new("/binary"), 0, b"hello").is_err());
        assert_eq!(
            smoke_environment(None, None).keys().collect::<Vec<_>>(),
            [&"HOME".to_string(), &"PATH".to_string()]
        );
    }

    #[test]
    fn invalid_mode_errors_use_python_string_repr() {
        for mode in [
            "python",
            "a'b",
            "a\"b",
            "a\\b\n",
            "bad\u{2028}x",
            "bad\u{200b}x",
        ] {
            let dir = fixture();
            fs::write(dir.path().join("mode"), mode).unwrap();
            let Some(output) = crate::oracle::python(
                "scripts/lib/front_door_build.py",
                "try: m.select(p.parent, p.read_text(), p.parent/'target', cargo_available=False)\nexcept ValueError as e: print(str(e))",
                &dir.path().join("mode"),
            ) else {
                return;
            };
            assert_eq!(
                validate_mode(mode).unwrap_err().to_string().as_bytes(),
                output.strip_suffix(b"\n").unwrap_or(&output)
            );
        }
    }
    #[test]
    fn python_digest_differential_without_workspace_members() {
        let dir = fixture();
        let Some(output) = crate::oracle::python(
            "scripts/lib/front_door_build.py",
            "print(m.source_digest(p))",
            dir.path(),
        ) else {
            return;
        };
        assert_eq!(
            source_digest(dir.path()).unwrap(),
            String::from_utf8(output).unwrap().trim()
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selected {
    pub kind: &'static str,
    pub source: PathBuf,
    pub note: &'static str,
}
#[derive(Debug, Clone)]
pub struct SmokeOutput {
    pub returncode: i32,
    pub stdout: Vec<u8>,
}

/// Platform execution is injected at the same boundary as subprocess.run in
/// the source. Implementations null stdin, honor the smoke timeout, route cargo
/// progress to stderr and apply a bounded build deadline. No utility process is
/// spawned by this inert policy crate.
pub trait FrontDoorHost {
    fn environment(&self) -> BTreeMap<String, std::ffi::OsString>;
    fn cargo(&self) -> Option<PathBuf>;
    fn darwin(&self) -> bool;
    fn build(&mut self, plan: &BuildPlan) -> Result<i32>;
    fn smoke(
        &mut self,
        binary: &Path,
        environment: &BTreeMap<String, std::ffi::OsString>,
        timeout_seconds: u64,
    ) -> Result<SmokeOutput>;
}

pub fn smoke(binary: &Path, host: &mut impl FrontDoorHost) -> Result<()> {
    let environment = host.environment();
    let output = host.smoke(
        binary,
        &smoke_environment(
            environment.get("PATH").map(|s| s.as_os_str()),
            environment.get("HOME").map(|s| s.as_os_str()),
        ),
        SMOKE_TIMEOUT_SECONDS,
    )?;
    validate_smoke(binary, output.returncode, &output.stdout)
}
pub fn build(
    krate: &Path,
    target: &Path,
    digest: &str,
    host: &mut impl FrontDoorHost,
) -> Result<PathBuf> {
    let cargo = host.cargo().ok_or(crate::Error::CargoUnavailable)?;
    let plan = build_plan(
        krate,
        target,
        digest,
        &cargo,
        host.environment(),
        host.darwin(),
    )?;
    let returncode = host.build(&plan)?;
    if returncode != 0 || !plan.binary.is_file() {
        return Err(invalid(format!("cargo build failed (exit {returncode})")));
    }
    Ok(plan.binary)
}
pub fn select(
    product_root: &Path,
    mode: &str,
    build_directory: &Path,
    prebuilt: Option<&Path>,
    host: &mut impl FrontDoorHost,
) -> Result<Selected> {
    validate_mode(mode)?;
    let krate = product_root.join(CRATE_RELATIVE);
    if !krate.join("Cargo.toml").is_file() {
        return Err(invalid(format!(
            "no {CRATE_RELATIVE} crate in {}",
            product_root.display()
        )));
    }
    let digest = source_digest(&krate)?;
    if let Some(binary) = prebuilt {
        validate_prebuilt(binary, &digest)?;
        smoke(binary, host)?;
        return Ok(Selected {
            kind: "rust",
            source: binary.into(),
            note: "prebuilt",
        });
    }
    let binary = build(&krate, build_directory, &digest, host)?;
    smoke(&binary, host)?;
    Ok(Selected {
        kind: "rust",
        source: binary,
        note: "built",
    })
}

#[derive(Debug, clap::Parser)]
#[command(name = "front_door_build.py")]
pub struct Arguments {
    #[command(subcommand)]
    pub command: Command,
}
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    Digest {
        krate: PathBuf,
    },
    Embedded {
        binary: PathBuf,
    },
    Smoke {
        binary: PathBuf,
    },
    Build {
        krate: PathBuf,
        target_dir: PathBuf,
    },
    Select {
        #[arg(long)]
        product_root: PathBuf,
        #[arg(long)]
        mode: String,
        #[arg(long)]
        build_dir: PathBuf,
        #[arg(long)]
        prebuilt: Option<PathBuf>,
    },
}
pub fn run(command: &Command, host: &mut impl FrontDoorHost) -> crate::codex::CommandResult {
    let result = (|| -> Result<Option<String>> {
        Ok(match command {
            Command::Digest { krate } => Some(source_digest(krate)?),
            Command::Embedded { binary } => {
                let bytes = fs::read(binary).map_err(|e| io(binary, e))?;
                embedded_digest(&bytes).map(str::to_owned)
            }
            Command::Smoke { binary } => {
                smoke(binary, host)?;
                Some(String::new())
            }
            Command::Build { krate, target_dir } => {
                let digest = source_digest(krate)?;
                Some(pending::path_text(&build(
                    krate, target_dir, &digest, host,
                )?)?)
            }
            Command::Select {
                product_root,
                mode,
                build_dir,
                prebuilt,
            } => {
                let selected = select(product_root, mode, build_dir, prebuilt.as_deref(), host)?;
                Some(format!("{}\t{}", selected.kind, selected.source.display()))
            }
        })
    })();
    match result {
        Ok(Some(output)) => crate::codex::CommandResult {
            exit_code: 0,
            stdout: if output.is_empty() {
                output
            } else {
                format!("{output}\n")
            },
            stderr: String::new(),
        },
        Ok(None) => crate::codex::CommandResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: String::new(),
        },
        Err(error) => crate::codex::CommandResult {
            exit_code: 3,
            stdout: String::new(),
            stderr: format!(
                "front door: {error}\n{}",
                if matches!(error, crate::Error::CargoUnavailable) {
                    format!("{RUSTUP_FIX}\n")
                } else {
                    String::new()
                }
            ),
        },
    }
}

#[cfg(test)]
mod host_tests {
    use super::*;
    struct Access {
        allowed: Vec<PathBuf>,
    }
    impl ExecutableAccess for Access {
        fn executable(&self, path: &Path) -> bool {
            self.allowed.iter().any(|p| p == path)
        }
    }
    #[test]
    fn cargo_search_applies_execute_access_to_path_and_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("path");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("cargo"), "").unwrap();
        let fallback = dir.path().join(".cargo/bin/cargo");
        fs::create_dir_all(fallback.parent().unwrap()).unwrap();
        fs::write(&fallback, "").unwrap();
        assert_eq!(
            cargo_path_with_access(path.as_os_str(), dir.path(), &Access { allowed: vec![] }),
            None
        );
        assert_eq!(
            cargo_path_with_access(
                path.as_os_str(),
                dir.path(),
                &Access {
                    allowed: vec![fallback.clone()]
                }
            ),
            Some(fallback)
        );
        assert_eq!(
            cargo_path_with_access(
                path.as_os_str(),
                dir.path(),
                &Access {
                    allowed: vec![path.join("cargo")]
                }
            ),
            Some(path.join("cargo"))
        );
    }
    #[test]
    fn cargo_search_uses_real_execute_access_for_path_and_fallback() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("path");
        fs::create_dir(&path).unwrap();
        let candidate = path.join("cargo");
        let fallback = dir.path().join(".cargo/bin/cargo");
        fs::create_dir_all(fallback.parent().unwrap()).unwrap();
        fs::write(&candidate, "not executable").unwrap();
        fs::write(&fallback, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&fallback, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            cargo_path(path.as_os_str(), dir.path()),
            Some(fallback.clone())
        );
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            cargo_path(path.as_os_str(), dir.path()),
            Some(candidate.clone())
        );
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&fallback, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(cargo_path(path.as_os_str(), dir.path()), None);
    }

    struct Host {
        cargo: Option<PathBuf>,
        status: i32,
        help: Vec<u8>,
        builds: usize,
        smokes: usize,
    }
    impl FrontDoorHost for Host {
        fn environment(&self) -> BTreeMap<String, std::ffi::OsString> {
            BTreeMap::from([
                ("PATH".into(), "/bin".into()),
                ("HOME".into(), "/home".into()),
                ("SECRET".into(), "redacted".into()),
            ])
        }
        fn cargo(&self) -> Option<PathBuf> {
            self.cargo.clone()
        }
        fn darwin(&self) -> bool {
            true
        }
        fn build(&mut self, plan: &BuildPlan) -> Result<i32> {
            self.builds += 1;
            assert_eq!(plan.environment["MACOSX_DEPLOYMENT_TARGET"], "15.0");
            fs::create_dir_all(plan.binary.parent().unwrap()).unwrap();
            fs::write(&plan.binary, b"built").unwrap();
            Ok(self.status)
        }
        fn smoke(
            &mut self,
            _: &Path,
            environment: &BTreeMap<String, std::ffi::OsString>,
            timeout: u64,
        ) -> Result<SmokeOutput> {
            self.smokes += 1;
            assert_eq!(environment.len(), 2);
            assert_eq!(timeout, 30);
            Ok(SmokeOutput {
                returncode: self.status,
                stdout: self.help.clone(),
            })
        }
    }
    #[test]
    fn selection_build_prebuilt_and_failure_paths_are_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let krate = dir.path().join(CRATE_RELATIVE);
        fs::create_dir_all(krate.join("src")).unwrap();
        fs::write(krate.join("Cargo.toml"), "manifest").unwrap();
        let target = dir.path().join("target");
        let mut host = Host {
            cargo: None,
            status: 0,
            help: b"usage: provenant".to_vec(),
            builds: 0,
            smokes: 0,
        };
        assert!(matches!(
            select(dir.path(), "rust", &target, None, &mut host),
            Err(crate::Error::CargoUnavailable)
        ));
        host.cargo = Some("/cargo".into());
        let result = select(dir.path(), "rust", &target, None, &mut host).unwrap();
        assert_eq!(result.note, "built");
        assert_eq!((host.builds, host.smokes), (1, 1));
        let binary = dir.path().join("prebuilt");
        fs::write(
            &binary,
            format!(
                "provenant-front-door source-sha256={}\n",
                source_digest(&krate).unwrap()
            ),
        )
        .unwrap();
        assert_eq!(
            select(dir.path(), "rust", &target, Some(&binary), &mut host)
                .unwrap()
                .note,
            "prebuilt"
        );
        assert_eq!((host.builds, host.smokes), (1, 2));
        host.status = 7;
        assert!(
            select(dir.path(), "rust", &target, None, &mut host)
                .unwrap_err()
                .to_string()
                .contains("cargo build failed (exit 7)")
        );
        assert_eq!(host.smokes, 2);
        let command = Command::Select {
            product_root: dir.path().into(),
            mode: "rust".into(),
            build_dir: target,
            prebuilt: None,
        };
        host.cargo = None;
        let result = run(&command, &mut host);
        assert_eq!(result.exit_code, 3);
        assert!(result.stderr.ends_with(&format!("{RUSTUP_FIX}\n")));
    }
}

#[cfg(test)]
mod dot_source_tests {
    use super::*;
    #[test]
    fn literal_dot_rs_is_a_rust_glob_input_even_inside_src() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("Cargo.toml"), "manifest").unwrap();
        let before = source_digest(root).unwrap();
        fs::write(root.join("src/.rs"), "first").unwrap();
        let first = source_digest(root).unwrap();
        assert_ne!(first, before);
        fs::write(root.join("src/.rs"), "second").unwrap();
        assert_ne!(source_digest(root).unwrap(), first);
        let Some(expected) = crate::oracle::python(
            "scripts/lib/front_door_build.py",
            "print(m.source_digest(p))",
            root,
        ) else {
            return;
        };
        assert_eq!(
            format!("{}\n", source_digest(root).unwrap()).as_bytes(),
            expected
        );
    }
}
