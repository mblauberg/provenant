//! Front-door source identity and build policy; execution belongs to supervisor.
use crate::{invalid, io, pending, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

pub const CRATE_RELATIVE: &str = "tools/front-door";
pub const HASHED_FILES: [&str; 3] = ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"];
pub const RUSTUP_FIX: &str = "fix: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh";
pub const DEPLOYMENT_TARGET: &str = "15.0";
pub const SMOKE_TIMEOUT_SECONDS: u64 = 30;
const IDENTITY: &[u8] = b"provenant-front-door source-sha256=";

fn files(directory: &Path, result: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory).map_err(|e| io(directory, e))? {
        let path = entry.map_err(|e| io(directory, e))?.path();
        if path.is_file() {
            result.push(path);
        } else if path.is_dir() && !path.is_symlink() {
            files(&path, result)?;
        }
    }
    Ok(())
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
    let mut rest = vec![];
    files(&krate.join("src"), &mut rest)?;
    for relative in ["build.rs", ".cargo/config", ".cargo/config.toml"] {
        if krate.join(relative).is_file() {
            rest.push(krate.join(relative));
        }
    }
    let members = krate.join("crates");
    if members.is_dir() {
        for entry in fs::read_dir(&members).map_err(|e| io(&members, e))? {
            let member = entry.map_err(|e| io(&members, e))?.path();
            if !member.join("Cargo.toml").is_file() {
                continue;
            }
            rest.push(member.join("Cargo.toml"));
            if member.join("build.rs").is_file() {
                rest.push(member.join("build.rs"));
            }
            if member.join("src").is_dir() {
                files(&member.join("src"), &mut rest)?;
            }
        }
    }
    // Python's source ordering compares POSIX strings, not Path components.
    let mut encoded = rest
        .into_iter()
        .map(|p| {
            let relative = p.strip_prefix(krate).map_err(|e| invalid(e.to_string()))?;
            let name = relative
                .to_str()
                .ok_or_else(|| invalid("source digest path is not UTF-8"))?
                .to_owned();
            Ok((name, p))
        })
        .collect::<Result<Vec<_>>>()?;
    encoded.sort_by(|a, b| a.0.cmp(&b.0));
    names.extend(encoded.into_iter().map(|(_, path)| path));
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
        return Err(invalid(format!("prebuilt front door {} was built from different sources (embeds {}, checkout is {digest}); rebuild it from this checkout", binary.display(), stamped.unwrap_or("None"))));
    }
    Ok(())
}

pub fn validate_mode(mode: &str) -> Result<()> {
    if mode != "rust" {
        return Err(invalid(format!(
            "PROVENANT_FRONT_DOOR must be rust, got {mode:?}"
        )));
    }
    Ok(())
}

pub fn cargo_path(path: &std::ffi::OsStr, home: &Path) -> Option<PathBuf> {
    for directory in std::env::split_paths(path) {
        let candidate = directory.join("cargo");
        if candidate.is_file()
            && fs::metadata(&candidate)
                .ok()
                .is_some_and(|m| m.permissions().mode() & 0o111 != 0)
        {
            return Some(candidate);
        }
    }
    let fallback = home.join(".cargo/bin/cargo");
    fallback.is_file().then_some(fallback)
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
    let krate = pending::resolve(krate)?;
    let target = pending::resolve(target)?;
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
