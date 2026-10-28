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
    pub(crate) fn fallback(reason: &'static str) -> Self {
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

/// Seed the Git store independently of the registry.
pub(crate) fn seed_git(enabled: bool, home: &Path, cache: &Path) -> Outcome {
    if !enabled {
        return Outcome::skipped("disabled");
    }
    #[cfg(target_os = "macos")]
    {
        seed_store_with(home, cache, "git", |source, dest| {
            clone_registry(source, dest).map_err(|reason| match reason {
                "registry_unavailable" => "git_unavailable",
                "registry_read_failed" => "git_read_failed",
                "registry_too_deep" => "git_too_deep",
                "registry_contains_unsupported_entry" => "git_contains_unsupported_entry",
                other => other,
            })
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (home, cache);
        Outcome::skipped("unsupported_platform")
    }
}

pub(crate) fn seed_target(
    enabled: bool,
    state: &Path,
    cache: &Path,
    worktree: &Path,
    home: &Path,
    environment: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Outcome {
    if !enabled {
        return Outcome::skipped("disabled");
    }
    #[cfg(target_os = "macos")]
    {
        let Ok((version, triple)) =
            crate::target_seed::lane_rustc_metadata(worktree, home, environment)
        else {
            return Outcome::fallback("toolchain_unverified");
        };
        seed_target_with(state, cache, &version, &triple, clone_target)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (state, cache, worktree, home, environment);
        Outcome::skipped("unsupported_platform")
    }
}

#[cfg(any(target_os = "macos", test))]
fn seed_target_with(
    state: &Path,
    cache: &Path,
    version: &str,
    triple: &str,
    clone: impl FnOnce(&Path, &provenant_os::fs::Directory) -> Result<(), &'static str>,
) -> Outcome {
    use provenant_os::fs::{Directory, temporary_name};
    use std::ffi::{OsStr, OsString};
    let Ok(cache_dir) = Directory::open_anchor(cache) else {
        return Outcome::fallback("cache_unavailable");
    };
    match cache_dir.metadata_at(OsStr::new("target"), false) {
        Ok(_) => return Outcome::skipped("existing_target"),
        Err(error) if error.is_not_found() => {}
        Err(_) => return Outcome::fallback("target_unavailable"),
    }
    // Held until the entire clone is published or removed: GC cannot unlink its source.
    let Ok(_lock) = crate::target_seed::lock_store(state) else {
        return Outcome::fallback("seed_lock_unavailable");
    };
    let generations = crate::target_seed::store_path(state).join("generations");
    let Ok(entries) = std::fs::read_dir(generations) else {
        return Outcome::fallback("store_unavailable");
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
    let mut saw_version = false;
    let mut saw_manifest = false;
    let mut saw_entry = false;
    let mut selected = None;
    for entry in entries {
        use std::os::unix::fs::PermissionsExt;
        if !entry.file_type().is_ok_and(|kind| kind.is_dir())
            || entry.file_name().to_string_lossy().starts_with('.')
        {
            continue;
        }
        saw_entry = true;
        if !entry
            .metadata()
            .is_ok_and(|m| m.permissions().mode() & 0o222 == 0)
        {
            continue;
        }
        if !std::fs::read(entry.path().join("complete")).is_ok_and(|data| data == b"complete\n") {
            continue;
        }
        let Ok(data) = std::fs::read(entry.path().join("manifest.json")) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_slice::<crate::target_seed::Manifest>(&data) else {
            continue;
        };
        saw_manifest = true;
        if manifest.rustc_vv != version {
            continue;
        }
        saw_version = true;
        if manifest.host_triple != triple {
            continue;
        }
        selected = Some(entry.path().join("target"));
        break;
    }
    let Some(source) = selected else {
        return Outcome::fallback(if !saw_entry {
            "no_generations"
        } else if !saw_manifest {
            "manifest_invalid"
        } else if !saw_version {
            "toolchain_mismatch"
        } else {
            "triple_mismatch"
        });
    };
    let Ok(random) = temporary_name() else {
        return Outcome::fallback("staging_unavailable");
    };
    let mut stage = OsString::from(".target-seed-");
    stage.push(random);
    if cache_dir.mkdir(&stage, 0o700).is_err() {
        return Outcome::fallback("staging_unavailable");
    }
    let result = (|| {
        let staging = cache_dir
            .open_verified_directory(&stage)
            .map_err(|_| "staging_unavailable")?;
        clone(&source, &staging)?;
        cache_dir
            .rename_noreplace(&stage, &cache_dir, OsStr::new("target"))
            .map_err(|_| "publish_failed")?;
        Ok(())
    })();
    match result {
        Ok(()) => Outcome {
            status: "seeded",
            reason: "cloned",
        },
        Err(reason) => {
            if crate::artifacts::remove_entry_tree(&cache_dir, &stage).is_err() {
                return Outcome::fallback("staging_cleanup_failed");
            }
            Outcome::fallback(reason)
        }
    }
}

#[cfg(target_os = "macos")]
fn clone_target(
    source: &Path,
    destination: &provenant_os::fs::Directory,
) -> Result<(), &'static str> {
    // The seed can be read-only; make each clone writable without changing the seed.
    fn walk(
        source: &provenant_os::fs::Directory,
        dest: &provenant_os::fs::Directory,
        depth: usize,
    ) -> Result<(), &'static str> {
        use provenant_os::fs::FileKind;
        if depth > 128 {
            return Err("target_too_deep");
        }
        for entry in source.entries().map_err(|_| "target_read_failed")? {
            let name = entry.map_err(|_| "target_read_failed")?;
            match source
                .metadata_at(&name, false)
                .map_err(|_| "target_read_failed")?
                .kind
            {
                FileKind::Directory => {
                    let child = source
                        .open_verified_directory(&name)
                        .map_err(|_| "target_read_failed")?;
                    dest.mkdir(&name, 0o700).map_err(|_| "clone_failed")?;
                    let target = dest
                        .open_verified_directory(&name)
                        .map_err(|_| "clone_failed")?;
                    walk(&child, &target, depth + 1)?;
                }
                FileKind::Regular => {
                    let file = source.open_read(&name).map_err(|_| "target_read_failed")?;
                    dest.clone_file_from(&file, &name)
                        .map_err(|_| "clone_failed")?;
                    dest.open_read(&name)
                        .and_then(|file| file.make_owner_writable())
                        .map_err(|_| "clone_failed")?;
                }
                _ => return Err("target_contains_unsupported_entry"),
            }
        }
        Ok(())
    }
    let source =
        provenant_os::fs::Directory::open_anchor(source).map_err(|_| "target_unavailable")?;
    walk(&source, destination, 0)
}

#[cfg(any(target_os = "macos", test))]
fn seed_with(
    home: &Path,
    cache: &Path,
    clone: impl FnOnce(&Path, &provenant_os::fs::Directory) -> Result<(), &'static str>,
) -> Outcome {
    seed_store_with(home, cache, "registry", clone)
}

#[cfg(any(target_os = "macos", test))]
fn seed_store_with(
    home: &Path,
    cache: &Path,
    store: &'static str,
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
    let lock_name = format!("{store}-seed.lock");
    let stage_prefix = format!(".{store}-seed-");
    let Ok(_lock) = cargo_dir.try_lock(OsStr::new(&lock_name), LockMode::Exclusive) else {
        return Outcome::fallback("seed_lock_unavailable");
    };
    let Ok(entries) = cargo_dir.entries() else {
        return Outcome::fallback("staging_cleanup_failed");
    };
    for entry in entries {
        let Ok(name) = entry else {
            return Outcome::fallback("staging_cleanup_failed");
        };
        if name.to_string_lossy().starts_with(&stage_prefix)
            && crate::artifacts::remove_entry_tree(&cargo_dir, &name).is_err()
        {
            return Outcome::fallback("staging_cleanup_failed");
        }
    }
    let destination = OsStr::new(store);
    match cargo_dir.metadata_at(destination, false) {
        Ok(_) => {
            return Outcome::skipped(if store == "registry" {
                "existing_registry"
            } else {
                "existing_git"
            });
        }
        Err(error) if error.is_not_found() => {}
        Err(_) => {
            return Outcome::fallback(if store == "registry" {
                "registry_unavailable"
            } else {
                "git_unavailable"
            });
        }
    }
    let mut stage = None;
    for _ in 0..16 {
        let Ok(random) = temporary_name() else {
            return Outcome::fallback("staging_unavailable");
        };
        let mut name = OsString::from(&stage_prefix);
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
        clone(&home.join(".cargo").join(store), &staging)?;
        sync_tree(&staging).map_err(|_| "staging_sync_failed")?;
        match cargo_dir.rename_noreplace(&stage_name, &cargo_dir, destination) {
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

    fn generation(state: &Path, name: &str, version: &str, triple: &str) {
        let dir = crate::target_seed::store_path(state)
            .join("generations")
            .join(name);
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("complete"), b"complete\n").unwrap();
        let manifest = crate::target_seed::Manifest {
            rustc_vv: version.into(),
            host_triple: triple.into(),
            commit: "abc".into(),
            profile: "dev".into(),
            features: vec![],
            flags: vec![],
        };
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    }

    #[test]
    fn target_selection_and_fallbacks() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let cache = tmp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        let run = |version, triple| {
            seed_target_with(&state, &cache, version, triple, |source, _| {
                assert!(source.ends_with("002/target"));
                Ok(())
            })
        };
        assert_eq!(run("v1", "host"), Outcome::fallback("no_generations"));
        generation(&state, "001", "v1", "host");
        generation(&state, "002", "v1", "host");
        generation(&state, "003", "v2", "other");
        assert_eq!(
            run("unknown", "host"),
            Outcome::fallback("toolchain_mismatch")
        );
        assert_eq!(run("v1", "other"), Outcome::fallback("triple_mismatch"));
        assert_eq!(run("v1", "host").status, "seeded");
        assert_eq!(run("v1", "host"), Outcome::skipped("existing_target"));
    }

    #[test]
    fn target_failed_clone_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        generation(tmp.path(), "001", "v1", "host");
        let outcome = seed_target_with(tmp.path(), &cache, "v1", "host", |_, stage| {
            stage.mkdir(std::ffi::OsStr::new("partial"), 0o700).unwrap();
            Err("clone_failed")
        });
        assert_eq!(outcome, Outcome::fallback("clone_failed"));
        assert_eq!(std::fs::read_dir(cache).unwrap().count(), 0);
    }

    #[test]
    fn target_invalid_manifest_and_busy_store_fall_back() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        generation(tmp.path(), "001", "v1", "host");
        let manifest =
            crate::target_seed::store_path(tmp.path()).join("generations/001/manifest.json");
        std::fs::write(manifest, b"invalid").unwrap();
        let run = || {
            seed_target_with(tmp.path(), &cache, "v1", "host", |_, _| {
                panic!("must not clone")
            })
        };
        assert_eq!(run(), Outcome::fallback("manifest_invalid"));
        let _lock = crate::target_seed::lock_store(tmp.path()).unwrap();
        assert_eq!(run(), Outcome::fallback("seed_lock_unavailable"));
    }

    #[test]
    fn target_incomplete_generation_falls_back() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        generation(tmp.path(), "001", "v1", "host");
        let dir = crate::target_seed::store_path(tmp.path()).join("generations/001");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let run = || {
            seed_target_with(tmp.path(), &cache, "v1", "host", |_, _| {
                panic!("must not clone")
            })
        };
        assert_eq!(run(), Outcome::fallback("manifest_invalid"));
        std::fs::remove_file(dir.join("complete")).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(run(), Outcome::fallback("manifest_invalid"));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn target_clone_is_writable_and_cannot_change_seed() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        generation(tmp.path(), "001", "v1", "host");
        let seed = crate::target_seed::store_path(tmp.path()).join("generations/001/target/file");
        std::fs::write(&seed, b"seed").unwrap();
        std::fs::set_permissions(&seed, std::fs::Permissions::from_mode(0o444)).unwrap();
        assert_eq!(
            seed_target_with(tmp.path(), &cache, "v1", "host", clone_target).status,
            "seeded"
        );
        std::fs::write(cache.join("target/file"), b"lane").unwrap();
        assert_eq!(std::fs::read(&seed).unwrap(), b"seed");
    }

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

    #[test]
    fn git_seed_is_independent_and_preserves_existing_stores() {
        let tmp = tempfile::tempdir().unwrap();
        let cargo = tmp.path().join("cargo-home");
        std::fs::create_dir_all(cargo.join("registry")).unwrap();
        std::fs::write(cargo.join("registry/keep"), b"registry").unwrap();
        let outcome = seed_store_with(tmp.path(), tmp.path(), "git", |source, stage| {
            assert_eq!(source, tmp.path().join(".cargo/git"));
            stage.mkdir(std::ffi::OsStr::new("db"), 0o700).unwrap();
            Ok(())
        });
        assert_eq!(outcome.status, "seeded");
        assert!(cargo.join("git/db").is_dir());
        assert_eq!(
            std::fs::read(cargo.join("registry/keep")).unwrap(),
            b"registry"
        );
        assert_eq!(
            seed_store_with(tmp.path(), tmp.path(), "git", |_, _| Err("unexpected")),
            Outcome::skipped("existing_git")
        );
    }

    #[test]
    fn failed_git_clone_cleans_staging_without_touching_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let cargo = tmp.path().join("cargo-home");
        std::fs::create_dir_all(cargo.join("registry")).unwrap();
        let outcome = seed_store_with(tmp.path(), tmp.path(), "git", |_, stage| {
            stage.mkdir(std::ffi::OsStr::new("partial"), 0o700).unwrap();
            Err("clone_failed")
        });
        assert_eq!(outcome, Outcome::fallback("clone_failed"));
        assert!(!cargo.join("git").exists());
        assert!(cargo.join("registry").is_dir());
        assert!(!std::fs::read_dir(cargo).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".git-seed-")
        }));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn git_clone_is_private() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(home.join(".cargo/git/db")).unwrap();
        std::fs::create_dir(&cache).unwrap();
        std::fs::write(home.join(".cargo/git/db/item"), b"host").unwrap();
        assert_eq!(seed_git(true, &home, &cache).status, "seeded");
        std::fs::write(cache.join("cargo-home/git/db/item"), b"lane").unwrap();
        assert_eq!(
            std::fs::read(home.join(".cargo/git/db/item")).unwrap(),
            b"host"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn missing_and_unsupported_git_store_fall_back() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cache = tmp.path().join("cache");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir(&cache).unwrap();
        assert_eq!(
            seed_git(true, &home, &cache),
            Outcome::fallback("git_unavailable")
        );
        std::fs::create_dir_all(home.join(".cargo/git/checkouts")).unwrap();
        std::os::unix::fs::symlink("/outside", home.join(".cargo/git/checkouts/link")).unwrap();
        assert_eq!(
            seed_git(true, &home, &cache),
            Outcome::fallback("git_contains_unsupported_entry")
        );
        assert!(!cache.join("cargo-home/git").exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "offline fixture runs isolated git and cargo"
    )]
    fn seeded_git_dependency_builds_offline() {
        use std::process::Command;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("host");
        let cache = tmp.path().join("lane");
        let dep = tmp.path().join("dependency");
        let app = tmp.path().join("app");
        std::fs::create_dir_all(dep.join("src")).unwrap();
        std::fs::create_dir_all(app.join("src")).unwrap();
        std::fs::create_dir_all(home.join(".cargo")).unwrap();
        std::fs::create_dir(&cache).unwrap();
        std::fs::write(
            dep.join("Cargo.toml"),
            "[package]\nname = \"seed_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(dep.join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n").unwrap();
        for args in [
            &["init", "-q"][..],
            &["add", "."][..],
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ][..],
        ] {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&dep)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        std::fs::write(app.join("Cargo.toml"), format!("[package]\nname = \"seed_app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\nseed_fixture = {{ git = \"file://{}\" }}\n", dep.display())).unwrap();
        std::fs::write(
            app.join("src/lib.rs"),
            "pub fn value() -> u8 { seed_fixture::value() }\n",
        )
        .unwrap();
        assert!(
            Command::new("cargo")
                .arg("fetch")
                .current_dir(&app)
                .env("CARGO_HOME", home.join(".cargo"))
                .env("CARGO_NET_GIT_FETCH_WITH_CLI", "true")
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(seed_git(true, &home, &cache).status, "seeded");
        assert!(
            Command::new("cargo")
                .args(["check", "--offline"])
                .current_dir(&app)
                .env("CARGO_HOME", cache.join("cargo-home"))
                .env("CARGO_TARGET_DIR", cache.join("target"))
                .env("CARGO_BUILD_BUILD_DIR", cache.join("target"))
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn git_recovery_leaves_registry_staging_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let cargo = tmp.path().join("cargo-home");
        std::fs::create_dir_all(cargo.join(".registry-seed-active")).unwrap();
        std::fs::create_dir(cargo.join(".git-seed-stale")).unwrap();
        let outcome = seed_store_with(tmp.path(), tmp.path(), "git", |_, _| Err("clone_failed"));
        assert_eq!(outcome, Outcome::fallback("clone_failed"));
        assert!(cargo.join(".registry-seed-active").is_dir());
        assert!(!cargo.join(".git-seed-stale").exists());
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
