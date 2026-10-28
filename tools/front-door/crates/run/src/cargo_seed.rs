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
    use provenant_os::fs::{Directory, LockMode, temporary_name};
    use std::ffi::{OsStr, OsString};
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
    if cargo_dir.make_private().is_err() {
        return Outcome::fallback("cargo_home_not_private");
    }
    // Serialise staging and recovery, including across interrupted attempts.
    let Ok(_lock) = cargo_dir.try_lock(OsStr::new("registry-seed.lock"), LockMode::Exclusive)
    else {
        return Outcome::fallback("seed_lock_unavailable");
    };
    let Ok(entries) = cargo_dir.entries() else {
        return Outcome::fallback("staging_cleanup_failed");
    };
    for entry in entries {
        let Ok(name) = entry else {
            return Outcome::fallback("staging_cleanup_failed");
        };
        if name.to_string_lossy().starts_with(".registry-seed-")
            && crate::artifacts::remove_entry_tree(&cargo_dir, &name).is_err()
        {
            return Outcome::fallback("staging_cleanup_failed");
        }
    }
    let registry = OsStr::new("registry");
    match cargo_dir.metadata_at(registry, false) {
        Ok(_) => return Outcome::skipped("existing_registry"),
        Err(error) if error.is_not_found() => {}
        Err(_) => return Outcome::fallback("registry_unavailable"),
    }
    let mut stage = None;
    for _ in 0..16 {
        let Ok(random) = temporary_name() else {
            return Outcome::fallback("staging_unavailable");
        };
        let mut name = OsString::from(".registry-seed-");
        name.push(random);
        match cargo_dir.mkdir(&name, 0o700) {
            Ok(()) => {
                stage = Some(name);
                break;
            }
            Err(error) if error.is_exists() => continue,
            Err(_) => return Outcome::fallback("staging_unavailable"),
        }
    }
    let Some(stage_name) = stage else {
        return Outcome::fallback("staging_unavailable");
    };
    let result = (|| {
        let staging = cargo_dir
            .open_verified_directory(&stage_name)
            .map_err(|_| "staging_unavailable")?;
        clone(&home.join(".cargo/registry"), &staging)?;
        sync_tree(&staging).map_err(|_| "staging_sync_failed")?;
        match cargo_dir.rename_noreplace(&stage_name, &cargo_dir, registry) {
            Ok(()) => {}
            Err(error) if error.is_exists() => return Err("publish_raced"),
            Err(_) => return Err("publish_failed"),
        }
        Ok(())
    })();
    match result {
        Ok(()) => Outcome {
            status: "seeded",
            reason: if cargo_dir.sync().is_ok() {
                "cloned"
            } else {
                "durability_unconfirmed"
            },
        },
        Err(reason) => {
            if crate::artifacts::remove_entry_tree(&cargo_dir, &stage_name).is_err() {
                return Outcome::fallback("staging_cleanup_failed");
            }
            if reason == "publish_raced" {
                Outcome::skipped(reason)
            } else {
                Outcome::fallback(reason)
            }
        }
    }
}

#[cfg(any(target_os = "macos", test))]
fn sync_tree(directory: &provenant_os::fs::Directory) -> provenant_os::fs::Result<()> {
    use provenant_os::fs::FileKind;
    for name in directory.entries()? {
        let name = name?;
        match directory.metadata_at(&name, false)?.kind {
            FileKind::Directory => sync_tree(&directory.open_verified_directory(&name)?)?,
            FileKind::Regular => directory.open_read(&name)?.sync()?,
            _ => return Err(provenant_os::fs::FsError::NotRegular { name }),
        }
    }
    directory.sync()
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
            1 // The stable seed lock remains for the next attempt.
        );
    }

    #[test]
    fn stale_stages_are_removed_and_cargo_home_is_private() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let tmp = tempfile::tempdir().unwrap();
        let cargo = tmp.path().join("cargo-home");
        std::fs::create_dir(&cargo).unwrap();
        std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir(cargo.join(".registry-seed-stale")).unwrap();
        let outcome = seed_with(tmp.path(), tmp.path(), |_, _| Err("clone_failed"));
        assert_eq!(outcome, Outcome::fallback("clone_failed"));
        assert!(!cargo.join(".registry-seed-stale").exists());
        assert_eq!(std::fs::metadata(cargo).unwrap().mode() & 0o777, 0o700);
    }

    #[test]
    fn failed_cleanup_does_not_follow_replaced_cargo_home() {
        let tmp = tempfile::tempdir().unwrap();
        let cargo = tmp.path().join("cargo-home");
        let held = tmp.path().join("held");
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let outcome = seed_with(tmp.path(), tmp.path(), |_, _| {
            std::fs::rename(&cargo, &held).unwrap();
            std::os::unix::fs::symlink(&outside, &cargo).unwrap();
            Err("clone_failed")
        });
        assert_eq!(outcome, Outcome::fallback("clone_failed"));
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(&held).unwrap().count(), 1);
    }

    #[test]
    fn publish_race_keeps_existing_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let outcome = seed_with(tmp.path(), tmp.path(), |_, _| {
            std::fs::create_dir(tmp.path().join("cargo-home/registry")).unwrap();
            Ok(())
        });
        assert_eq!(outcome, Outcome::skipped("publish_raced"));
        assert!(tmp.path().join("cargo-home/registry").is_dir());
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

    #[cfg(target_os = "macos")]
    #[test]
    fn registry_symlink_refuses_entire_seed() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(home.join(".cargo/registry/src")).unwrap();
        std::fs::create_dir(&cache).unwrap();
        std::os::unix::fs::symlink("/outside", home.join(".cargo/registry/src/link")).unwrap();
        assert_eq!(
            seed_registry(true, &home, &cache),
            Outcome::fallback("registry_contains_unsupported_entry")
        );
        assert!(!cache.join("cargo-home/registry").exists());
    }
}
