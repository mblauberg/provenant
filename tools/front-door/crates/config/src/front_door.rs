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
        for path in rust_files
            .into_iter()
            .filter(|path| path.extension().is_some_and(|e| e == "rs"))
        {
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
        return Err(invalid(format!("prebuilt front door {} was built from different sources (embeds {}, checkout is {digest}); rebuild it from this checkout", binary.display(), stamped.unwrap_or("None"))));
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
