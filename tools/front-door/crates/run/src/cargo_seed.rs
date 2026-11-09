//! Best-effort, private Cargo registry seed for writer lanes.
use std::path::Path;

/// Every `detail` a toolchain-verification refusal can report in
/// `cargo-target-setup.json`. One list, so a typo cannot invent a code and a
/// test can hold the set to snake_case without duplicates.
macro_rules! detail_codes {
    ($($name:ident = $text:literal),* $(,)?) => {
        // Toolchain verification runs only on Unix, where seeds are cloned.
        #[cfg_attr(not(unix), allow(dead_code))]
        pub(crate) mod codes {
            $(pub(crate) const $name: &str = $text;)*
            #[cfg(test)]
            pub(crate) const ALL: &[&str] = &[$($name),*];
        }
    };
}
detail_codes! {
    WORKTREE_LAYOUT_UNEXPECTED = "worktree_layout_unexpected",
    CARGO_HOME_ENV_MISSING = "cargo_home_env_missing",
    CARGO_HOME_ENV_UNEXPECTED = "cargo_home_env_unexpected",
    CARGO_HOME_CONFIG_PRESENT = "cargo_home_config_present",
    CARGO_CONFIG_UNREADABLE = "cargo_config_unreadable",
    CARGO_CONFIG_TOO_LARGE = "cargo_config_too_large",
    CARGO_CONFIG_INVALID = "cargo_config_invalid",
    CARGO_CONFIG_SELECTS_COMPILER = "cargo_config_selects_compiler",
    WORKTREE_UNRESOLVABLE = "worktree_unresolvable",
    RUSTUP_HOME_WRITABLE = "rustup_home_writable",
    CARGO_BIN_WRITABLE = "cargo_bin_writable",
    PATH_MISSING = "path_missing",
    PATH_DUPLICATED = "path_duplicated",
    RUSTUP_SHIM_UNRESOLVABLE = "rustup_shim_unresolvable",
    RUSTUP_SHIM_WRITABLE = "rustup_shim_writable",
    PATH_ENTRY_RELATIVE = "path_entry_relative",
    PATH_ENTRY_WRITABLE = "path_entry_writable",
    RUSTC_NOT_RUSTUP_SHIM = "rustc_not_rustup_shim",
    RUSTC_PATH_UNRESOLVABLE = "rustc_path_unresolvable",
    RUSTC_NOT_ON_PATH = "rustc_not_on_path",
    RUSTUP_HOME_UNEXPECTED = "rustup_home_unexpected",
    RUSTUP_AUTO_INSTALL_UNEXPECTED = "rustup_auto_install_unexpected",
    TOOLCHAIN_ENV_OVERRIDE = "toolchain_env_override",
    RUSTUP_HOME_MISSING = "rustup_home_missing",
    TOOLCHAIN_DIR_UNREADABLE = "toolchain_dir_unreadable",
    RELEASE_TOOLCHAIN_AMBIGUOUS_OR_MISSING = "release_toolchain_ambiguous_or_missing",
    TOOLCHAIN_NOT_DIRECTORY = "toolchain_not_directory",
    COMPILER_UNRESOLVABLE = "compiler_unresolvable",
    TOOLCHAIN_UNRESOLVABLE = "toolchain_unresolvable",
    COMPILER_ESCAPES_HOST_TOOLCHAIN = "compiler_escapes_host_toolchain",
    COMPILER_PROBE_FAILED = "compiler_probe_failed",
    COMPILER_PROBE_INVALID = "compiler_probe_invalid",
    COMPILER_PIN_MISMATCH = "compiler_pin_mismatch",
    COMPILER_HOST_MISMATCH = "compiler_host_mismatch",
    TOOLCHAIN_CONFIG_TOO_LARGE = "toolchain_config_too_large",
    TOOLCHAIN_CONFIG_INVALID = "toolchain_config_invalid",
    RUSTUP_OVERRIDES_PRESENT = "rustup_overrides_present",
    TOOLCHAIN_PIN_NON_REGULAR = "toolchain_pin_non_regular",
    TOOLCHAIN_PIN_UNREADABLE = "toolchain_pin_unreadable",
    TOOLCHAIN_PIN_CUSTOM = "toolchain_pin_custom",
    TOOLCHAIN_PIN_ABSENT_OR_AMBIGUOUS = "toolchain_pin_absent_or_ambiguous",
    TOOLCHAIN_NOT_FIXED_RELEASE = "toolchain_not_fixed_release",
    RELEASE_TOOLCHAIN_MISSING = "release_toolchain_missing",
    COMPILER_HOST_MISSING = "compiler_host_missing",
    TOOLCHAIN_PIN_INVALID = "toolchain_pin_invalid",
    TOOLCHAIN_PIN_CHANNEL_MISSING = "toolchain_pin_channel_missing",
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Outcome {
    status: &'static str,
    reason: &'static str,
    /// Stable snake_case code for the specific refusal behind `reason`. Never
    /// an environment value or path.
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<&'static str>,
}
impl Outcome {
    pub(crate) fn skipped(reason: &'static str) -> Self {
        Self {
            status: "skipped",
            reason,
            detail: None,
        }
    }
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn fallback(reason: &'static str) -> Self {
        Self {
            status: "fallback",
            reason,
            detail: None,
        }
    }
    /// One line for the host's stderr when a seed was wanted and not used, so
    /// a lane that builds its dependencies from scratch is not a surprise.
    pub(crate) fn fallback_warning(&self, what: &str) -> Option<String> {
        (self.status == "fallback").then(|| {
            format!(
                "warning: {what} not seeded ({}{}); this lane builds without it",
                self.reason,
                self.detail.map(|d| format!(": {d}")).unwrap_or_default()
            )
        })
    }
    pub(crate) fn fallback_detail(reason: &'static str, detail: &'static str) -> Self {
        Self {
            status: "fallback",
            reason,
            detail: Some(detail),
        }
    }
}

pub(crate) fn seed_registry(
    enabled: bool,
    home: &Path,
    cache: &Path,
    cancel: &provenant_core::CancelToken,
) -> Outcome {
    if !enabled {
        return Outcome::skipped("disabled");
    }
    #[cfg(unix)]
    {
        seed_store_cancellable(home, cache, "registry", Some(cancel), |source, dest| {
            clone_registry(source, dest, cancel)
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (home, cache, cancel);
        Outcome::skipped("unsupported_platform")
    }
}

/// Seed the Git store independently of the registry.
pub(crate) fn seed_git(
    enabled: bool,
    home: &Path,
    cache: &Path,
    cancel: &provenant_core::CancelToken,
) -> Outcome {
    if !enabled {
        return Outcome::skipped("disabled");
    }
    #[cfg(unix)]
    {
        seed_store_cancellable(home, cache, "git", Some(cancel), |source, dest| {
            clone_git(source, dest, cancel)
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (home, cache, cancel);
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
    cancel: &provenant_core::CancelToken,
) -> Outcome {
    if !enabled {
        return Outcome::skipped("disabled");
    }
    #[cfg(unix)]
    {
        let (version, triple) = match crate::target_seed::lane_rustc_metadata_cancellable(
            worktree,
            home,
            environment,
            Some(cancel),
        ) {
            Ok(metadata) => metadata,
            Err(detail) => return Outcome::fallback_detail("toolchain_unverified", detail),
        };
        seed_target_cancellable(
            state,
            cache,
            &version,
            &triple,
            Some(cancel),
            |source, dest| clone_target(source, dest, cancel),
        )
    }
    #[cfg(not(unix))]
    {
        let _ = (state, cache, worktree, home, environment, cancel);
        Outcome::skipped("unsupported_platform")
    }
}

#[cfg(test)]
fn seed_target_with(
    state: &Path,
    cache: &Path,
    version: &str,
    triple: &str,
    clone: impl FnOnce(&Path, &provenant_os::fs::Directory) -> Result<(), &'static str>,
) -> Outcome {
    seed_target_cancellable(state, cache, version, triple, None, clone)
}

#[cfg(any(unix, test))]
fn seed_target_cancellable(
    state: &Path,
    cache: &Path,
    version: &str,
    triple: &str,
    cancel: Option<&provenant_core::CancelToken>,
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
    // An interrupted clone leaves a private stage; remove it under the same
    // store lock before attempting another target seed.
    let Ok(stages) = cache_dir.entries() else {
        return Outcome::fallback("staging_cleanup_failed");
    };
    for stage in stages {
        if cancel.is_some_and(|token| token.is_cancelled()) {
            return Outcome::fallback("cancelled");
        }
        let Ok(stage) = stage else {
            return Outcome::fallback("staging_cleanup_failed");
        };
        if stage.to_string_lossy().starts_with(".target-seed-") {
            match remove_seed_tree(&cache_dir, &stage, cancel) {
                Ok(true) => {}
                Ok(false) => return Outcome::fallback("cancelled"),
                Err(_) => return Outcome::fallback("staging_cleanup_failed"),
            }
        }
    }
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
        if cancel.is_some_and(|token| token.is_cancelled()) {
            return Outcome::fallback("cancelled");
        }
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
        // As for the registry: durable before the rename can be.
        staging.sync().map_err(|_| "staging_sync_failed")?;
        cache_dir
            .rename_noreplace(&stage, &cache_dir, OsStr::new("target"))
            .map_err(|_| "publish_failed")?;
        Ok(())
    })();
    match result {
        Ok(()) => Outcome {
            status: "seeded",
            reason: if cache_dir.sync().is_ok() {
                "cloned"
            } else {
                "durability_unconfirmed"
            },
            detail: None,
        },
        Err("cancelled") => Outcome::fallback("cancelled"),
        Err(reason) => {
            match remove_seed_tree(&cache_dir, &stage, cancel) {
                Ok(true) => {}
                Ok(false) => return Outcome::fallback("cancelled"),
                Err(_) => return Outcome::fallback("staging_cleanup_failed"),
            }
            Outcome::fallback(reason)
        }
    }
}

#[cfg(unix)]
fn clone_target(
    source: &Path,
    destination: &provenant_os::fs::Directory,
    cancel: &provenant_core::CancelToken,
) -> Result<(), &'static str> {
    // The seed can be read-only; make each clone writable without changing the seed.
    clone_tree(
        source,
        destination,
        &CloneStore {
            unavailable: "target_unavailable",
            read_failed: "target_read_failed",
            too_deep: "target_too_deep",
            unsupported: "target_contains_unsupported_entry",
            writable_files: true,
        },
        cancel,
    )
}

#[cfg(test)]
pub(crate) fn seed_with(
    home: &Path,
    cache: &Path,
    clone: impl FnOnce(&Path, &provenant_os::fs::Directory) -> Result<(), &'static str>,
) -> Outcome {
    seed_store_with(home, cache, "registry", clone)
}

#[cfg(test)]
fn seed_store_with(
    home: &Path,
    cache: &Path,
    store: &'static str,
    clone: impl FnOnce(&Path, &provenant_os::fs::Directory) -> Result<(), &'static str>,
) -> Outcome {
    seed_store_cancellable(home, cache, store, None, clone)
}

#[cfg(any(unix, test))]
fn seed_store_cancellable(
    home: &Path,
    cache: &Path,
    store: &'static str,
    cancel: Option<&provenant_core::CancelToken>,
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
    let Ok(entries) = cargo_dir.entries() else {
        return Outcome::fallback("staging_cleanup_failed");
    };
    for entry in entries {
        let Ok(name) = entry else {
            return Outcome::fallback("staging_cleanup_failed");
        };
        if cancel.is_some_and(|token| token.is_cancelled()) {
            return Outcome::fallback("cancelled");
        }
        if name.to_string_lossy().starts_with(&stage_prefix) {
            match remove_seed_tree(&cargo_dir, &name, cancel) {
                Ok(true) => {}
                Ok(false) => return Outcome::fallback("cancelled"),
                Err(_) => return Outcome::fallback("staging_cleanup_failed"),
            }
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
        // A clone writes no file data, and APFS commits metadata in order, so
        // flushing here makes the tree durable before the rename can be.
        staging.sync().map_err(|_| "staging_sync_failed")?;
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
            detail: None,
        },
        Err("cancelled") => Outcome::fallback("cancelled"),
        Err(reason) => {
            match remove_seed_tree(&cargo_dir, &stage_name, cancel) {
                Ok(true) => {}
                Ok(false) => return Outcome::fallback("cancelled"),
                Err(_) => return Outcome::fallback("staging_cleanup_failed"),
            }
            if reason == "publish_raced" {
                Outcome::skipped(reason)
            } else {
                Outcome::fallback(reason)
            }
        }
    }
}

// Stale or failed staging trees can be large too. Leave a partial private
// tree for the next seed if cancellation arrives during cleanup.
#[cfg(any(unix, test))]
fn remove_seed_tree(
    parent: &provenant_os::fs::Directory,
    name: &std::ffi::OsStr,
    cancel: Option<&provenant_core::CancelToken>,
) -> provenant_os::fs::Result<bool> {
    use provenant_os::fs::FileKind;
    if cancel.is_some_and(|token| token.is_cancelled()) {
        return Ok(false);
    }
    let metadata = match parent.metadata_at(name, false) {
        Ok(metadata) => metadata,
        Err(error) if error.is_not_found() => return Ok(true),
        Err(error) => return Err(error),
    };
    if metadata.kind != FileKind::Directory {
        parent.unlink(name)?;
        return Ok(true);
    }
    let directory = parent.open_verified_directory(name)?;
    directory.make_writable()?;
    let mut entries = Vec::new();
    for entry in directory.entries()? {
        if cancel.is_some_and(|token| token.is_cancelled()) {
            return Ok(false);
        }
        entries.push(entry?);
    }
    for entry in entries {
        if !remove_seed_tree(&directory, &entry, cancel)? {
            return Ok(false);
        }
    }
    parent.rmdir(name)?;
    Ok(true)
}

#[cfg(unix)]
fn clone_registry(
    source: &Path,
    destination: &provenant_os::fs::Directory,
    cancel: &provenant_core::CancelToken,
) -> Result<(), &'static str> {
    clone_tree(
        source,
        destination,
        &CloneStore {
            unavailable: "registry_unavailable",
            read_failed: "registry_read_failed",
            too_deep: "registry_too_deep",
            unsupported: "registry_contains_unsupported_entry",
            writable_files: false,
        },
        cancel,
    )
}

#[cfg(unix)]
fn clone_git(
    source: &Path,
    destination: &provenant_os::fs::Directory,
    cancel: &provenant_core::CancelToken,
) -> Result<(), &'static str> {
    clone_tree(
        source,
        destination,
        &CloneStore {
            unavailable: "git_unavailable",
            read_failed: "git_read_failed",
            too_deep: "git_too_deep",
            unsupported: "git_contains_unsupported_entry",
            writable_files: false,
        },
        cancel,
    )
}

/// One seeded store: its refusal codes, and whether clones must be writable.
#[cfg(unix)]
struct CloneStore {
    unavailable: &'static str,
    read_failed: &'static str,
    too_deep: &'static str,
    unsupported: &'static str,
    writable_files: bool,
}

/// Clone one entry at a time so cancellation can interrupt large stores.
#[cfg(unix)]
fn clone_tree(
    source: &Path,
    destination: &provenant_os::fs::Directory,
    store: &CloneStore,
    cancel: &provenant_core::CancelToken,
) -> Result<(), &'static str> {
    let source = provenant_os::fs::Directory::open_anchor(source).map_err(|_| store.unavailable)?;
    clone_entries(&source, destination, store, cancel, 0)
}

#[cfg(unix)]
fn clone_entries(
    source: &provenant_os::fs::Directory,
    destination: &provenant_os::fs::Directory,
    store: &CloneStore,
    cancel: &provenant_core::CancelToken,
    depth: usize,
) -> Result<(), &'static str> {
    use provenant_os::fs::FileKind;
    if depth > 128 {
        return Err(store.too_deep);
    }
    for entry in source.entries().map_err(|_| store.read_failed)? {
        if cancel.is_cancelled() {
            return Err("cancelled");
        }
        let name = entry.map_err(|_| store.read_failed)?;
        match source
            .metadata_at(&name, false)
            .map_err(|_| store.read_failed)?
            .kind
        {
            FileKind::Directory => {
                let child = source
                    .open_verified_directory(&name)
                    .map_err(|_| store.read_failed)?;
                destination
                    .mkdir(&name, 0o700)
                    .map_err(|_| "clone_failed")?;
                let target = destination
                    .open_verified_directory(&name)
                    .map_err(|_| "clone_failed")?;
                clone_entries(&child, &target, store, cancel, depth + 1)?;
            }
            FileKind::Regular => {
                let file = source.open_read(&name).map_err(|_| store.read_failed)?;
                destination
                    .clone_file_from(&file, &name)
                    .map_err(|_| "clone_failed")?;
                if store.writable_files {
                    destination
                        .open_read(&name)
                        .and_then(|file| file.make_owner_writable())
                        .map_err(|_| "clone_failed")?;
                }
            }
            _ => return Err(store.unsupported),
        }
    }
    if cancel.is_cancelled() {
        return Err("cancelled");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_preserves_private_staging_and_next_seed_cleans_it() {
        let tmp = tempfile::tempdir().unwrap();
        let cancel = provenant_core::CancelToken::new();
        let result = seed_store_cancellable(
            tmp.path(),
            tmp.path(),
            "registry",
            Some(&cancel),
            |_, stage| {
                stage.mkdir(std::ffi::OsStr::new("partial"), 0o700).unwrap();
                cancel.cancel();
                Err("cancelled")
            },
        );
        assert_eq!(result.reason, "cancelled");
        let cargo = tmp.path().join("cargo-home");
        assert!(std::fs::read_dir(&cargo).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".registry-seed-")
        }));
        let result = seed_store_cancellable(tmp.path(), tmp.path(), "registry", None, |_, _| {
            Err("clone_failed")
        });
        assert_eq!(result.reason, "clone_failed");
        assert!(!std::fs::read_dir(&cargo).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".registry-seed-")
        }));
    }

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
    fn detail_codes_are_snake_case_and_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for code in codes::ALL {
            assert!(
                !code.is_empty()
                    && !code.starts_with('_')
                    && !code.ends_with('_')
                    && !code.contains("__")
                    && code.bytes().all(|byte| byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'_'),
                "{code} is not snake_case"
            );
            assert!(seen.insert(*code), "{code} is listed twice");
        }
        assert!(codes::ALL.len() > 40);
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

    // Acceptance 2 of #2484: on APFS, an enabled writer with a matching host
    // generation and a verified fixed-release pin reports `seeded`.
    #[cfg(target_os = "macos")]
    #[test]
    fn enabled_writer_with_a_verified_pin_is_seeded_on_apfs() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let worktree = tmp.path().join("writer");
        let cache = tmp.path().join("cache");
        let state = tmp.path().join("state");
        let bin = home.join(".rustup/toolchains/1.99.1-test-host/bin");
        std::fs::create_dir_all(worktree.join("tools/front-door")).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(home.join(".rustup/settings.toml"), "[overrides]\n").unwrap();
        std::fs::write(
            worktree.join("tools/front-door/rust-toolchain.toml"),
            "[toolchain]\nchannel = '1.99.1'\n",
        )
        .unwrap();
        let report = "rustc 1.99.1\nhost: test-host\nrelease: 1.99.1\n";
        let rustc = bin.join("rustc");
        std::fs::write(
            &rustc,
            format!("#!/bin/sh\nprintf '{}'\n", report.replace('\n', "\\n")),
        )
        .unwrap();
        std::fs::set_permissions(&rustc, std::fs::Permissions::from_mode(0o755)).unwrap();
        generation(&state, "001", report, "test-host");
        let env = [
            ("RUSTUP_HOME".into(), home.join(".rustup").into_os_string()),
            ("RUSTUP_AUTO_INSTALL".into(), "0".into()),
        ];
        let outcome = seed_target(
            true,
            &state,
            &cache,
            &worktree,
            &home,
            &env,
            &provenant_core::CancelToken::new(),
        );
        assert_eq!((outcome.status, outcome.reason), ("seeded", "cloned"));
        assert!(cache.join("target").is_dir());
        // A refusal records its code and nothing from the environment.
        let refused = [("RUSTC".into(), "/secret/rustc".into())];
        let outcome = seed_target(
            true,
            &state,
            &cache,
            &worktree,
            &home,
            &refused,
            &provenant_core::CancelToken::new(),
        );
        assert_eq!(
            outcome,
            Outcome::fallback_detail("toolchain_unverified", codes::TOOLCHAIN_ENV_OVERRIDE)
        );
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
            seed_target_with(tmp.path(), &cache, "v1", "host", |src, dst| clone_target(
                src,
                dst,
                &provenant_core::CancelToken::new()
            ))
            .status,
            "seeded"
        );
        std::fs::write(cache.join("target/file"), b"lane").unwrap();
        assert_eq!(std::fs::read(&seed).unwrap(), b"seed");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn target_clone_of_a_read_only_tree_is_private_and_writable() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        generation(tmp.path(), "001", "v1", "host");
        let target = crate::target_seed::store_path(tmp.path()).join("generations/001/target");
        let nested = target.join("debug/deps");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("artifact"), b"seed").unwrap();
        let read_only = |path: &Path, mode| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        read_only(&nested.join("artifact"), 0o444);
        read_only(&nested, 0o555);
        read_only(&target.join("debug"), 0o555);
        assert_eq!(
            seed_target_with(tmp.path(), &cache, "v1", "host", |src, dst| clone_target(
                src,
                dst,
                &provenant_core::CancelToken::new()
            ))
            .status,
            "seeded"
        );
        let lane = cache.join("target/debug/deps");
        std::fs::write(lane.join("artifact"), b"lane").unwrap();
        std::fs::write(lane.join("new"), b"lane").unwrap();
        assert_eq!(std::fs::read(nested.join("artifact")).unwrap(), b"seed");
        assert!(!nested.join("new").exists());
        read_only(&target.join("debug"), 0o755);
        read_only(&nested, 0o755);
    }

    #[test]
    fn disabled_does_not_touch_cache() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            seed_registry(
                false,
                tmp.path(),
                tmp.path(),
                &provenant_core::CancelToken::new()
            ),
            Outcome::skipped("disabled")
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
    fn existing_registry_does_not_walk_stale_staging() {
        let tmp = tempfile::tempdir().unwrap();
        let cargo = tmp.path().join("cargo-home");
        std::fs::create_dir_all(cargo.join("registry")).unwrap();
        let stale = cargo.join(".registry-seed-stale");
        std::fs::create_dir(&stale).unwrap();
        std::fs::write(stale.join("keep"), b"untouched").unwrap();
        assert_eq!(
            seed_with(tmp.path(), tmp.path(), |_, _| panic!("clone")),
            Outcome::skipped("existing_registry")
        );
        assert_eq!(std::fs::read(stale.join("keep")).unwrap(), b"untouched");
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
        assert_eq!(
            seed_git(true, &home, &cache, &provenant_core::CancelToken::new()).status,
            "seeded"
        );
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
            seed_git(true, &home, &cache, &provenant_core::CancelToken::new()),
            Outcome::fallback("git_unavailable")
        );
        std::fs::create_dir_all(home.join(".cargo/git/checkouts")).unwrap();
        std::os::unix::fs::symlink("/outside", home.join(".cargo/git/checkouts/link")).unwrap();
        assert_eq!(
            seed_git(true, &home, &cache, &provenant_core::CancelToken::new()),
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
        use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
        use provenant_os::fixture::{git_env, run_git};
        use std::ffi::OsString;
        use std::time::Duration;

        // Cargo runs with the fixture env: a lane's GIT_DIR must not reach its Git fetch.
        let run = |argv: &[OsString], cwd: &std::path::Path, overrides: &[(&str, OsString)]| {
            let mut env = git_env();
            for (key, value) in overrides {
                env.retain(|(existing, _)| existing != key);
                env.push(((*key).into(), value.clone()));
            }
            let mut options = ExecOptions::new(argv, cwd, Duration::from_secs(120));
            options.env = Some(&env);
            let outcome = SystemExecutor::new(1).unwrap().execute(options).unwrap();
            assert_eq!(outcome.termination, Termination::Exit(0));
            assert!(!outcome.timed_out);
        };
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
        run_git(&dep, &["init", "-q"]).unwrap();
        run_git(&dep, &["add", "."]).unwrap();
        run_git(&dep, &["commit", "-qm", "fixture"]).unwrap();
        std::fs::write(app.join("Cargo.toml"), format!("[package]\nname = \"seed_app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\nseed_fixture = {{ git = \"file://{}\" }}\n", dep.display())).unwrap();
        std::fs::write(
            app.join("src/lib.rs"),
            "pub fn value() -> u8 { seed_fixture::value() }\n",
        )
        .unwrap();
        run(
            &["cargo".into(), "fetch".into()],
            &app,
            &[
                ("CARGO_HOME", home.join(".cargo").into()),
                ("CARGO_NET_GIT_FETCH_WITH_CLI", "true".into()),
            ],
        );
        assert_eq!(
            seed_git(true, &home, &cache, &provenant_core::CancelToken::new()).status,
            "seeded"
        );
        run(
            &["cargo".into(), "check".into(), "--offline".into()],
            &app,
            &[
                ("CARGO_HOME", cache.join("cargo-home").into()),
                ("CARGO_TARGET_DIR", cache.join("target").into()),
                ("CARGO_BUILD_BUILD_DIR", cache.join("target").into()),
            ],
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
        assert_eq!(
            seed_registry(true, &home, &cache, &provenant_core::CancelToken::new()).status,
            "seeded"
        );
        std::fs::write(cache.join("cargo-home/registry/index/item"), b"lane").unwrap();
        assert_eq!(
            std::fs::read(home.join(".cargo/registry/index/item")).unwrap(),
            b"host"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn registry_clone_copies_every_nested_file() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cache = tmp.path().join("cache");
        let nested = home.join(".cargo/registry/src/index/crate-1.0/src");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(&cache).unwrap();
        std::fs::write(nested.join("lib.rs"), b"host").unwrap();
        std::fs::write(home.join(".cargo/registry/top"), b"top").unwrap();
        assert_eq!(
            seed_registry(true, &home, &cache, &provenant_core::CancelToken::new()).status,
            "seeded"
        );
        let seeded = cache.join("cargo-home/registry");
        assert_eq!(
            std::fs::read(seeded.join("src/index/crate-1.0/src/lib.rs")).unwrap(),
            b"host"
        );
        assert_eq!(std::fs::read(seeded.join("top")).unwrap(), b"top");
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
            seed_registry(true, &home, &cache, &provenant_core::CancelToken::new()),
            Outcome::fallback("registry_contains_unsupported_entry")
        );
        assert!(!cache.join("cargo-home/registry").exists());
    }
}
