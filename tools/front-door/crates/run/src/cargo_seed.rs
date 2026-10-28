//! Best-effort, private Cargo registry seed for writer lanes.
use std::path::Path;

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Outcome {
    status: &'static str,
    reason: &'static str,
}
impl Outcome {
    pub(crate) fn skipped(reason: &'static str) -> Self {
        Self {
            status: "skipped",
            reason,
        }
    }
    #[cfg(any(target_os = "macos", test))]
    fn fallback(reason: &'static str) -> Self {
        Self {
            status: "fallback",
            reason,
        }
    }
}

pub(crate) fn seed_registry(enabled: bool, home: &Path, cache: &Path) -> Outcome {
    if !enabled {
        return Outcome::skipped("disabled");
    }
    #[cfg(target_os = "macos")]
    {
        seed_with(home, cache, clone_registry)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (home, cache);
        Outcome::skipped("unsupported_platform")
    }
}

#[cfg(any(target_os = "macos", test))]
fn seed_with(
    home: &Path,
    cache: &Path,
    clone: impl FnOnce(&Path, &provenant_os::fs::Directory) -> Result<(), &'static str>,
) -> Outcome {
    use provenant_os::fs::Directory;
    use std::{
        ffi::OsStr,
        sync::atomic::{AtomicU64, Ordering},
    };
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let Ok(cache_dir) = Directory::open_anchor(cache) else {
        return Outcome::fallback("cache_unavailable");
    };
    let cargo_name = OsStr::new("cargo-home");
    if let Err(error) = cache_dir.mkdir(cargo_name, 0o700)
        && !error.is_exists()
    {
        return Outcome::fallback("cargo_home_unavailable");
    }
    let Ok(cargo_dir) = cache_dir.open_verified_directory(cargo_name) else {
        return Outcome::fallback("cargo_home_unavailable");
    };
    let registry = OsStr::new("registry");
    match cargo_dir.metadata_at(registry, false) {
        Ok(_) => return Outcome::skipped("existing_registry"),
        Err(error) if error.is_not_found() => {}
        Err(_) => return Outcome::fallback("registry_unavailable"),
    }
    let stage = format!(
        ".registry-seed-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    );
    let stage_name = OsStr::new(&stage);
    if cargo_dir.mkdir(stage_name, 0o700).is_err() {
        return Outcome::fallback("staging_unavailable");
    }
    let result = (|| {
        let staging = cargo_dir
            .open_verified_directory(stage_name)
            .map_err(|_| "staging_unavailable")?;
        clone(&home.join(".cargo/registry"), &staging)?;
        cargo_dir
            .rename_noreplace(stage_name, &cargo_dir, registry)
            .map_err(|_| "publish_failed")?;
        Ok(())
    })();
    match result {
        Ok(()) => Outcome {
            status: "seeded",
            reason: "cloned",
        },
        Err(reason) => {
            // This directory is private to the host and created exclusively above.
            if std::fs::remove_dir_all(cache.join("cargo-home").join(stage)).is_err() {
                return Outcome::fallback("staging_cleanup_failed");
            }
            Outcome::fallback(reason)
        }
    }
}

#[cfg(target_os = "macos")]
fn clone_registry(
    source: &Path,
    destination: &provenant_os::fs::Directory,
) -> Result<(), &'static str> {
    use provenant_os::fs::{Directory, FileKind};
    fn walk(source: &Directory, destination: &Directory, depth: usize) -> Result<(), &'static str> {
        if depth > 128 {
            return Err("registry_too_deep");
        }
        for entry in source.entries().map_err(|_| "registry_read_failed")? {
            let name = entry.map_err(|_| "registry_read_failed")?;
            match source
                .metadata_at(&name, false)
                .map_err(|_| "registry_read_failed")?
                .kind
            {
                FileKind::Directory => {
                    let child = source
                        .open_verified_directory(&name)
                        .map_err(|_| "registry_read_failed")?;
                    destination
                        .mkdir(&name, 0o700)
                        .map_err(|_| "clone_failed")?;
                    let target = destination
                        .open_verified_directory(&name)
                        .map_err(|_| "clone_failed")?;
                    walk(&child, &target, depth + 1)?;
                }
                FileKind::Regular => {
                    let file = source
                        .open_read(&name)
                        .map_err(|_| "registry_read_failed")?;
                    destination
                        .clone_file_from(&file, &name)
                        .map_err(|_| "clone_failed")?;
                }
                _ => return Err("registry_contains_unsupported_entry"),
            }
        }
        Ok(())
    }
    let source = Directory::open_anchor(source).map_err(|_| "registry_unavailable")?;
    walk(&source, destination, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_does_not_touch_cache() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            seed_registry(false, tmp.path(), tmp.path()),
            Outcome::skipped("disabled")
        );
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn enabled_is_a_no_op_on_other_platforms() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            seed_registry(true, tmp.path(), tmp.path()),
            Outcome::skipped("unsupported_platform")
        );
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[test]
    fn failed_clone_removes_partial_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let outcome = seed_with(tmp.path(), tmp.path(), |_, stage| {
            stage.mkdir(std::ffi::OsStr::new("partial"), 0o700).unwrap();
            Err("clone_failed")
        });
        assert_eq!(outcome, Outcome::fallback("clone_failed"));
        assert_eq!(
            std::fs::read_dir(tmp.path().join("cargo-home"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn existing_registry_is_never_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("cargo-home/registry")).unwrap();
        std::fs::write(tmp.path().join("cargo-home/registry/keep"), b"keep").unwrap();
        let outcome = seed_with(tmp.path(), tmp.path(), |_, _| Err("unexpected"));
        assert_eq!(outcome, Outcome::skipped("existing_registry"));
        assert_eq!(
            std::fs::read(tmp.path().join("cargo-home/registry/keep")).unwrap(),
            b"keep"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn registry_clone_is_private() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(home.join(".cargo/registry/index")).unwrap();
        std::fs::create_dir(&cache).unwrap();
        std::fs::write(home.join(".cargo/registry/index/item"), b"host").unwrap();
        assert_eq!(seed_registry(true, &home, &cache).status, "seeded");
        std::fs::write(cache.join("cargo-home/registry/index/item"), b"lane").unwrap();
        assert_eq!(
            std::fs::read(home.join(".cargo/registry/index/item")).unwrap(),
            b"host"
        );
    }
}
