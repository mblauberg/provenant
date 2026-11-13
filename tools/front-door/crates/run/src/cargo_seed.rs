//! Shared lane Cargo downloads and best-effort private target seeds.
use provenant_core::{Clock, Deadline, DurationMicros, WaitBudget};
use provenant_os::clock::SystemClock;
use std::path::Path;

/// The filesystem preparation edge samples time through the shared clock.
/// Carry one clock and deadline through selection, cloning and cleanup.
struct SeedBudget {
    clock: SystemClock,
    deadline: Deadline,
}
impl SeedBudget {
    fn new() -> Self {
        let clock = SystemClock::new();
        let deadline =
            WaitBudget::up_to(clock.mono_now(), DurationMicros::from_micros(2_000_000)).deadline();
        Self { clock, deadline }
    }
    fn expired(&self) -> bool {
        self.deadline.is_expired(self.clock.mono_now())
    }
    #[cfg(test)]
    fn exhausted() -> Self {
        let clock = SystemClock::new();
        let deadline = WaitBudget::nonblocking(provenant_core::MonoTime::from_micros(0)).deadline();
        Self { clock, deadline }
    }
}

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
    share_store(home, cache, "registry", cancel)
}

/// Git downloads share the same lane-only store and Cargo locks as the registry.
pub(crate) fn seed_git(
    enabled: bool,
    home: &Path,
    cache: &Path,
    cancel: &provenant_core::CancelToken,
) -> Outcome {
    if !enabled {
        return Outcome::skipped("disabled");
    }
    share_store(home, cache, "git", cancel)
}

fn share_store(
    home: &Path,
    cache: &Path,
    store: &str,
    cancel: &provenant_core::CancelToken,
) -> Outcome {
    if cancel.is_cancelled() {
        return Outcome::fallback("cancelled");
    }
    #[cfg(unix)]
    {
        use provenant_os::fs::Directory;
        use std::ffi::OsStr;
        let result = (|| {
            // Separate from ~/.cargo: lanes must never mutate sources the host
            // later builds outside confinement. Only downloads and their locks
            // are shared; configuration and credential state stay private.
            let mut shared = Directory::open_anchor(home).map_err(|_| "home_unavailable")?;
            for name in ["Library", "Caches", "provenant-cargo-downloads"] {
                let name = OsStr::new(name);
                if let Err(error) = shared.mkdir(name, 0o700)
                    && !error.is_exists()
                {
                    return Err("shared_store_unavailable");
                }
                shared = shared
                    .open_verified_directory(name)
                    .map_err(|_| "shared_store_unavailable")?;
            }
            let name = OsStr::new(store);
            if let Err(error) = shared.mkdir(name, 0o700)
                && !error.is_exists()
            {
                return Err("shared_store_unavailable");
            }
            shared
                .open_verified_directory(name)
                .map_err(|_| "shared_store_unavailable")?;
            let cache = Directory::open_anchor(cache).map_err(|_| "cache_unavailable")?;
            let name = OsStr::new("cargo-home");
            if let Err(error) = cache.mkdir(name, 0o700)
                && !error.is_exists()
            {
                return Err("cargo_home_unavailable");
            }
            let cargo = cache
                .open_verified_directory(name)
                .map_err(|_| "cargo_home_unavailable")?;
            cargo.make_private().map_err(|_| "cargo_home_not_private")?;
            // Cargo serialises cache mutations via these locks, which must name
            // the same inodes in every private home that uses the shared stores.
            for lock in [".package-cache", ".package-cache-mutate"] {
                let name = OsStr::new(lock);
                if let Err(error) = shared.create_new(name)
                    && !error.is_exists()
                {
                    return Err("shared_lock_unavailable");
                }
                shared
                    .open_read(name)
                    .map_err(|_| "shared_lock_unavailable")?;
            }
            // Preflight every entry before publishing any link. An old private
            // package-cache lock must never accompany a newly shared registry.
            for entry in [store, ".package-cache", ".package-cache-mutate"] {
                let name = OsStr::new(entry);
                let target = home
                    .join("Library/Caches/provenant-cargo-downloads")
                    .join(entry);
                match cargo.metadata_at(name, false) {
                    Ok(_) if cargo.read_link(name).is_ok_and(|path| path == target) => {}
                    Ok(_) => return Err("existing_private_store"),
                    Err(error) if error.is_not_found() => {}
                    Err(_) => return Err("shared_link_failed"),
                }
            }
            // Locks first: a failed download link leaves only harmless shared
            // lock links, never shared downloads without shared locking.
            for entry in [".package-cache", ".package-cache-mutate", store] {
                let name = OsStr::new(entry);
                let target = home
                    .join("Library/Caches/provenant-cargo-downloads")
                    .join(entry);
                match cargo.symlink(&target, name) {
                    Ok(()) => {}
                    Err(error)
                        if error.is_exists()
                            && cargo.read_link(name).is_ok_and(|path| path == target) => {}
                    Err(_) => return Err("shared_link_failed"),
                }
            }

            Ok(())
        })();
        match result {
            Ok(()) => Outcome {
                status: "shared",
                reason: "lane_download_cache",
                detail: None,
            },
            Err("existing_private_store") => Outcome::skipped("existing_private_store"),
            Err(reason) => Outcome::fallback(reason),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (home, cache, store);
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
            |source, dest, deadline| clone_target_until(source, dest, cancel, deadline),
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
    seed_target_cancellable(state, cache, version, triple, None, |source, dest, _| {
        clone(source, dest)
    })
}

#[cfg(any(unix, test))]
fn seed_target_cancellable(
    state: &Path,
    cache: &Path,
    version: &str,
    triple: &str,
    cancel: Option<&provenant_core::CancelToken>,
    clone: impl FnOnce(&Path, &provenant_os::fs::Directory, &SeedBudget) -> Result<(), &'static str>,
) -> Outcome {
    let budget = SeedBudget::new();
    let deadline = &budget;
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
        if deadline.expired() {
            return Outcome::fallback("seed_budget_exhausted");
        }
        if cancel.is_some_and(|token| token.is_cancelled()) {
            return Outcome::fallback("cancelled");
        }
        let Ok(stage) = stage else {
            return Outcome::fallback("staging_cleanup_failed");
        };
        if stage.to_string_lossy().starts_with(".target-seed-") {
            match remove_seed_tree(&cache_dir, &stage, cancel, deadline) {
                Ok(true) => {}
                Ok(false) => {
                    return Outcome::fallback(
                        if cancel.is_some_and(|token| token.is_cancelled()) {
                            "cancelled"
                        } else {
                            "seed_budget_exhausted"
                        },
                    );
                }
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
        if deadline.expired() {
            return Outcome::fallback("seed_budget_exhausted");
        }
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
        clone(&source, &staging, deadline)?;
        if deadline.expired() {
            return Err("seed_budget_exhausted");
        }
        // Publish only a complete private target.
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
        Err(reason @ ("cancelled" | "seed_budget_exhausted")) => Outcome::fallback(reason),
        Err(reason) => {
            match remove_seed_tree(&cache_dir, &stage, cancel, deadline) {
                Ok(true) => {}
                Ok(false) => {
                    return Outcome::fallback(
                        if cancel.is_some_and(|token| token.is_cancelled()) {
                            "cancelled"
                        } else {
                            "seed_budget_exhausted"
                        },
                    );
                }
                Err(_) => return Outcome::fallback("staging_cleanup_failed"),
            }
            Outcome::fallback(reason)
        }
    }
}

#[cfg(all(target_os = "macos", test))]
fn clone_target(
    source: &Path,
    destination: &provenant_os::fs::Directory,
    cancel: &provenant_core::CancelToken,
) -> Result<(), &'static str> {
    clone_target_until(source, destination, cancel, &SeedBudget::new())
}

#[cfg(unix)]
fn clone_target_until(
    source: &Path,
    destination: &provenant_os::fs::Directory,
    cancel: &provenant_core::CancelToken,
    deadline: &SeedBudget,
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
            deadline,
        },
        cancel,
    )
}

#[cfg(any(unix, test))]
fn remove_seed_tree(
    parent: &provenant_os::fs::Directory,
    name: &std::ffi::OsStr,
    cancel: Option<&provenant_core::CancelToken>,
    deadline: &SeedBudget,
) -> provenant_os::fs::Result<bool> {
    use provenant_os::fs::FileKind;
    if deadline.expired() || cancel.is_some_and(|token| token.is_cancelled()) {
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
        if deadline.expired() || cancel.is_some_and(|token| token.is_cancelled()) {
            return Ok(false);
        }
        entries.push(entry?);
    }
    for entry in entries {
        if !remove_seed_tree(&directory, &entry, cancel, deadline)? {
            return Ok(false);
        }
    }
    parent.rmdir(name)?;
    Ok(true)
}

/// One seeded store: its refusal codes, and whether clones must be writable.
#[cfg(unix)]
struct CloneStore<'a> {
    unavailable: &'static str,
    read_failed: &'static str,
    too_deep: &'static str,
    unsupported: &'static str,
    writable_files: bool,
    deadline: &'a SeedBudget,
}

/// Clone one entry at a time so cancellation can interrupt large stores.
#[cfg(unix)]
fn clone_tree(
    source: &Path,
    destination: &provenant_os::fs::Directory,
    store: &CloneStore<'_>,
    cancel: &provenant_core::CancelToken,
) -> Result<(), &'static str> {
    if store.deadline.expired() {
        return Err("seed_budget_exhausted");
    }
    let source = provenant_os::fs::Directory::open_anchor(source).map_err(|_| store.unavailable)?;
    clone_entries(&source, destination, store, cancel, 0)
}

#[cfg(unix)]
fn clone_entries(
    source: &provenant_os::fs::Directory,
    destination: &provenant_os::fs::Directory,
    store: &CloneStore<'_>,
    cancel: &provenant_core::CancelToken,
    depth: usize,
) -> Result<(), &'static str> {
    use provenant_os::fs::FileKind;
    if store.deadline.expired() {
        return Err("seed_budget_exhausted");
    }
    if depth > 128 {
        return Err(store.too_deep);
    }
    for entry in source.entries().map_err(|_| store.read_failed)? {
        if store.deadline.expired() {
            return Err("seed_budget_exhausted");
        }
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

    #[cfg(unix)]
    #[test]
    fn expired_seed_budget_does_not_walk_or_clone() {
        let tmp = tempfile::tempdir().unwrap();
        let directory = provenant_os::fs::Directory::open_anchor(tmp.path()).unwrap();
        assert_eq!(
            clone_target_until(
                Path::new("/missing"),
                &directory,
                &provenant_core::CancelToken::new(),
                &SeedBudget::exhausted()
            ),
            Err("seed_budget_exhausted")
        );
        assert!(std::fs::read_dir(tmp.path()).unwrap().next().is_none());
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

    #[cfg(unix)]
    #[test]
    fn download_stores_are_shared_without_reading_host_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join(".cargo/registry")).unwrap();
        // A source entry that the old recursive clone would reject. Sharing
        // neither reads nor copies anything from the host's registry.
        std::os::unix::fs::symlink("/unavailable", home.join(".cargo/registry/link")).unwrap();
        let cancel = provenant_core::CancelToken::new();
        for lane in ["one", "two"] {
            let cache = tmp.path().join(lane);
            std::fs::create_dir(&cache).unwrap();
            for store in ["registry", "git"] {
                assert_eq!(share_store(&home, &cache, store, &cancel).status, "shared");
                assert_eq!(
                    std::fs::read_link(cache.join("cargo-home").join(store)).unwrap(),
                    home.join("Library/Caches/provenant-cargo-downloads")
                        .join(store)
                );
            }
            for lock in [".package-cache", ".package-cache-mutate"] {
                assert_eq!(
                    std::fs::read_link(cache.join("cargo-home").join(lock)).unwrap(),
                    home.join("Library/Caches/provenant-cargo-downloads")
                        .join(lock)
                );
            }
            assert!(!cache.join("cargo-home/registry/link").exists());
        }
        let one = tmp.path().join("one/cargo-home/registry");
        let two = tmp.path().join("two/cargo-home/registry");
        std::fs::write(one.join("download"), b"shared").unwrap();
        assert_eq!(std::fs::read(two.join("download")).unwrap(), b"shared");
        assert!(!home.join(".cargo/registry/download").exists());
    }

    #[cfg(unix)]
    #[test]
    fn existing_private_downloads_are_retained_without_a_walk() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("cargo-home/registry")).unwrap();
        std::fs::write(tmp.path().join("cargo-home/registry/keep"), b"keep").unwrap();
        assert_eq!(
            seed_registry(
                true,
                tmp.path(),
                tmp.path(),
                &provenant_core::CancelToken::new()
            ),
            Outcome::skipped("existing_private_store")
        );
        assert_eq!(
            std::fs::read(tmp.path().join("cargo-home/registry/keep")).unwrap(),
            b"keep"
        );
        std::fs::write(
            tmp.path().join("cargo-home/.package-cache"),
            b"private lock",
        )
        .unwrap();
        assert_eq!(
            seed_git(
                true,
                tmp.path(),
                tmp.path(),
                &provenant_core::CancelToken::new()
            ),
            Outcome::skipped("existing_private_store")
        );
        assert!(std::fs::symlink_metadata(tmp.path().join("cargo-home/git")).is_err());
    }
}
