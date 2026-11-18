//! Best-effort private target seeds from host-built generations.
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
    CARGO_WORKSPACE_UNREADABLE = "cargo_workspace_unreadable",
    CARGO_WORKSPACE_MISSING = "cargo_workspace_missing",
    CARGO_CONFIG_UNREADABLE = "cargo_config_unreadable",
    CARGO_CONFIG_TOO_LARGE = "cargo_config_too_large",
    CARGO_CONFIG_INVALID = "cargo_config_invalid",
    CARGO_CONFIG_SELECTS_COMPILER = "cargo_config_selects_compiler",
    CARGO_CONFIG_CHANGES_BUILD_TARGET = "cargo_config_changes_build_target",
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

/// Refuse bounded Cargo config input when it can alter the rustc, target, or
/// build flags represented by a shared target seed. Used by both lane setup
/// and host seed currentness; values are never returned or logged.
pub(crate) fn cargo_config_seed_eligibility(bytes: &[u8]) -> Result<(), &'static str> {
    if bytes.len() > 65536 {
        return Err(codes::CARGO_CONFIG_TOO_LARGE);
    }
    let document: toml_edit::DocumentMut = String::from_utf8(bytes.to_vec())
        .map_err(|_| codes::CARGO_CONFIG_INVALID)?
        .parse()
        .map_err(|_| codes::CARGO_CONFIG_INVALID)?;
    let compiler_key = |key: &str| {
        key == "PATH"
            || key.starts_with("RUSTUP_")
            || key.starts_with("RUSTC")
            || key.starts_with("CARGO_BUILD_RUSTC")
    };
    let target_key = |key: &str| {
        matches!(
            key,
            "CARGO_BUILD_TARGET"
                | "RUSTFLAGS"
                | "CARGO_ENCODED_RUSTFLAGS"
                | "CARGO_BUILD_RUSTFLAGS"
        ) || key.starts_with("CARGO_TARGET_")
            || (key.starts_with("CARGO_PROFILE_") && key != "CARGO_PROFILE_DEV_DEBUG")
    };
    let build = document.get("build").and_then(|item| item.as_table_like());
    let env = document.get("env").and_then(|item| item.as_table_like());
    if document.contains_key("include")
        || build.is_some_and(|table| table.iter().any(|(key, _)| key.starts_with("rustc")))
        || env.is_some_and(|table| table.iter().any(|(key, _)| compiler_key(key)))
        || (document.contains_key("build") && build.is_none())
        || (document.contains_key("env") && env.is_none())
    {
        return Err(codes::CARGO_CONFIG_SELECTS_COMPILER);
    }

    let build_changes_target = build.is_some_and(|table| {
        table
            .iter()
            .any(|(key, _)| matches!(key, "target" | "rustflags" | "rustdocflags" | "incremental"))
    });
    let env_changes_target = env.is_some_and(|table| table.iter().any(|(key, _)| target_key(key)));
    let target_changes_build = match document.get("target") {
        None => false,
        Some(item) => item.as_table_like().is_none_or(|targets| {
            targets.iter().any(|(_, settings)| {
                settings.as_table_like().is_none_or(|settings| {
                    settings
                        .iter()
                        .any(|(key, _)| matches!(key, "linker" | "ar" | "rustflags"))
                })
            })
        }),
    };
    if build_changes_target || env_changes_target || target_changes_build {
        return Err(codes::CARGO_CONFIG_CHANGES_BUILD_TARGET);
    }
    Ok(())
}

/// Every reason persisted in `cargo-target-setup.json` when target seeding
/// falls back. Keep call sites and the coverage table on this declaration.
macro_rules! fallback_reasons {
    ($($name:ident = $text:literal),* $(,)?) => {
        mod fallback_reasons {
            $(pub(super) const $name: &str = $text;)*
            #[cfg(test)]
            pub(super) const ALL: &[&str] = &[$($name),*];
        }
    };
}
fallback_reasons! {
    CACHE_UNAVAILABLE = "cache_unavailable",
    TARGET_UNAVAILABLE = "target_unavailable",
    SEED_LOCK_UNAVAILABLE = "seed_lock_unavailable",
    STAGING_CLEANUP_FAILED = "staging_cleanup_failed",
    SEED_BUDGET_EXHAUSTED = "seed_budget_exhausted",
    CANCELLED = "cancelled",
    STORE_UNAVAILABLE = "store_unavailable",
    NO_GENERATIONS = "no_generations",
    MANIFEST_INVALID = "manifest_invalid",
    TOOLCHAIN_MISMATCH = "toolchain_mismatch",
    TRIPLE_MISMATCH = "triple_mismatch",
    CARGO_PROFILE_MISMATCH = "cargo_profile_mismatch",
    CARGO_PROFILE_UNREPRESENTABLE = "cargo_profile_unrepresentable",
    STAGING_UNAVAILABLE = "staging_unavailable",
    STAGING_SYNC_FAILED = "staging_sync_failed",
    PUBLISH_FAILED = "publish_failed",
    TARGET_READ_FAILED = "target_read_failed",
    TARGET_TOO_DEEP = "target_too_deep",
    TARGET_CONTAINS_UNSUPPORTED_ENTRY = "target_contains_unsupported_entry",
    CLONE_FAILED = "clone_failed",
    TOOLCHAIN_UNVERIFIED = "toolchain_unverified",
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Outcome {
    status: String,
    reason: String,
    /// Stable snake_case code for the specific refusal behind `reason`. Never
    /// an environment value or path.
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    /// Path-free receipt proving which complete generation supplied a seed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seed_source: Option<SeedSource>,
    #[serde(flatten)]
    extra: provenant_records::ExtraFields,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct SeedSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generation_id: Option<String>,
    project_match: bool,
}

struct SeedCandidate {
    target: std::path::PathBuf,
    generation_id: Option<String>,
    project_match: bool,
}

fn safe_generation_id(name: &std::ffi::OsStr) -> Option<String> {
    let name = name.to_str()?;
    (!name.is_empty()
        && name.len() <= 96
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-')))
    .then(|| name.to_owned())
}

impl Outcome {
    pub(crate) fn skipped(reason: &'static str) -> Self {
        Self {
            status: "skipped".into(),
            reason: reason.into(),
            detail: None,
            seed_source: None,
            extra: Default::default(),
        }
    }
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn fallback(reason: &'static str) -> Self {
        Self {
            status: "fallback".into(),
            reason: reason.into(),
            detail: None,
            seed_source: None,
            extra: Default::default(),
        }
    }
    /// One line for the host's stderr when a seed was wanted and not used, so
    /// a lane that builds its dependencies from scratch is not a surprise.
    pub(crate) fn fallback_warning(&self, what: &str) -> Option<String> {
        (self.status == "fallback").then(|| {
            format!(
                "warning: {what} not seeded ({}{}); this lane builds without it",
                self.reason,
                self.detail
                    .as_deref()
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default()
            )
        })
    }
    pub(crate) fn fallback_detail(reason: &'static str, detail: &'static str) -> Self {
        Self {
            status: "fallback".into(),
            reason: reason.into(),
            detail: Some(detail.into()),
            seed_source: None,
            extra: Default::default(),
        }
    }
}

pub(crate) fn seed_target(
    state: &Path,
    cache: &Path,
    worktree: &Path,
    project_key: &str,
    home: &Path,
    environment: &[(std::ffi::OsString, std::ffi::OsString)],
    cancel: &provenant_core::CancelToken,
) -> Outcome {
    seed_target_with_reservation(
        state,
        cache,
        worktree,
        project_key,
        home,
        environment,
        cancel,
        |_| {},
    )
}

pub(crate) fn seed_target_with_reservation(
    state: &Path,
    cache: &Path,
    worktree: &Path,
    project_key: &str,
    home: &Path,
    environment: &[(std::ffi::OsString, std::ffi::OsString)],
    cancel: &provenant_core::CancelToken,
    before_clone: impl FnOnce(Option<u64>),
) -> Outcome {
    #[cfg(unix)]
    {
        let settings = provenant_config::LaneBuildSettings::default();
        let Some(cargo_profile) = settings.cargo_build_profile(environment) else {
            return Outcome::fallback(fallback_reasons::CARGO_PROFILE_UNREPRESENTABLE);
        };
        let (version, triple) = match crate::target_seed::lane_rustc_metadata_cancellable(
            worktree,
            home,
            state,
            environment,
            Some(cancel),
        ) {
            Ok(metadata) => metadata,
            Err(detail) => {
                return Outcome::fallback_detail(fallback_reasons::TOOLCHAIN_UNVERIFIED, detail);
            }
        };
        seed_target_cancellable(
            state,
            cache,
            SeedCompatibility {
                version: &version,
                triple: &triple,
                cargo_profile: &cargo_profile,
                project_key,
            },
            Some(cancel),
            before_clone,
            |source, dest, deadline| clone_target_until(source, dest, cancel, deadline),
        )
    }
    #[cfg(not(unix))]
    {
        let _ = (
            state,
            cache,
            worktree,
            project_key,
            home,
            environment,
            cancel,
            before_clone,
        );
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
    let cargo_profile = provenant_config::LaneBuildSettings::default()
        .cargo_build_profile(&[])
        .expect("default Cargo profile is UTF-8");
    seed_target_cancellable(
        state,
        cache,
        SeedCompatibility {
            version,
            triple,
            cargo_profile: &cargo_profile,
            project_key: "",
        },
        None,
        |_| {},
        |source, dest, _| clone(source, dest),
    )
}

#[cfg(any(unix, test))]
struct SeedCompatibility<'a> {
    version: &'a str,
    triple: &'a str,
    cargo_profile: &'a provenant_config::CargoBuildProfile,
    project_key: &'a str,
}

#[cfg(any(unix, test))]
fn seed_target_cancellable(
    state: &Path,
    cache: &Path,
    compatibility: SeedCompatibility<'_>,
    cancel: Option<&provenant_core::CancelToken>,
    before_clone: impl FnOnce(Option<u64>),
    clone: impl FnOnce(&Path, &provenant_os::fs::Directory, &SeedBudget) -> Result<(), &'static str>,
) -> Outcome {
    let budget = SeedBudget::new();
    let deadline = &budget;
    use provenant_os::fs::{Directory, temporary_name};
    use std::ffi::{OsStr, OsString};
    let Ok(cache_dir) = Directory::open_anchor(cache) else {
        return Outcome::fallback(fallback_reasons::CACHE_UNAVAILABLE);
    };
    match cache_dir.metadata_at(OsStr::new("target"), false) {
        Ok(_) => return Outcome::skipped("existing_target"),
        Err(error) if error.is_not_found() => {}
        Err(_) => return Outcome::fallback(fallback_reasons::TARGET_UNAVAILABLE),
    }
    // Held until the entire clone is published or removed: GC cannot unlink its source.
    let Ok(_lock) = crate::target_seed::lock_store(state) else {
        return Outcome::fallback(fallback_reasons::SEED_LOCK_UNAVAILABLE);
    };
    // An interrupted clone leaves a private stage; remove it under the same
    // store lock before attempting another target seed.
    let Ok(stages) = cache_dir.entries() else {
        return Outcome::fallback(fallback_reasons::STAGING_CLEANUP_FAILED);
    };
    for stage in stages {
        if deadline.expired() {
            return Outcome::fallback(fallback_reasons::SEED_BUDGET_EXHAUSTED);
        }
        if cancel.is_some_and(|token| token.is_cancelled()) {
            return Outcome::fallback(fallback_reasons::CANCELLED);
        }
        let Ok(stage) = stage else {
            return Outcome::fallback(fallback_reasons::STAGING_CLEANUP_FAILED);
        };
        if stage.to_string_lossy().starts_with(".target-seed-") {
            match remove_seed_tree(&cache_dir, &stage, cancel, deadline) {
                Ok(true) => {}
                Ok(false) => {
                    return Outcome::fallback(
                        if cancel.is_some_and(|token| token.is_cancelled()) {
                            fallback_reasons::CANCELLED
                        } else {
                            fallback_reasons::SEED_BUDGET_EXHAUSTED
                        },
                    );
                }
                Err(_) => return Outcome::fallback(fallback_reasons::STAGING_CLEANUP_FAILED),
            }
        }
    }
    let generations = crate::target_seed::store_path(state).join("generations");
    let Ok(entries) = std::fs::read_dir(generations) else {
        return Outcome::fallback(fallback_reasons::STORE_UNAVAILABLE);
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
    let mut saw_version = false;
    let mut saw_triple = false;
    let mut saw_cargo_profile = false;
    let mut saw_manifest = false;
    let mut saw_entry = false;
    let mut selected_project = None;
    let mut selected_shared = None;
    for entry in entries {
        if deadline.expired() {
            return Outcome::fallback(fallback_reasons::SEED_BUDGET_EXHAUSTED);
        }
        if cancel.is_some_and(|token| token.is_cancelled()) {
            return Outcome::fallback(fallback_reasons::CANCELLED);
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
        if manifest.rustc_vv != compatibility.version {
            continue;
        }
        saw_version = true;
        if manifest.host_triple != compatibility.triple {
            continue;
        }
        saw_triple = true;
        if &manifest.cargo_profile != compatibility.cargo_profile {
            continue;
        }
        saw_cargo_profile = true;
        let project_match = !compatibility.project_key.is_empty()
            && manifest.project_key == compatibility.project_key;
        let candidate = SeedCandidate {
            target: entry.path().join("target"),
            generation_id: safe_generation_id(&entry.file_name()),
            project_match,
        };
        if project_match {
            selected_project = Some(candidate);
            break;
        }
        if selected_shared.is_none() {
            selected_shared = Some(candidate);
        }
    }
    let Some(selected) = selected_project.or(selected_shared) else {
        return Outcome::fallback(if !saw_entry {
            fallback_reasons::NO_GENERATIONS
        } else if !saw_manifest {
            fallback_reasons::MANIFEST_INVALID
        } else if !saw_version {
            fallback_reasons::TOOLCHAIN_MISMATCH
        } else if !saw_triple {
            fallback_reasons::TRIPLE_MISMATCH
        } else if !saw_cargo_profile {
            fallback_reasons::CARGO_PROFILE_MISMATCH
        } else {
            fallback_reasons::MANIFEST_INVALID
        });
    };
    let source = selected.target;
    before_clone(allocated_tree_bytes(&source, deadline, cancel));
    if deadline.expired() {
        return Outcome::fallback(fallback_reasons::SEED_BUDGET_EXHAUSTED);
    }
    if cancel.is_some_and(|token| token.is_cancelled()) {
        return Outcome::fallback(fallback_reasons::CANCELLED);
    }
    let Ok(random) = temporary_name() else {
        return Outcome::fallback(fallback_reasons::STAGING_UNAVAILABLE);
    };
    let mut stage = OsString::from(".target-seed-");
    stage.push(random);
    if cache_dir.mkdir(&stage, 0o700).is_err() {
        return Outcome::fallback(fallback_reasons::STAGING_UNAVAILABLE);
    }
    let result = (|| {
        let staging = cache_dir
            .open_verified_directory(&stage)
            .map_err(|_| fallback_reasons::STAGING_UNAVAILABLE)?;
        clone(&source, &staging, deadline)?;
        if deadline.expired() {
            return Err(fallback_reasons::SEED_BUDGET_EXHAUSTED);
        }
        // Publish only a complete private target.
        staging
            .sync()
            .map_err(|_| fallback_reasons::STAGING_SYNC_FAILED)?;
        cache_dir
            .rename_noreplace(&stage, &cache_dir, OsStr::new("target"))
            .map_err(|_| fallback_reasons::PUBLISH_FAILED)?;
        Ok(())
    })();
    match result {
        Ok(()) => Outcome {
            status: "seeded".into(),
            reason: if cache_dir.sync().is_ok() {
                "cloned".into()
            } else {
                "durability_unconfirmed".into()
            },
            detail: None,
            seed_source: Some(SeedSource {
                generation_id: selected.generation_id,
                project_match: selected.project_match,
            }),
            extra: Default::default(),
        },
        Err(reason)
            if reason == fallback_reasons::CANCELLED
                || reason == fallback_reasons::SEED_BUDGET_EXHAUSTED =>
        {
            Outcome::fallback(reason)
        }
        Err(reason) => {
            match remove_seed_tree(&cache_dir, &stage, cancel, deadline) {
                Ok(true) => {}
                Ok(false) => {
                    return Outcome::fallback(
                        if cancel.is_some_and(|token| token.is_cancelled()) {
                            fallback_reasons::CANCELLED
                        } else {
                            fallback_reasons::SEED_BUDGET_EXHAUSTED
                        },
                    );
                }
                Err(_) => return Outcome::fallback(fallback_reasons::STAGING_CLEANUP_FAILED),
            }
            Outcome::fallback(reason)
        }
    }
}

#[cfg(unix)]
fn allocated_tree_bytes(
    path: &Path,
    deadline: &SeedBudget,
    cancel: Option<&provenant_core::CancelToken>,
) -> Option<u64> {
    use std::{fs, os::unix::fs::MetadataExt};
    fn walk(
        path: &Path,
        depth: usize,
        deadline: &SeedBudget,
        cancel: Option<&provenant_core::CancelToken>,
    ) -> Option<u64> {
        if depth > 64 || deadline.expired() || cancel.is_some_and(|token| token.is_cancelled()) {
            return None;
        }
        let metadata = fs::symlink_metadata(path).ok()?;
        let mut bytes = metadata.blocks().saturating_mul(512);
        if metadata.file_type().is_dir() {
            for entry in fs::read_dir(path).ok()? {
                if deadline.expired() || cancel.is_some_and(|token| token.is_cancelled()) {
                    return None;
                }
                bytes = bytes.saturating_add(walk(
                    &entry.ok()?.path(),
                    depth + 1,
                    deadline,
                    cancel,
                )?);
            }
        }
        Some(bytes)
    }
    walk(path, 0, deadline, cancel)
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
            unavailable: fallback_reasons::TARGET_UNAVAILABLE,
            read_failed: fallback_reasons::TARGET_READ_FAILED,
            too_deep: fallback_reasons::TARGET_TOO_DEEP,
            unsupported: fallback_reasons::TARGET_CONTAINS_UNSUPPORTED_ENTRY,
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
        return Err(fallback_reasons::SEED_BUDGET_EXHAUSTED);
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
        return Err(fallback_reasons::SEED_BUDGET_EXHAUSTED);
    }
    if depth > 128 {
        return Err(store.too_deep);
    }
    for entry in source.entries().map_err(|_| store.read_failed)? {
        if store.deadline.expired() {
            return Err(fallback_reasons::SEED_BUDGET_EXHAUSTED);
        }
        if cancel.is_cancelled() {
            return Err(fallback_reasons::CANCELLED);
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
                    .map_err(|_| fallback_reasons::CLONE_FAILED)?;
                let target = destination
                    .open_verified_directory(&name)
                    .map_err(|_| fallback_reasons::CLONE_FAILED)?;
                clone_entries(&child, &target, store, cancel, depth + 1)?;
            }
            FileKind::Regular => {
                let file = source.open_read(&name).map_err(|_| store.read_failed)?;
                destination
                    .clone_file_from(&file, &name)
                    .map_err(|_| fallback_reasons::CLONE_FAILED)?;
                if store.writable_files {
                    destination
                        .open_read(&name)
                        .and_then(|file| file.make_owner_writable())
                        .map_err(|_| fallback_reasons::CLONE_FAILED)?;
                }
            }
            _ => return Err(store.unsupported),
        }
    }
    if cancel.is_cancelled() {
        return Err(fallback_reasons::CANCELLED);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generation(state: &Path, name: &str, version: &str, triple: &str) {
        generation_for_project(state, name, version, triple, "");
    }

    fn generation_for_project(
        state: &Path,
        name: &str,
        version: &str,
        triple: &str,
        project_key: &str,
    ) {
        generation_for_profile(
            state,
            name,
            version,
            triple,
            project_key,
            provenant_config::LaneBuildSettings::default()
                .cargo_build_profile(&[])
                .expect("default Cargo profile is UTF-8"),
        );
    }

    fn generation_for_profile(
        state: &Path,
        name: &str,
        version: &str,
        triple: &str,
        project_key: &str,
        cargo_profile: provenant_config::CargoBuildProfile,
    ) {
        let dir = crate::target_seed::store_path(state)
            .join("generations")
            .join(name);
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("complete"), b"complete\n").unwrap();
        let manifest = crate::target_seed::Manifest {
            project_key: project_key.into(),
            rustc_vv: version.into(),
            host_triple: triple.into(),
            commit: "abc".into(),
            profile: "dev".into(),
            cargo_profile,
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
    }

    #[test]
    fn fallback_reasons_are_snake_case_and_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for reason in fallback_reasons::ALL {
            assert!(
                !reason.is_empty()
                    && !reason.starts_with('_')
                    && !reason.ends_with('_')
                    && !reason.contains("__")
                    && reason.bytes().all(|byte| byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'_'),
                "{reason} is not snake_case"
            );
            assert!(seen.insert(*reason), "{reason} is listed twice");
        }
    }

    #[test]
    fn download_stores_are_shared_without_reading_host_registry() {
        let temp = tempfile::tempdir().unwrap();
        let host_home = temp.path().join("host");
        let cargo_home = host_home.join(".cargo");
        let lane_home = temp.path().join("lane-home");
        let lane_cache = temp.path().join("lane-cache");
        std::fs::create_dir_all(cargo_home.join("registry/src")).unwrap();
        std::fs::write(cargo_home.join("registry/src/package.crate"), b"fixture").unwrap();
        std::fs::create_dir_all(&lane_home).unwrap();
        std::fs::create_dir_all(&lane_cache).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                cargo_home.join("registry"),
                std::fs::Permissions::from_mode(0o0),
            )
            .unwrap();
        }
        let environment = provenant_confinement::cargo::CargoHome::resolve(&host_home, |_| None)
            .map(|binding| {
                let mut environment = Vec::new();
                binding.lane_cargo_environment(&lane_home, &lane_cache, &mut environment);
                environment
            });
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                cargo_home.join("registry"),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        let environment = environment.unwrap();

        let cargo_home_values: Vec<_> = environment
            .iter()
            .filter(|(key, _)| key == "CARGO_HOME")
            .map(|(_, value)| Path::new(value))
            .collect();
        assert_eq!(cargo_home_values, [cargo_home.as_path()]);
        assert!(!lane_home.join(".cargo/registry").exists());
        assert!(std::fs::read_dir(&lane_home).unwrap().next().is_none());
        assert!(std::fs::read_dir(&lane_cache).unwrap().next().is_none());
    }

    #[test]
    fn setup_outcome_preserves_unknown_fields_through_bounded_decode() {
        let mut payload =
            serde_json::to_value(Outcome::fallback(fallback_reasons::NO_GENERATIONS)).unwrap();
        payload.as_object_mut().unwrap().insert(
            "future_setup_fact".into(),
            serde_json::json!({"kept": true}),
        );
        let bytes = serde_json::to_vec(&payload).unwrap();
        let decoded: Outcome =
            provenant_records::decode(bytes.as_slice(), provenant_records::Limits::default())
                .unwrap();
        let encoded = provenant_records::encode(&decoded).unwrap();
        let round_trip: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(
            round_trip.get("future_setup_fact"),
            Some(&serde_json::json!({"kept": true}))
        );
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
        assert_eq!(
            run("v1", "host"),
            Outcome::fallback(fallback_reasons::NO_GENERATIONS)
        );
        generation(&state, "001", "v1", "host");
        generation(&state, "002", "v1", "host");
        generation(&state, "003", "v2", "other");
        for (version, triple, reason) in [
            ("unknown", "host", fallback_reasons::TOOLCHAIN_MISMATCH),
            ("v1", "other", fallback_reasons::TRIPLE_MISMATCH),
        ] {
            assert_eq!(run(version, triple), Outcome::fallback(reason));
        }
        assert_eq!(run("v1", "host").status, "seeded");
        assert_eq!(run("v1", "host"), Outcome::skipped("existing_target"));
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn project_generation_beats_newer_foreign_generation_and_receipt_identifies_it() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let cache = tmp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        generation_for_project(&state, "001", "v1", "host", "project-a");
        generation_for_project(&state, "002", "v1", "host", "project-b");
        let profile = provenant_config::LaneBuildSettings::default()
            .cargo_build_profile(&[])
            .expect("default Cargo profile is UTF-8");
        let outcome = seed_target_cancellable(
            &state,
            &cache,
            SeedCompatibility {
                version: "v1",
                triple: "host",
                cargo_profile: &profile,
                project_key: "project-a",
            },
            None,
            |_| {},
            |source, _, _| {
                assert!(source.ends_with("001/target"));
                Ok(())
            },
        );
        assert_eq!(
            (outcome.status.as_str(), outcome.reason.as_str()),
            ("seeded", "cloned")
        );
        assert_eq!(
            outcome.seed_source,
            Some(SeedSource {
                generation_id: Some("001".into()),
                project_match: true,
            })
        );
        let record = serde_json::to_value(&outcome).unwrap();
        assert_eq!(
            record["seed_source"],
            serde_json::json!({"generation_id":"001", "project_match":true})
        );
        assert!(!record.to_string().contains("/target"));
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn seed_with_a_different_cargo_profile_is_not_selected() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let cache = tmp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        let profile = provenant_config::LaneBuildSettings::default()
            .cargo_build_profile(&[])
            .expect("default Cargo profile is UTF-8");
        let mut different = profile.clone();
        different.debug = "full".into();
        generation_for_profile(&state, "001", "v1", "host", "project-a", different);

        let outcome = seed_target_cancellable(
            &state,
            &cache,
            SeedCompatibility {
                version: "v1",
                triple: "host",
                cargo_profile: &profile,
                project_key: "project-a",
            },
            None,
            |_| {},
            |_, _, _| panic!("profile-incompatible artifacts are never cloned"),
        );
        assert_eq!(
            outcome,
            Outcome::fallback(fallback_reasons::CARGO_PROFILE_MISMATCH)
        );
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn compatible_foreign_generation_is_used_only_when_project_seed_is_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let cache = tmp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        generation_for_project(&state, "001", "v1", "host", "project-b");
        let profile = provenant_config::LaneBuildSettings::default()
            .cargo_build_profile(&[])
            .expect("default Cargo profile is UTF-8");
        let outcome = seed_target_cancellable(
            &state,
            &cache,
            SeedCompatibility {
                version: "v1",
                triple: "host",
                cargo_profile: &profile,
                project_key: "project-a",
            },
            None,
            |_| {},
            |source, _, _| {
                assert!(source.ends_with("001/target"));
                Ok(())
            },
        );
        assert_eq!(outcome.status, "seeded");
        assert_eq!(
            outcome.seed_source,
            Some(SeedSource {
                generation_id: Some("001".into()),
                project_match: false,
            })
        );
    }

    #[test]
    fn target_failed_clone_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        generation(tmp.path(), "001", "v1", "host");
        let outcome = seed_target_with(tmp.path(), &cache, "v1", "host", |_, stage| {
            stage.mkdir(std::ffi::OsStr::new("partial"), 0o700).unwrap();
            Err(fallback_reasons::CLONE_FAILED)
        });
        assert_eq!(outcome, Outcome::fallback(fallback_reasons::CLONE_FAILED));
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
        assert_eq!(run(), Outcome::fallback(fallback_reasons::MANIFEST_INVALID));
        let _lock = crate::target_seed::lock_store(tmp.path()).unwrap();
        assert_eq!(
            run(),
            Outcome::fallback(fallback_reasons::SEED_LOCK_UNAVAILABLE)
        );
    }

    #[test]
    fn target_generation_without_complete_marker_is_not_selected() {
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
        assert_eq!(run(), Outcome::fallback(fallback_reasons::MANIFEST_INVALID));
        std::fs::remove_file(dir.join("complete")).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(run(), Outcome::fallback(fallback_reasons::MANIFEST_INVALID));
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
        let build = worktree.join("arbitrary/nested/cargo/workspace");
        std::fs::create_dir_all(&build).unwrap();
        std::fs::create_dir_all(worktree.join(".provenant")).unwrap();
        let mut ports = provenant_config::PortTable::builtin();
        ports.verify.workspace = "arbitrary/nested/cargo/workspace".into();
        std::fs::write(
            worktree.join(provenant_config::PORTS_CONFIG),
            serde_json::to_vec(&ports).unwrap(),
        )
        .unwrap();
        std::fs::write(build.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(home.join(".rustup/settings.toml"), "[overrides]\n").unwrap();
        std::fs::write(
            build.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = '1.99.1'\n",
        )
        .unwrap();
        let report = "rustc 1.99.1\nhost: test-host\nrelease: 1.99.1\n";
        std::fs::create_dir(worktree.join(".git")).unwrap();
        let project_key = crate::target_seed::project_key(&worktree.join(".git"));
        let rustc = bin.join("rustc");
        std::fs::write(
            &rustc,
            format!("#!/bin/sh\nprintf '{}'\n", report.replace('\n', "\\n")),
        )
        .unwrap();
        std::fs::set_permissions(&rustc, std::fs::Permissions::from_mode(0o755)).unwrap();
        generation_for_project(&state, "001", report, "test-host", &project_key);
        // No ambient CARGO_HOME: the same finalisation used by handoff must
        // make a fresh, arbitrarily nested Cargo project seedable.
        let mut env = vec![];
        provenant_confinement::cargo::CargoHome::resolve(&home, |_| None)
            .unwrap()
            .lane_cargo_environment(&home, &cache, &mut env);
        let outcome = seed_target(
            &state,
            &cache,
            &worktree,
            &project_key,
            &home,
            &env,
            &provenant_core::CancelToken::new(),
        );
        assert_eq!(
            (outcome.status.as_str(), outcome.reason.as_str()),
            ("seeded", "cloned")
        );
        assert!(cache.join("target").is_dir());
        // A refusal records its code and nothing from the environment.
        let refused = [("RUSTC".into(), "/secret/rustc".into())];
        let outcome = seed_target(
            &state,
            &cache,
            &worktree,
            &project_key,
            &home,
            &refused,
            &provenant_core::CancelToken::new(),
        );
        assert_eq!(
            outcome,
            Outcome::fallback_detail(
                fallback_reasons::TOOLCHAIN_UNVERIFIED,
                codes::TOOLCHAIN_ENV_OVERRIDE,
            )
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn target_clones_are_private_and_writable_for_read_only_tree_shapes() {
        use std::os::unix::fs::PermissionsExt;
        for nested_tree in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let cache = tmp.path().join("cache");
            std::fs::create_dir(&cache).unwrap();
            generation(tmp.path(), "001", "v1", "host");
            let target = crate::target_seed::store_path(tmp.path()).join("generations/001/target");
            let seed_directory = if nested_tree {
                target.join("debug/deps")
            } else {
                target.clone()
            };
            std::fs::create_dir_all(&seed_directory).unwrap();
            let seed = seed_directory.join("artifact");
            std::fs::write(&seed, b"seed").unwrap();
            std::fs::set_permissions(&seed, std::fs::Permissions::from_mode(0o444)).unwrap();
            if nested_tree {
                std::fs::set_permissions(
                    target.join("debug"),
                    std::fs::Permissions::from_mode(0o555),
                )
                .unwrap();
                std::fs::set_permissions(&seed_directory, std::fs::Permissions::from_mode(0o555))
                    .unwrap();
            }

            let outcome = seed_target_with(tmp.path(), &cache, "v1", "host", |src, dst| {
                clone_target(src, dst, &provenant_core::CancelToken::new())
            });
            assert_eq!(outcome.status, "seeded");
            let lane_directory = if nested_tree {
                cache.join("target/debug/deps")
            } else {
                cache.join("target")
            };
            std::fs::write(lane_directory.join("artifact"), b"lane").unwrap();
            std::fs::write(lane_directory.join("new"), b"lane").unwrap();
            assert_eq!(std::fs::read(&seed).unwrap(), b"seed");
            assert!(!seed_directory.join("new").exists());

            if nested_tree {
                std::fs::set_permissions(
                    target.join("debug"),
                    std::fs::Permissions::from_mode(0o755),
                )
                .unwrap();
                std::fs::set_permissions(&seed_directory, std::fs::Permissions::from_mode(0o755))
                    .unwrap();
            }
        }
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
            Err(fallback_reasons::SEED_BUDGET_EXHAUSTED)
        );
        assert!(std::fs::read_dir(tmp.path()).unwrap().next().is_none());
    }
}
