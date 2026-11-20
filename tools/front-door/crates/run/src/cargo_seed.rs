//! Best-effort private target seeds from host-built generations.
use provenant_core::{Clock, Deadline, DurationMicros, WaitBudget};
use provenant_os::clock::SystemClock;
use std::path::Path;

/// Which bounded step of a seed a `SeedBudget` belongs to. Each step gets its
/// own deadline from the same configured budget, so a slow clone cannot eat the
/// time allowed for selection or dep-info aging, and a fallback can say which
/// step ran out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BudgetStep {
    Selection,
    Clone,
    Aging,
    Cleanup,
}
impl BudgetStep {
    const fn detail(self) -> &'static str {
        match self {
            Self::Selection => "seed_budget_selection",
            Self::Clone => "seed_budget_clone",
            Self::Aging => "seed_budget_dep_info",
            Self::Cleanup => "seed_budget_cleanup",
        }
    }
    const fn label(self) -> &'static str {
        match self {
            Self::Selection => "generation selection",
            Self::Clone => "clone",
            Self::Aging => "dep-info aging",
            Self::Cleanup => "stage cleanup",
        }
    }
}
const BUDGET_STEPS: [&str; 4] = [
    BudgetStep::Selection.detail(),
    BudgetStep::Clone.detail(),
    BudgetStep::Aging.detail(),
    BudgetStep::Cleanup.detail(),
];
fn is_budget_detail(code: &str) -> bool {
    BUDGET_STEPS.contains(&code)
}
fn step_from_detail(detail: &str) -> BudgetStep {
    match detail {
        code if code == BudgetStep::Selection.detail() => BudgetStep::Selection,
        code if code == BudgetStep::Aging.detail() => BudgetStep::Aging,
        code if code == BudgetStep::Cleanup.detail() => BudgetStep::Cleanup,
        _ => BudgetStep::Clone,
    }
}

/// The configured seed budget and the layer that supplied it, as reported in
/// `cargo-target-setup.json` and the fallback warning.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SeedBudgetInfo {
    pub setting: String,
    pub source: String,
    /// The step whose own deadline was spent: selection, clone, dep-info aging
    /// or stage cleanup.
    pub step: String,
}

fn display_source(source: provenant_records::LimitSource) -> &'static str {
    match source {
        provenant_records::LimitSource::Default => "compiled default",
        provenant_records::LimitSource::Host => "host",
        provenant_records::LimitSource::Project => "project",
        provenant_records::LimitSource::Tier => "tier",
        provenant_records::LimitSource::Template => "template",
        provenant_records::LimitSource::Launch => "launch",
    }
}

/// The filesystem preparation edge samples time through the shared clock. One
/// deadline is carried through one step; a fresh one starts at the next step.
struct SeedBudget {
    clock: Box<dyn Clock>,
    deadline: Deadline,
    step: BudgetStep,
}
impl SeedBudget {
    fn for_step(setting: &provenant_config::SeedBudgetSetting, step: BudgetStep) -> Self {
        Self::for_step_clock(Box::new(SystemClock::new()), setting, step)
    }
    /// Build a step against an injected clock. Tests advance the clock during
    /// a slow clone to prove the next step starts with a fresh deadline.
    fn for_step_clock(
        clock: Box<dyn Clock>,
        setting: &provenant_config::SeedBudgetSetting,
        step: BudgetStep,
    ) -> Self {
        let deadline = WaitBudget::up_to(
            clock.mono_now(),
            DurationMicros::from_micros(setting.micros),
        )
        .deadline();
        Self {
            clock,
            deadline,
            step,
        }
    }
    /// The compiled default, for unit tests that exercise one step in isolation.
    #[cfg(test)]
    fn new() -> Self {
        Self::for_step(
            &provenant_config::SeedBudgetSetting::compiled_default(),
            BudgetStep::Clone,
        )
    }
    fn expired(&self) -> bool {
        self.deadline.is_expired(self.clock.mono_now())
    }
    /// The detail code a step returns when its own deadline is spent.
    fn exhausted_code(&self) -> &'static str {
        self.step.detail()
    }
    #[cfg(test)]
    fn exhausted(step: BudgetStep) -> Self {
        Self {
            clock: Box::new(SystemClock::new()),
            deadline: WaitBudget::nonblocking(provenant_core::MonoTime::from_micros(0)).deadline(),
            step,
        }
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
    CARGO_HOME_UNRESOLVABLE = "cargo_home_unresolvable",
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
    FULL_GENERATION = "full_generation",
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
    FRESHNESS_RESET_FAILED = "freshness_reset_failed",
    SEED_LAYOUT_UNRECOGNISED = "seed_layout_unrecognised",
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Outcome {
    status: String,
    reason: String,
    /// Stable snake_case code for the specific refusal behind `reason`. Never
    /// an environment value or path.
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    /// What differed when the refusal is a mismatch: the expected fact and the
    /// one found. Toolchain, triple, profile and variable-class names only, never
    /// a path or an environment value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    differs: Option<Difference>,
    /// Path-free receipt proving which complete generation supplied a seed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seed_source: Option<SeedSource>,
    /// The effective per-step seed budget when a step ran out of time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    budget: Option<SeedBudgetInfo>,
    /// `Some(false)` when the seed generation predates recorded sizes and so
    /// reserves no cache budget; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    size_recorded: Option<bool>,
    #[serde(flatten)]
    extra: provenant_records::ExtraFields,
}

/// Expected against found, in words a person can act on. Both sides are bounded
/// plain text: control characters are dropped and the length is capped, so a
/// value read from a checkout or a store cannot reshape a terminal line.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Difference {
    expected: String,
    found: String,
}

impl Difference {
    pub(crate) fn new(expected: impl AsRef<str>, found: impl AsRef<str>) -> Self {
        let plain = |text: &str| -> String {
            text.chars()
                .filter(|character| !character.is_control())
                .take(120)
                .collect()
        };
        Self {
            expected: plain(expected.as_ref()),
            found: plain(found.as_ref()),
        }
    }

    /// Distinct values joined for display, at most four named.
    #[cfg_attr(not(unix), allow(dead_code))]
    fn found_among(found: &std::collections::BTreeSet<String>) -> String {
        const NAMED: usize = 4;
        let mut text = found
            .iter()
            .take(NAMED)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if found.len() > NAMED {
            text.push_str(&format!(" and {} more", found.len() - NAMED));
        }
        text
    }
}

/// A toolchain-verification refusal: a stable snake_case code and, where the
/// refusal is a mismatch, what was expected and what was found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) code: &'static str,
    pub(crate) differs: Option<Difference>,
}

impl Refusal {
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn differs(
        code: &'static str,
        expected: impl AsRef<str>,
        found: impl AsRef<str>,
    ) -> Self {
        Self {
            code,
            differs: Some(Difference::new(expected, found)),
        }
    }
}

impl From<&'static str> for Refusal {
    fn from(code: &'static str) -> Self {
        Self {
            code,
            differs: None,
        }
    }
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
    /// Size recorded when the generation was built, for cache reservation.
    allocated_bytes: Option<u64>,
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
            differs: None,
            seed_source: None,
            budget: None,
            size_recorded: None,
            extra: Default::default(),
        }
    }
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn fallback(reason: &'static str) -> Self {
        Self {
            status: "fallback".into(),
            reason: reason.into(),
            detail: None,
            differs: None,
            seed_source: None,
            budget: None,
            size_recorded: None,
            extra: Default::default(),
        }
    }
    /// A step ran out of its own budget. `detail` names the step and `budget`
    /// records the effective setting and its source.
    fn budget_fallback(
        detail: &'static str,
        setting: &provenant_config::SeedBudgetSetting,
    ) -> Self {
        Self {
            status: "fallback".into(),
            reason: fallback_reasons::SEED_BUDGET_EXHAUSTED.into(),
            detail: Some(detail.into()),
            differs: None,
            seed_source: None,
            budget: Some(SeedBudgetInfo {
                setting: setting.setting.clone(),
                source: display_source(setting.source).into(),
                step: step_from_detail(detail).label().into(),
            }),
            size_recorded: None,
            extra: Default::default(),
        }
    }
    /// One line for the host's stderr when a seed was wanted and not used, so
    /// a target that builds from scratch is not a surprise. A mismatch names
    /// what was expected and what was found. Where the filesystem cannot clone,
    /// the reason is `clone_failed`: the target stays unseeded and its first
    /// build is cold. A budget fallback names the step that ran out and the
    /// setting with its source. `consequence` says what the caller gets instead.
    pub(crate) fn fallback_warning(&self, what: &str, consequence: &str) -> Option<String> {
        (self.status == "fallback").then(|| {
            format!(
                "warning: {what} not seeded ({}{}{}{}); {consequence}",
                self.reason,
                self.detail
                    .as_deref()
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default(),
                self.differs
                    .as_ref()
                    .map(|d| format!("; expected {}, found {}", d.expected, d.found))
                    .unwrap_or_default(),
                self.budget
                    .as_ref()
                    .map(|budget| {
                        format!(
                            "; budget {} (source {}) ran out in {}",
                            budget.setting, budget.source, budget.step
                        )
                    })
                    .unwrap_or_default()
            )
        })
    }
    pub(crate) fn fallback_detail(reason: &'static str, detail: &'static str) -> Self {
        Self::fallback_refusal(reason, detail.into())
    }
    pub(crate) fn fallback_refusal(reason: &'static str, refusal: Refusal) -> Self {
        Self {
            status: "fallback".into(),
            reason: reason.into(),
            detail: Some(refusal.code.into()),
            differs: refusal.differs,
            seed_source: None,
            budget: None,
            size_recorded: None,
            extra: Default::default(),
        }
    }
    /// A generation mismatch: no complete generation matched what was wanted.
    #[cfg_attr(not(unix), allow(dead_code))]
    fn fallback_differs(reason: &'static str, differs: Difference) -> Self {
        Self {
            differs: Some(differs),
            ..Self::fallback(reason)
        }
    }
    pub(crate) fn is_skipped(&self) -> bool {
        self.status == "skipped"
    }

    pub(crate) fn is_seeded(&self) -> bool {
        self.status == "seeded"
    }
}

/// What `provenant worktree seed-target` did for one checkout.
#[derive(Debug, serde::Serialize)]
pub struct WorktreeSeed {
    /// The directory to use as `CARGO_TARGET_DIR`.
    pub target: std::path::PathBuf,
    pub status: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// What differed, expected against found, when a mismatch kept the seed out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub differs: Option<Difference>,
    /// The host seed generation cloned, when one was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    /// Where the Cargo home that was verified came from: the caller's shell can
    /// differ from the host a lane runs under.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cargo_home: Option<&'static str>,
    /// How a person gets a seed this checkout can use, when none was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair: Option<&'static str>,
    /// The effective per-step seed budget and its source when a step ran out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<SeedBudgetInfo>,
}

/// Where host seeds come from, for a reason that only a new generation fixes.
fn seed_repair(reason: &str) -> Option<&'static str> {
    match reason {
        fallback_reasons::NO_GENERATIONS
        | fallback_reasons::STORE_UNAVAILABLE
        | fallback_reasons::MANIFEST_INVALID
        | fallback_reasons::FULL_GENERATION
        | fallback_reasons::TOOLCHAIN_MISMATCH
        | fallback_reasons::TRIPLE_MISMATCH
        | fallback_reasons::CARGO_PROFILE_MISMATCH => Some(
            "host seeds are built from the verify base by `provenant worktree seed-build` and refreshed by tick when it moves; build one with the toolchain and profile above, then run seed-target again",
        ),
        fallback_reasons::SEED_LAYOUT_UNRECOGNISED => Some(
            "this release cannot age the seed's Cargo build layout; upgrade provenant, or rebuild the seed with `provenant worktree seed-build` after upgrading",
        ),
        _ => None,
    }
}

/// The caller's environment as a lane would see it for the toolchain homes:
/// rustup keeps its toolchains in `RUSTUP_HOME`, which defaults to
/// `$HOME/.rustup`, where lanes always set it. Outside a lane nothing sets it,
/// so an unset or empty value takes the default; any other value is kept, so a
/// toolchain home that differs from the lane's is refused and named.
fn outside_lane_environment(
    home: &Path,
    environment: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let mut resolved = environment.to_vec();
    if !resolved
        .iter()
        .any(|(key, value)| key == "RUSTUP_HOME" && !value.is_empty())
    {
        resolved.retain(|(key, _)| key != "RUSTUP_HOME");
        resolved.push(("RUSTUP_HOME".into(), home.join(".rustup").into_os_string()));
    }
    resolved
}

/// Seed a checkout's private target, the one a writer lane in that checkout
/// uses, from the newest compatible host seed generation, with dep-info aged.
/// An existing target is kept. Runs on the host with the caller's environment,
/// so the caller's Cargo profile overrides choose the generation. The
/// toolchain is resolved as cargo and rustup would for the checkout (its
/// `rust-toolchain.toml`, the default `$HOME/.rustup` and `$HOME/.cargo`) and
/// verified by the rules lanes use; a refusal names what differed.
pub fn seed_worktree_target(
    state: &Path,
    home: &Path,
    worktree: &Path,
    common_git_dir: &Path,
    cache: &Path,
    environment: &[(std::ffi::OsString, std::ffi::OsString)],
) -> WorktreeSeed {
    let environment = outside_lane_environment(home, environment);
    let mut cargo_home_source = None;
    let outcome = match crate::target_seed::cargo_build_directory(worktree, state) {
        Ok(Some(build)) => match provenant_confinement::cargo::CargoHome::resolve(home, |name| {
            environment
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        })
        .map_err(|_| codes::CARGO_HOME_UNRESOLVABLE)
        .and_then(|cargo_home| {
            cargo_home_source = Some(cargo_home.source);
            crate::writer::verified_cargo_config(&build, &cargo_home.path)
        }) {
            Err(detail) => Outcome::fallback_detail(fallback_reasons::TOOLCHAIN_UNVERIFIED, detail),
            Ok(()) => seed_target_with_reservation(
                state,
                cache,
                worktree,
                &crate::target_seed::project_key(common_git_dir),
                home,
                &environment,
                &provenant_core::CancelToken::new(),
                "target",
                false,
                |_| {},
            ),
        },
        Ok(None) => Outcome::skipped("non_cargo_project"),
        Err(_) => Outcome::fallback_detail(
            fallback_reasons::TOOLCHAIN_UNVERIFIED,
            codes::CARGO_WORKSPACE_UNREADABLE,
        ),
    };
    WorktreeSeed {
        target: cache.join("target"),
        warning: outcome.fallback_warning("the Cargo target", "builds in it start cold"),
        cargo_home: cargo_home_source,
        repair: seed_repair(&outcome.reason).filter(|_| outcome.status == "fallback"),
        differs: outcome.differs.clone(),
        generation: outcome
            .seed_source
            .as_ref()
            .and_then(|source| source.generation_id.clone()),
        status: outcome.status,
        reason: outcome.reason,
        detail: outcome.detail,
        budget: outcome.budget,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn seed_target_with_reservation(
    state: &Path,
    cache: &Path,
    worktree: &Path,
    project_key: &str,
    home: &Path,
    environment: &[(std::ffi::OsString, std::ffi::OsString)],
    cancel: &provenant_core::CancelToken,
    destination: &str,
    split_clone_budget: bool,
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
            Err(refusal) => {
                return Outcome::fallback_refusal(fallback_reasons::TOOLCHAIN_UNVERIFIED, refusal);
            }
        };
        seed_target_cancellable_inner_for_destination(
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
            SeedBudget::for_step,
            destination,
            split_clone_budget,
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
            destination,
            split_clone_budget,
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

/// The first line of `rustc -vV`, which names the release, commit and date.
#[cfg(any(unix, test))]
fn compiler_line(version: &str) -> &str {
    version.lines().next().unwrap_or(version)
}

#[cfg(any(unix, test))]
fn describe_profile(profile: &provenant_config::CargoBuildProfile) -> String {
    format!(
        "CARGO_INCREMENTAL={} CARGO_PROFILE_DEV_DEBUG={}",
        profile.incremental, profile.debug
    )
}

#[cfg(test)]
fn seed_target_cancellable(
    state: &Path,
    cache: &Path,
    compatibility: SeedCompatibility<'_>,
    cancel: Option<&provenant_core::CancelToken>,
    before_clone: impl FnOnce(Option<u64>),
    clone: impl FnOnce(&Path, &provenant_os::fs::Directory, &SeedBudget) -> Result<(), &'static str>,
) -> Outcome {
    seed_target_cancellable_inner(
        state,
        cache,
        compatibility,
        cancel,
        before_clone,
        clone,
        SeedBudget::for_step,
    )
}

/// Same seeding flow, with the per-step budget construction injected. Production
/// passes the real per-step builder; tests inject a clock-backed one so a slow
/// clone can be simulated without waiting on the real clock.
#[cfg(test)]
fn seed_target_cancellable_inner(
    state: &Path,
    cache: &Path,
    compatibility: SeedCompatibility<'_>,
    cancel: Option<&provenant_core::CancelToken>,
    before_clone: impl FnOnce(Option<u64>),
    clone: impl FnOnce(&Path, &provenant_os::fs::Directory, &SeedBudget) -> Result<(), &'static str>,
    budget: impl Fn(&provenant_config::SeedBudgetSetting, BudgetStep) -> SeedBudget,
) -> Outcome {
    seed_target_cancellable_inner_for_destination(
        state,
        cache,
        compatibility,
        cancel,
        before_clone,
        clone,
        budget,
        "target",
        false,
    )
}

#[cfg(any(unix, test))]
#[allow(clippy::too_many_arguments)]
fn seed_target_cancellable_inner_for_destination(
    state: &Path,
    cache: &Path,
    compatibility: SeedCompatibility<'_>,
    cancel: Option<&provenant_core::CancelToken>,
    before_clone: impl FnOnce(Option<u64>),
    clone: impl FnOnce(&Path, &provenant_os::fs::Directory, &SeedBudget) -> Result<(), &'static str>,
    budget: impl Fn(&provenant_config::SeedBudgetSetting, BudgetStep) -> SeedBudget,
    destination: &str,
    split_clone_budget: bool,
) -> Outcome {
    // One configured budget, one deadline per step. Cleanup, selection, clone
    // and aging each start their own deadline when the step begins, so a slow
    // step cannot spend another's time.
    let setting = match provenant_config::cargo_seed_budget(state) {
        Ok(setting) => setting,
        Err(error) => {
            use std::io::Write;
            let path = state.join("config.toml");
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: could not read the Cargo seed budget from {}: {error}; using the compiled default {}",
                path.display(),
                provenant_config::CARGO_SEED_BUDGET_DEFAULT
            );
            provenant_config::SeedBudgetSetting::compiled_default()
        }
    };
    // Cleanup runs before selection: give it its own deadline and create the
    // selection deadline only afterwards, so time spent removing an interrupted
    // stage is not charged to selection.
    let cleanup = budget(&setting, BudgetStep::Cleanup);
    // Cleanup after a clone or publish has spent its own budget gets a fresh one.
    let fresh_cleanup = || budget(&setting, BudgetStep::Cleanup);
    use provenant_os::fs::{Directory, temporary_name};
    use std::ffi::{OsStr, OsString};
    let Ok(cache_dir) = Directory::open_anchor(cache) else {
        return Outcome::fallback(fallback_reasons::CACHE_UNAVAILABLE);
    };
    match cache_dir.metadata_at(OsStr::new(destination), false) {
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
        if cleanup.expired() {
            return Outcome::budget_fallback(BudgetStep::Cleanup.detail(), &setting);
        }
        if cancel.is_some_and(|token| token.is_cancelled()) {
            return Outcome::fallback(fallback_reasons::CANCELLED);
        }
        let Ok(stage) = stage else {
            return Outcome::fallback(fallback_reasons::STAGING_CLEANUP_FAILED);
        };
        if stage.to_string_lossy().starts_with(".target-seed-") {
            match remove_seed_tree(&cache_dir, &stage, cancel, &cleanup) {
                Ok(true) => {}
                Ok(false) => {
                    return if cancel.is_some_and(|token| token.is_cancelled()) {
                        Outcome::fallback(fallback_reasons::CANCELLED)
                    } else {
                        Outcome::budget_fallback(BudgetStep::Cleanup.detail(), &setting)
                    };
                }
                Err(_) => return Outcome::fallback(fallback_reasons::STAGING_CLEANUP_FAILED),
            }
        }
    }
    // Selection starts only now: stage cleanup above has its own deadline.
    let selection = budget(&setting, BudgetStep::Selection);
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
    let mut saw_full_generation = false;
    let mut saw_entry = false;
    // What the complete generations held, for a refusal that names it.
    let mut found_versions = std::collections::BTreeSet::new();
    let mut found_triples = std::collections::BTreeSet::new();
    let mut found_profiles = std::collections::BTreeSet::new();
    let mut selected_project = None;
    let mut selected_shared = None;
    for entry in entries {
        if selection.expired() {
            return Outcome::budget_fallback(BudgetStep::Selection.detail(), &setting);
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
        // A generation that still holds the checkout's own units can go stale
        // against this worktree. It is never a candidate, whatever else matches.
        if manifest.contents != "deps" {
            saw_full_generation = true;
            continue;
        }
        if manifest.rustc_vv != compatibility.version {
            found_versions.insert(compiler_line(&manifest.rustc_vv).to_owned());
            continue;
        }
        saw_version = true;
        if manifest.host_triple != compatibility.triple {
            found_triples.insert(manifest.host_triple.clone());
            continue;
        }
        saw_triple = true;
        if &manifest.cargo_profile != compatibility.cargo_profile {
            found_profiles.insert(describe_profile(&manifest.cargo_profile));
            continue;
        }
        saw_cargo_profile = true;
        let project_match = !compatibility.project_key.is_empty()
            && manifest.project_key == compatibility.project_key;
        let candidate = SeedCandidate {
            target: entry.path().join("target"),
            generation_id: safe_generation_id(&entry.file_name()),
            project_match,
            allocated_bytes: manifest.allocated_bytes,
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
        return if !saw_entry {
            Outcome::fallback(fallback_reasons::NO_GENERATIONS)
        } else if !saw_manifest {
            Outcome::fallback(fallback_reasons::MANIFEST_INVALID)
        } else if saw_full_generation {
            // Only obsolete full generations were found; a deps generation is
            // needed, and refresh will build one.
            Outcome::fallback(fallback_reasons::FULL_GENERATION)
        } else if !saw_version {
            Outcome::fallback_differs(
                fallback_reasons::TOOLCHAIN_MISMATCH,
                Difference::new(
                    compiler_line(compatibility.version),
                    Difference::found_among(&found_versions),
                ),
            )
        } else if !saw_triple {
            Outcome::fallback_differs(
                fallback_reasons::TRIPLE_MISMATCH,
                Difference::new(
                    compatibility.triple,
                    Difference::found_among(&found_triples),
                ),
            )
        } else if !saw_cargo_profile {
            Outcome::fallback_differs(
                fallback_reasons::CARGO_PROFILE_MISMATCH,
                Difference::new(
                    describe_profile(compatibility.cargo_profile),
                    Difference::found_among(&found_profiles),
                ),
            )
        } else {
            Outcome::fallback(fallback_reasons::MANIFEST_INVALID)
        };
    };
    let source = selected.target;
    // The size was measured once when the generation was built. Walking a
    // multi-gigabyte seed here is what used to spend the clone's time before it
    // started. An older generation records none; reserve nothing for it.
    before_clone(selected.allocated_bytes);
    if selection.expired() {
        return Outcome::budget_fallback(BudgetStep::Selection.detail(), &setting);
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
    let mut clone_setting = setting.clone();
    if split_clone_budget {
        clone_setting.micros /= 2;
        clone_setting.setting = format!("{} (half share)", setting.setting);
    }
    let cloning = budget(&clone_setting, BudgetStep::Clone);
    // The size was measured once at publication; record whether this
    // generation has one so a missing size is visible without a per-launch
    // warning. Eviction accounting resumes when the seed is refreshed.
    let size_recorded = selected.allocated_bytes.is_some();
    let result = (|| {
        let staging = cache_dir
            .open_verified_directory(&stage)
            .map_err(|_| fallback_reasons::STAGING_UNAVAILABLE)?;
        clone(&source, &staging, &cloning)?;
        // Aging starts only after the clone returns: its deadline is a full
        // budget from this moment, never whatever the clone left behind.
        let aging = budget(&setting, BudgetStep::Aging);
        age_dep_info(&cache.join(&stage), &aging, cancel)?;
        // Publish only a complete private target.
        staging
            .sync()
            .map_err(|_| fallback_reasons::STAGING_SYNC_FAILED)?;
        cache_dir
            .rename_noreplace(&stage, &cache_dir, OsStr::new(destination))
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
            differs: None,
            seed_source: Some(SeedSource {
                generation_id: selected.generation_id,
                project_match: selected.project_match,
            }),
            budget: None,
            size_recorded: (!size_recorded).then_some(false),
            extra: Default::default(),
        },
        Err(reason) if is_budget_detail(reason) || reason == fallback_reasons::CANCELLED => {
            // The step's own budget or the token is spent; remove the partial
            // stage with a fresh bounded budget so it does not linger for the
            // next seed to spend its budget on. One that does not finish is
            // retried by the next seed under the store lock.
            let _ = remove_seed_tree(&cache_dir, &stage, None, &fresh_cleanup());
            if is_budget_detail(reason) {
                Outcome::budget_fallback(
                    reason,
                    if reason == BudgetStep::Clone.detail() {
                        &clone_setting
                    } else {
                        &setting
                    },
                )
            } else {
                Outcome::fallback(reason)
            }
        }
        Err(reason) => {
            match remove_seed_tree(&cache_dir, &stage, cancel, &fresh_cleanup()) {
                Ok(true) => {}
                Ok(false) => {
                    let _ = remove_seed_tree(&cache_dir, &stage, None, &fresh_cleanup());
                    return if cancel.is_some_and(|token| token.is_cancelled()) {
                        Outcome::fallback(fallback_reasons::CANCELLED)
                    } else {
                        Outcome::budget_fallback(BudgetStep::Cleanup.detail(), &setting)
                    };
                }
                Err(_) => return Outcome::fallback(fallback_reasons::STAGING_CLEANUP_FAILED),
            }
            Outcome::fallback(reason)
        }
    }
}

/// The deepest directory the seed clone and the aging walk follow, so a tree
/// one accepts is a tree the other can read.
#[cfg(any(unix, test))]
const MAX_TREE_DEPTH: usize = 128;

/// A seed was built from another checkout. Cargo hashes a workspace member's
/// units by its workspace-relative path and treats them as fresh while every
/// source listed in their dep-info is older than that dep-info, so a worktree
/// checked out before the seed was built would reuse the seed's member
/// artifacts for different sources (#3561). Ageing a unit's dep-info to the
/// epoch makes it rebuild once in its new worktree.
///
/// Cargo does not promise where fingerprints live (build-dir layout v2 puts
/// them in `<profile>/build/<package>/<hash>/fingerprint/`, the earlier layout
/// in `<profile>/.fingerprint/<unit>/`), and it does not promise which units
/// keep dep-info there. So the pass is driven by the fingerprints themselves:
/// it walks the seed within the depth, budget and cancellation bounds for the
/// `*.json` fingerprints in a `fingerprint` directory or a unit directory under
/// `.fingerprint`, reads each one's `local` entries and ages exactly the files
/// its `CheckDepInfo` entries name, relative to the target root. Unknown fields
/// are ignored. A fingerprint that cannot be read as that shape, a named
/// dep-info that is missing or lies outside the target, or a non-empty seed in
/// which none is named is `seed_layout_unrecognised`: publishing it would
/// silently skip the protection for some units. Returns how many files were aged.
///
/// Kept for generations that still hold a full build. This pass is removed once
/// the real-Cargo `older_sources_rebuild_after_deps_only_seed` invariant passes
/// in CI: a deps-only seed has no member units to protect, so no ageing is needed.
#[cfg(any(unix, test))]
fn age_dep_info(
    target: &Path,
    budget: &SeedBudget,
    cancel: Option<&provenant_core::CancelToken>,
) -> Result<usize, &'static str> {
    use std::{collections::BTreeSet, ffi::OsStr, io::Read, path::Component};
    /// A fingerprint lists a unit's dependencies; none approaches this.
    const MAX_FINGERPRINT_BYTES: u64 = 1 << 20;
    fn named(path: &Path, name: &str) -> bool {
        path.file_name() == Some(OsStr::new(name))
    }
    let stop = || {
        if cancel.is_some_and(|token| token.is_cancelled()) {
            Err(fallback_reasons::CANCELLED)
        } else if budget.expired() {
            Err(budget.exhausted_code())
        } else {
            Ok(())
        }
    };
    let failed = |_: std::io::Error| fallback_reasons::FRESHNESS_RESET_FAILED;
    fn visit(
        directory: &Path,
        depth: usize,
        stop: &dyn Fn() -> Result<(), &'static str>,
        fingerprints: &mut Vec<std::path::PathBuf>,
    ) -> Result<(), &'static str> {
        if depth > MAX_TREE_DEPTH {
            return Err(fallback_reasons::TARGET_TOO_DEEP);
        }
        let failed = |_: std::io::Error| fallback_reasons::FRESHNESS_RESET_FAILED;
        let holds_fingerprints = named(directory, "fingerprint")
            || named(directory, ".fingerprint")
            || directory
                .parent()
                .is_some_and(|parent| named(parent, ".fingerprint"));
        for entry in std::fs::read_dir(directory).map_err(failed)? {
            stop()?;
            let entry = entry.map_err(failed)?;
            let kind = entry.file_type().map_err(failed)?;
            if kind.is_dir() {
                visit(&entry.path(), depth + 1, stop, fingerprints)?;
            } else if kind.is_file()
                && holds_fingerprints
                && entry.path().extension() == Some(OsStr::new("json"))
            {
                fingerprints.push(entry.path());
            }
        }
        Ok(())
    }
    let mut fingerprints = Vec::new();
    visit(target, 0, &stop, &mut fingerprints)?;

    let mut dep_infos = BTreeSet::new();
    for fingerprint in &fingerprints {
        stop()?;
        let mut bytes = Vec::new();
        std::fs::File::open(fingerprint)
            .map_err(failed)?
            .take(MAX_FINGERPRINT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(failed)?;
        let local = (bytes.len() as u64 <= MAX_FINGERPRINT_BYTES)
            .then(|| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .flatten()
            .and_then(|value| match value.get("local") {
                Some(serde_json::Value::Array(local)) => Some(local.clone()),
                _ => None,
            })
            .ok_or(fallback_reasons::SEED_LAYOUT_UNRECOGNISED)?;
        for entry in local {
            let Some(check) = entry.get("CheckDepInfo") else {
                continue;
            };
            let relative = check
                .get("dep_info")
                .and_then(serde_json::Value::as_str)
                .map(Path::new)
                .filter(|path| {
                    path.components().next().is_some()
                        && path
                            .components()
                            .all(|component| matches!(component, Component::Normal(_)))
                })
                .ok_or(fallback_reasons::SEED_LAYOUT_UNRECOGNISED)?;
            dep_infos.insert(relative.to_path_buf());
        }
    }
    for relative in &dep_infos {
        stop()?;
        let path = target.join(relative);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => {}
            _ => return Err(fallback_reasons::SEED_LAYOUT_UNRECOGNISED),
        }
        std::fs::File::open(&path)
            .and_then(|file| file.set_modified(std::time::SystemTime::UNIX_EPOCH))
            .map_err(failed)?;
    }
    let empty = std::fs::read_dir(target).map_err(failed)?.next().is_none();
    if dep_infos.is_empty() && !empty {
        return Err(fallback_reasons::SEED_LAYOUT_UNRECOGNISED);
    }
    Ok(dep_infos.len())
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
        return Err(store.deadline.exhausted_code());
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
        return Err(store.deadline.exhausted_code());
    }
    if depth > MAX_TREE_DEPTH {
        return Err(store.too_deep);
    }
    for entry in source.entries().map_err(|_| store.read_failed)? {
        if store.deadline.expired() {
            return Err(store.deadline.exhausted_code());
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
                // Off APFS or a reflink filesystem the clone fails and the
                // target stays unseeded: a copy cannot fit the seed budget.
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

    #[test]
    fn a_spent_budget_or_cancelled_clone_removes_its_stage() {
        let setting = provenant_config::SeedBudgetSetting::compiled_default();
        for (reason, expected) in [
            (
                BudgetStep::Clone.detail(),
                Outcome::budget_fallback(BudgetStep::Clone.detail(), &setting),
            ),
            (
                BudgetStep::Aging.detail(),
                Outcome::budget_fallback(BudgetStep::Aging.detail(), &setting),
            ),
            (
                fallback_reasons::CANCELLED,
                Outcome::fallback(fallback_reasons::CANCELLED),
            ),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let cache = tmp.path().join("cache");
            std::fs::create_dir(&cache).unwrap();
            generation(tmp.path(), "001", "v1", "host");
            let outcome = seed_target_with(tmp.path(), &cache, "v1", "host", |_, stage| {
                stage.mkdir(std::ffi::OsStr::new("partial"), 0o700).unwrap();
                Err(reason)
            });
            assert_eq!(outcome, expected);
            assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 0);
        }
    }

    #[test]
    fn a_recorded_size_is_reported_before_clone_without_walking_the_seed() {
        for recorded in [Some(4096_u64), None] {
            let tmp = tempfile::tempdir().unwrap();
            let cache = tmp.path().join("cache");
            std::fs::create_dir(&cache).unwrap();
            let cargo_profile = provenant_config::LaneBuildSettings::default()
                .cargo_build_profile(&[])
                .unwrap();
            publish_generation(
                tmp.path(),
                "001",
                &crate::target_seed::Manifest {
                    project_key: String::new(),
                    rustc_vv: "v1".into(),
                    host_triple: "host".into(),
                    commit: "abc".into(),
                    profile: "dev".into(),
                    cargo_profile: cargo_profile.clone(),
                    features: vec![],
                    flags: vec![],
                    allocated_bytes: recorded,
                    contents: "deps".into(),
                },
                None,
            );
            let seen = std::rc::Rc::new(std::cell::Cell::new(None));
            let reported = seen.clone();
            let outcome = seed_target_cancellable(
                tmp.path(),
                &cache,
                SeedCompatibility {
                    version: "v1",
                    triple: "host",
                    cargo_profile: &cargo_profile,
                    project_key: "",
                },
                None,
                |bytes| reported.set(bytes),
                |_, _, _| Ok(()),
            );
            assert!(outcome.is_seeded(), "{outcome:?}");
            assert_eq!(seen.get(), recorded);
        }
    }

    #[test]
    fn budget_step_details_are_snake_case_and_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for detail in BUDGET_STEPS {
            assert!(
                !detail.is_empty()
                    && !detail.starts_with('_')
                    && !detail.ends_with('_')
                    && !detail.contains("__")
                    && detail
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte == b'_'),
                "{detail} is not snake_case"
            );
            assert!(is_budget_detail(detail));
            assert!(seen.insert(detail), "duplicate step detail {detail}");
        }
        assert_eq!(seen.len(), BUDGET_STEPS.len());
    }

    #[test]
    fn a_budget_fallback_names_its_step_and_the_setting_source() {
        let setting = provenant_config::SeedBudgetSetting::compiled_default();
        let outcome = Outcome::budget_fallback(BudgetStep::Clone.detail(), &setting);
        let warning = outcome
            .fallback_warning("the Cargo target", "builds start cold")
            .unwrap();
        assert!(warning.contains("seed_budget_exhausted"), "{warning}");
        assert!(warning.contains("seed_budget_clone"), "{warning}");
        assert!(
            warning.contains("budget 5m (source compiled default)"),
            "{warning}"
        );
        assert!(warning.contains("ran out in clone"), "{warning}");
        assert_eq!(
            outcome.budget,
            Some(SeedBudgetInfo {
                setting: "5m".into(),
                source: "compiled default".into(),
                step: "clone".into(),
            })
        );
    }

    /// A test clock shared by every step budget, whose monotonic reading only
    /// moves when a test advances it. Clones of the handle share the reading,
    /// so a slow clone can be simulated without a real-time wait.
    #[derive(Clone)]
    struct AdvancingClock(std::sync::Arc<std::sync::atomic::AtomicU64>);
    impl AdvancingClock {
        fn new() -> Self {
            Self(std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)))
        }
        fn advance(&self, micros: u64) {
            self.0
                .fetch_add(micros, std::sync::atomic::Ordering::SeqCst);
        }
    }
    impl provenant_core::Clock for AdvancingClock {
        fn wall_now(&self) -> Result<provenant_core::WallTime, provenant_core::TimeError> {
            provenant_core::WallTime::from_micros(0)
                .map_err(|_| provenant_core::TimeError::WallRange)
        }
        fn mono_now(&self) -> provenant_core::MonoTime {
            provenant_core::MonoTime::from_micros(self.0.load(std::sync::atomic::Ordering::SeqCst))
        }
        fn wait_until(
            &self,
            _deadline: provenant_core::MonoTime,
        ) -> Result<(), provenant_core::TimeError> {
            Ok(())
        }
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn aging_starts_with_a_full_budget_after_a_slow_clone() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let cache = tmp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        generation(&state, "001", "v1", "host");
        let profile = provenant_config::LaneBuildSettings::default()
            .cargo_build_profile(&[])
            .unwrap();
        let clock = AdvancingClock::new();
        let budget_clock = clock.clone();
        let clone_clock = clock.clone();
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let recorded = seen.clone();
        let outcome = seed_target_cancellable_inner(
            &state,
            &cache,
            SeedCompatibility {
                version: "v1",
                triple: "host",
                cargo_profile: &profile,
                project_key: "",
            },
            None,
            |_| {},
            move |_, _, _| {
                // Simulate a slow clone consuming most of the default 5m step.
                clone_clock.advance(290 * 1_000_000);
                Ok(())
            },
            move |setting, step| {
                let budget =
                    SeedBudget::for_step_clock(Box::new(budget_clock.clone()), setting, step);
                recorded
                    .borrow_mut()
                    .push((step, budget.deadline.as_mono_time()));
                budget
            },
        );
        assert!(outcome.is_seeded(), "{outcome:?}");
        let seen = seen.borrow();
        let clone_at = seen
            .iter()
            .find(|(step, _)| *step == BudgetStep::Clone)
            .map(|(_, at)| *at)
            .expect("a clone budget");
        let aging_at = seen
            .iter()
            .find(|(step, _)| *step == BudgetStep::Aging)
            .map(|(_, at)| *at)
            .expect("an aging budget");
        // Aging's deadline was taken after the clone advanced the clock, so it
        // starts a full budget later rather than inheriting the clone's spend.
        assert_eq!(aging_at.as_micros() - clone_at.as_micros(), 290 * 1_000_000);
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn old_manifest_without_a_size_is_quiet_and_recorded_unmeasured() {
        let profile = provenant_config::LaneBuildSettings::default()
            .cargo_build_profile(&[])
            .unwrap();
        // A generation published before allocated_bytes existed: the field is
        // absent from the manifest and deserialises to None.
        let mut old = serde_json::to_value(crate::target_seed::Manifest {
            project_key: String::new(),
            rustc_vv: "v1".into(),
            host_triple: "host".into(),
            commit: "abc".into(),
            profile: "dev".into(),
            cargo_profile: profile.clone(),
            features: vec![],
            flags: vec![],
            allocated_bytes: Some(1),
            contents: "deps".into(),
        })
        .unwrap();
        old.as_object_mut().unwrap().remove("allocated_bytes");
        let manifest: crate::target_seed::Manifest = serde_json::from_value(old).unwrap();
        assert_eq!(manifest.allocated_bytes, None);

        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let cache = tmp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        publish_generation(&state, "001", &manifest, None);
        let reported = std::rc::Rc::new(std::cell::Cell::new(Some(1_u64)));
        let seen = reported.clone();
        let outcome = seed_target_cancellable(
            &state,
            &cache,
            SeedCompatibility {
                version: "v1",
                triple: "host",
                cargo_profile: &profile,
                project_key: "",
            },
            None,
            |bytes| reported.set(bytes),
            |_, _, _| Ok(()),
        );
        assert!(outcome.is_seeded(), "{outcome:?}");
        assert_eq!(seen.get(), None, "an old seed reserves no cache budget");
        let record = serde_json::to_value(&outcome).unwrap();
        assert_eq!(record["size_recorded"], serde_json::json!(false));
    }

    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn knob_setting_and_source_flow_into_the_budget_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        std::fs::write(
            state.join("config.toml"),
            "schema_version = 1\n[knobs]\n\"lanes.cargo_seed_budget\" = '45s'\n",
        )
        .unwrap();
        generation(&state, "001", "v1", "host");
        let profile = provenant_config::LaneBuildSettings::default()
            .cargo_build_profile(&[])
            .unwrap();
        // An always-spent budget makes selection fall back without a real wait.
        let outcome = seed_target_cancellable_inner(
            &state,
            &cache,
            SeedCompatibility {
                version: "v1",
                triple: "host",
                cargo_profile: &profile,
                project_key: "",
            },
            None,
            |_| {},
            |_, _, _| panic!("an expired selection never clones"),
            |_, step| SeedBudget::exhausted(step),
        );
        assert_eq!(outcome.reason, fallback_reasons::SEED_BUDGET_EXHAUSTED);
        assert_eq!(
            outcome.detail.as_deref(),
            Some(BudgetStep::Selection.detail())
        );
        assert_eq!(outcome.budget.as_ref().unwrap().setting, "45s");
        assert_eq!(outcome.budget.as_ref().unwrap().source, "host");
        let warning = outcome
            .fallback_warning("the Cargo target", "builds start cold")
            .unwrap();
        assert!(warning.contains("budget 45s (source host)"), "{warning}");
        assert!(
            warning.contains("ran out in generation selection"),
            "{warning}"
        );
    }

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
        publish_generation(
            state,
            name,
            &crate::target_seed::Manifest {
                project_key: project_key.into(),
                rustc_vv: version.into(),
                host_triple: triple.into(),
                commit: "abc".into(),
                profile: "dev".into(),
                cargo_profile,
                features: vec![],
                flags: vec![],
                allocated_bytes: None,
                contents: "deps".into(),
            },
            None,
        );
    }

    /// Publish a generation as `provenant worktree seed-build` does: its target
    /// and manifest, the complete marker, then the generation directory made
    /// read-only, which is what tells a reader the generation is complete. A
    /// built target is frozen read-only like the one seed-build publishes;
    /// without one the target is an empty, writable directory to fill in.
    fn publish_generation(
        state: &Path,
        name: &str,
        manifest: &crate::target_seed::Manifest,
        built: Option<&Path>,
    ) {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::target_seed::store_path(state)
            .join("generations")
            .join(name);
        std::fs::create_dir_all(&dir).unwrap();
        match built {
            Some(built) => {
                std::fs::rename(built, dir.join("target")).unwrap();
                freeze(&dir.join("target"));
            }
            None => std::fs::create_dir(dir.join("target")).unwrap(),
        }
        std::fs::write(dir.join("complete"), b"complete\n").unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec(manifest).unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    }

    /// Clear every write bit in a tree, as seed-build freezes a generation.
    fn freeze(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::symlink_metadata(path).unwrap();
        if metadata.file_type().is_symlink() {
            return;
        }
        if metadata.is_dir() {
            for entry in std::fs::read_dir(path).unwrap() {
                freeze(&entry.unwrap().path());
            }
        }
        std::fs::set_permissions(
            path,
            std::fs::Permissions::from_mode(metadata.permissions().mode() & !0o222),
        )
        .unwrap();
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

    /// Write a unit's fingerprint the way Cargo does, naming the dep-info the unit
    /// checks, and the dep-info itself.
    fn unit(target: &Path, directory: &str, name: &str, local: &str) -> PathBuf {
        let directory_path = target.join(directory);
        std::fs::create_dir_all(&directory_path).unwrap();
        std::fs::write(
            directory_path.join(format!("{name}.json")),
            format!(
                "{{\"rustc\":1,\"deps\":[],\"local\":[{local}],\"future_field\":{{\"x\":[1]}}}}"
            ),
        )
        .unwrap();
        directory_path
    }

    fn check_dep_info(path: &str) -> String {
        format!("{{\"CheckDepInfo\":{{\"dep_info\":\"{path}\",\"fingerprint\":\"mtime\"}}}}")
    }

    #[test]
    fn seeded_dep_info_named_by_fingerprints_is_aged_so_worktree_sources_decide_freshness()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let target = root.path().join("target");
        // Both Cargo build-dir layouts, host and cross-compiled: fingerprints under
        // `.fingerprint/<unit>/` and under `build/<package>/<hash>/fingerprint/`.
        let mut aged = Vec::new();
        for (directory, name, dep_info) in [
            (
                "debug/.fingerprint/member-0123456789abcdef",
                "lib-member",
                "debug/.fingerprint/member-0123456789abcdef/dep-lib-member",
            ),
            (
                "aarch64-apple-darwin/debug/.fingerprint/member-fedcba9876543210",
                "lib-member",
                "aarch64-apple-darwin/debug/.fingerprint/member-fedcba9876543210/dep-lib-member",
            ),
            (
                "debug/build/member/0123456789abcdef/fingerprint",
                "lib-member",
                "debug/build/member/0123456789abcdef/fingerprint/dep-lib-member",
            ),
            // A unit kind whose dep-info does not sit in the unit's own directory
            // is aged all the same, because the fingerprint names it.
            (
                "debug/build/member/0123456789abcdef/fingerprint",
                "test-lib-member",
                "debug/elsewhere/dep-test-lib-member",
            ),
            (
                "aarch64-apple-darwin/debug/build/member/fedcba9876543210/fingerprint",
                "build-script-build-script-build",
                "aarch64-apple-darwin/debug/build/member/fedcba9876543210/fingerprint/dep-build-script-build-script-build",
            ),
        ] {
            unit(&target, directory, name, &check_dep_info(dep_info));
            let path = target.join(dep_info);
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(&path, b"x")?;
            aged.push(path);
        }
        // A run unit that only reruns on changes names no dep-info, and a
        // registry-style unit is Precalculated: neither is touched.
        let run = unit(
            &target,
            "debug/build/member/0123456789abcdef/fingerprint",
            "run-build-script-build-script-build",
            "{\"RerunIfChanged\":{\"output\":\"out\",\"paths\":[\"build.rs\"]}},{\"Precalculated\":\"1.0.0\"}",
        );
        let kept = [
            run.join("run-build-script-build-script-build.json"),
            // A `dep-` file that no fingerprint names is not aged.
            target.join("debug/deps/dep-unrelated"),
            target.join("debug/deps/libmember.rmeta"),
        ];
        std::fs::create_dir_all(target.join("debug/deps"))?;
        for path in &kept[1..] {
            std::fs::write(path, b"x")?;
        }
        assert_eq!(
            age_dep_info(&target, &SeedBudget::new(), None),
            Ok(aged.len())
        );
        for path in &aged {
            assert_eq!(
                std::fs::metadata(path)?.modified()?,
                std::time::UNIX_EPOCH,
                "{path:?}"
            );
        }
        for path in &kept {
            assert_ne!(
                std::fs::metadata(path)?.modified()?,
                std::time::UNIX_EPOCH,
                "{path:?}"
            );
        }
        Ok(())
    }

    /// Ageing refuses, rather than skipping a unit, whenever a fingerprint cannot
    /// say which dep-info it checks.
    #[test]
    fn a_fingerprint_that_cannot_name_its_dep_info_refuses_the_seed() {
        let refuse = |prepare: &dyn Fn(&Path)| {
            let root = tempfile::tempdir().unwrap();
            let target = root.path().join("target");
            let good = "debug/build/good/0123456789abcdef/fingerprint";
            unit(
                &target,
                good,
                "lib-good",
                &check_dep_info(&format!("{good}/dep-lib-good")),
            );
            std::fs::write(target.join(good).join("dep-lib-good"), b"x").unwrap();
            assert_eq!(age_dep_info(&target, &SeedBudget::new(), None), Ok(1));
            prepare(&target);
            assert_eq!(
                age_dep_info(&target, &SeedBudget::new(), None),
                Err(fallback_reasons::SEED_LAYOUT_UNRECOGNISED)
            );
        };
        let bad = "debug/build/bad/fedcba9876543210/fingerprint";
        // The named dep-info is missing.
        refuse(&|target| {
            unit(
                target,
                bad,
                "lib-bad",
                &check_dep_info(&format!("{bad}/dep-lib-bad")),
            );
        });
        // The fingerprint is not JSON, or has no `local` list.
        refuse(&|target| {
            std::fs::create_dir_all(target.join(bad)).unwrap();
            std::fs::write(target.join(bad).join("lib-bad.json"), b"not json").unwrap();
        });
        refuse(&|target| {
            std::fs::create_dir_all(target.join(bad)).unwrap();
            std::fs::write(target.join(bad).join("lib-bad.json"), b"{\"deps\":[]}").unwrap();
        });
        // The named dep-info is not a path inside the target.
        for escape in ["../outside", "/etc/hosts", "", "a/../b"] {
            refuse(&|target| {
                unit(target, bad, "lib-bad", &check_dep_info(escape));
            });
        }
        // A named dep-info that is not a string.
        refuse(&|target| {
            unit(
                target,
                bad,
                "lib-bad",
                "{\"CheckDepInfo\":{\"dep_info\":7}}",
            );
        });
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
        // A mismatch names what was wanted and what the store holds.
        assert_eq!(
            run("unknown", "host"),
            Outcome::fallback_differs(
                fallback_reasons::TOOLCHAIN_MISMATCH,
                Difference::new("unknown", "v1, v2")
            )
        );
        assert_eq!(
            run("v1", "other"),
            Outcome::fallback_differs(
                fallback_reasons::TRIPLE_MISMATCH,
                Difference::new("other", "host")
            )
        );
        assert_eq!(run("v1", "host").status, "seeded");
        assert_eq!(run("v1", "host"), Outcome::skipped("existing_target"));
    }

    /// A full generation is obsolete: selection skips it with a stable reason
    /// rather than cloning member units that a lane's own sources can outdate.
    /// Spec: docs/specs/run.md#cargo-target-seed
    #[test]
    fn full_generation_is_skipped_by_selection() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let cache = tmp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        let profile = provenant_config::LaneBuildSettings::default()
            .cargo_build_profile(&[])
            .expect("default Cargo profile is UTF-8");
        let mut manifest = crate::target_seed::Manifest {
            project_key: String::new(),
            rustc_vv: "v1".into(),
            host_triple: "host".into(),
            commit: "abc".into(),
            profile: "dev".into(),
            cargo_profile: profile.clone(),
            features: vec![],
            flags: vec![],
            allocated_bytes: None,
            contents: String::new(),
        };
        publish_generation(&state, "001", &manifest, None);
        let compatibility = || SeedCompatibility {
            version: "v1",
            triple: "host",
            cargo_profile: &profile,
            project_key: "",
        };
        let outcome = seed_target_cancellable(
            &state,
            &cache,
            compatibility(),
            None,
            |_| {},
            |_, _, _| panic!("a full generation must never be cloned"),
        );
        assert_eq!(outcome.reason, fallback_reasons::FULL_GENERATION);

        // A deps generation in the store remains selectable.
        manifest.contents = "deps".into();
        publish_generation(&state, "002", &manifest, None);
        let outcome = seed_target_cancellable(
            &state,
            &cache,
            compatibility(),
            None,
            |_| {},
            |_, _, _| Ok(()),
        );
        assert_eq!(
            (outcome.status.as_str(), outcome.reason.as_str()),
            ("seeded", "cloned")
        );
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
        generation_for_profile(&state, "001", "v1", "host", "project-a", different.clone());

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
            Outcome::fallback_differs(
                fallback_reasons::CARGO_PROFILE_MISMATCH,
                Difference::new(describe_profile(&profile), describe_profile(&different))
            )
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
        let outcome = seed_target_with_reservation(
            &state,
            &cache,
            &worktree,
            &project_key,
            &home,
            &env,
            &provenant_core::CancelToken::new(),
            "target",
            false,
            |_| {},
        );
        assert_eq!(
            (outcome.status.as_str(), outcome.reason.as_str()),
            ("seeded", "cloned")
        );
        assert!(cache.join("target").is_dir());
    }

    /// A toolchain override is refused before any seed is read: the outcome
    /// records the refusal code and names the variable that differed, never
    /// the value the environment gave it.
    #[cfg(unix)]
    #[test]
    fn a_toolchain_override_refusal_names_the_variable_never_its_value() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        let refused = [("RUSTC".into(), "/secret/rustc".into())];
        let outcome = seed_target_with_reservation(
            &tmp.path().join("state"),
            &cache,
            &tmp.path().join("writer"),
            "",
            &tmp.path().join("home"),
            &refused,
            &provenant_core::CancelToken::new(),
            "target",
            false,
            |_: Option<u64>| panic!("a refused seed must not clone"),
        );
        assert_eq!(
            outcome,
            Outcome::fallback_refusal(
                fallback_reasons::TOOLCHAIN_UNVERIFIED,
                Refusal::differs(
                    codes::TOOLCHAIN_ENV_OVERRIDE,
                    "no compiler, target or build-flag override",
                    "RUSTC is set",
                ),
            )
        );
        assert!(!serde_json::to_string(&outcome).unwrap().contains("secret"));
        let warning = outcome
            .fallback_warning("the Cargo target", "builds in it start cold")
            .unwrap();
        assert!(
            warning.contains("found RUSTC is set") && !warning.contains("secret"),
            "{warning}"
        );
        assert!(!cache.join("target").exists());
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
            // A seed Cargo built names its dep-info from a fingerprint; one that
            // names none is refused as `seed_layout_unrecognised`.
            let unit_directory = if nested_tree {
                "debug/.fingerprint/member-0123456789abcdef"
            } else {
                ".fingerprint/member-0123456789abcdef"
            };
            let dep_info = format!("{unit_directory}/dep-lib-member");
            unit(
                &target,
                unit_directory,
                "lib-member",
                &check_dep_info(&dep_info),
            );
            let seed_dep_info = target.join(&dep_info);
            std::fs::write(&seed_dep_info, b"dep-info").unwrap();
            std::fs::set_permissions(&seed_dep_info, std::fs::Permissions::from_mode(0o444))
                .unwrap();
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
            assert_eq!(
                (outcome.status.as_str(), outcome.reason.as_str()),
                ("seeded", "cloned"),
                "{outcome:?}"
            );
            // Ageing reaches the private clone and never the seed it shares
            // blocks with.
            let modified = |path: &Path| std::fs::metadata(path).unwrap().modified().unwrap();
            assert_eq!(
                modified(&cache.join("target").join(&dep_info)),
                std::time::UNIX_EPOCH
            );
            assert_ne!(modified(&seed_dep_info), std::time::UNIX_EPOCH);
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
                &SeedBudget::exhausted(BudgetStep::Clone)
            ),
            Err(BudgetStep::Clone.detail())
        );
        assert!(std::fs::read_dir(tmp.path()).unwrap().next().is_none());
    }

    #[test]
    fn a_non_empty_target_without_dep_info_is_a_layout_refusal_not_a_silent_no_op() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        // Nothing to age in an empty seed, which is not an error.
        std::fs::create_dir(&target).unwrap();
        assert_eq!(age_dep_info(&target, &SeedBudget::new(), None), Ok(0));
        // Artifacts but no fingerprint directory: the layout is not one we can age.
        std::fs::create_dir_all(target.join("debug/deps")).unwrap();
        std::fs::write(target.join("debug/deps/libmember.rmeta"), b"x").unwrap();
        assert_eq!(
            age_dep_info(&target, &SeedBudget::new(), None),
            Err(fallback_reasons::SEED_LAYOUT_UNRECOGNISED)
        );
        let fingerprint = unit(
            &target,
            "debug/build/member/0123456789abcdef/fingerprint",
            "lib-member",
            &check_dep_info("debug/build/member/0123456789abcdef/fingerprint/dep-lib-member"),
        );
        std::fs::write(fingerprint.join("dep-lib-member"), b"x").unwrap();
        assert_eq!(
            age_dep_info(&target, &SeedBudget::exhausted(BudgetStep::Aging), None),
            Err(BudgetStep::Aging.detail())
        );
        let cancel = provenant_core::CancelToken::new();
        cancel.cancel();
        assert_eq!(
            age_dep_info(&target, &SeedBudget::new(), Some(&cancel)),
            Err(fallback_reasons::CANCELLED)
        );
    }

    #[test]
    fn a_seed_whose_layout_cannot_be_aged_is_not_published() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        generation(tmp.path(), "001", "v1", "host");
        let outcome = seed_target_with(tmp.path(), &cache, "v1", "host", |_, stage| {
            stage.mkdir(std::ffi::OsStr::new("debug"), 0o700).unwrap();
            Ok(())
        });
        assert_eq!(
            outcome,
            Outcome::fallback(fallback_reasons::SEED_LAYOUT_UNRECOGNISED)
        );
        assert!(
            outcome
                .fallback_warning("the Cargo target", "builds start cold")
                .is_some_and(|warning| warning.contains("seed_layout_unrecognised"))
        );
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 0);
    }

    /// A checkout that pins a release and a store holding another: the refusal
    /// names both, for a person who is not in a lane.
    struct OutsideLane {
        _root: tempfile::TempDir,
        home: PathBuf,
        worktree: PathBuf,
        cache: PathBuf,
        state: PathBuf,
        report: String,
    }

    fn outside_lane(release: &str) -> OutsideLane {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let worktree = root.path().join("writer");
        let cache = root.path().join("cache");
        let state = root.path().join("state");
        let bin = home.join(format!(".rustup/toolchains/{release}-test-host/bin"));
        for directory in [&bin, &worktree.join(".git"), &cache] {
            std::fs::create_dir_all(directory).unwrap();
        }
        std::fs::write(home.join(".rustup/settings.toml"), "[overrides]\n").unwrap();
        std::fs::write(worktree.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(
            worktree.join("rust-toolchain.toml"),
            format!("[toolchain]\nchannel = '{release}'\n"),
        )
        .unwrap();
        let report = format!("rustc {release}\nhost: test-host\nrelease: {release}\n");
        let rustc = bin.join("rustc");
        std::fs::write(
            &rustc,
            format!("#!/bin/sh\nprintf '{}'\n", report.replace('\n', "\\n")),
        )
        .unwrap();
        std::fs::set_permissions(&rustc, std::fs::Permissions::from_mode(0o755)).unwrap();
        OutsideLane {
            _root: root,
            home,
            worktree,
            cache,
            state,
            report,
        }
    }

    impl OutsideLane {
        fn seed(&self, environment: &[(&str, &PathBuf)]) -> WorktreeSeed {
            let environment: Vec<_> = environment
                .iter()
                .map(|(key, value)| (OsString::from(key), value.as_os_str().to_owned()))
                .collect();
            seed_worktree_target(
                &self.state,
                &self.home,
                &self.worktree,
                &self.worktree.join(".git"),
                &self.cache,
                &environment,
            )
        }
    }

    use std::ffi::OsString;
    use std::path::PathBuf;

    /// Run outside a lane, as a person's shell is: RUSTUP_HOME and CARGO_HOME
    /// unset. The toolchain resolves to the defaults under HOME, so the only thing
    /// left to refuse is a generation built with another compiler, and that
    /// refusal names both.
    #[test]
    fn unset_homes_resolve_to_the_defaults_and_a_mismatch_names_both_toolchains() {
        let fixture = outside_lane("1.99.1");
        generation(
            &fixture.state,
            "001",
            "rustc 1.98.0\nhost: test-host\nrelease: 1.98.0\n",
            "test-host",
        );
        let unset = fixture.seed(&[("HOME", &fixture.home)]);
        assert_eq!(unset.status, "fallback");
        assert_eq!(unset.reason, "toolchain_mismatch", "{unset:?}");
        assert_eq!(unset.detail, None);
        assert_eq!(
            unset.differs,
            Some(Difference::new("rustc 1.99.1", "rustc 1.98.0"))
        );
        let warning = unset.warning.as_deref().unwrap();
        assert!(
            warning.contains("expected rustc 1.99.1, found rustc 1.98.0"),
            "{warning}"
        );
        assert!(unset.repair.is_some_and(|text| text.contains("seed-build")));
        // Only the injected environment decides the Cargo home, whatever the test
        // process exports, and the outcome says which source won.
        assert_eq!(unset.cargo_home, Some("host HOME/.cargo"));
        // Spelling the defaults out changes nothing but that source: unset means
        // the defaults.
        let spelled = fixture.seed(&[
            ("HOME", &fixture.home),
            ("RUSTUP_HOME", &fixture.home.join(".rustup")),
            ("CARGO_HOME", &fixture.home.join(".cargo")),
        ]);
        assert_eq!(spelled.cargo_home, Some("host CARGO_HOME"));
        let without_source = |seed: &WorktreeSeed| {
            let mut value = serde_json::to_value(seed).unwrap();
            value.as_object_mut().unwrap().remove("cargo_home");
            value
        };
        assert_eq!(without_source(&spelled), without_source(&unset));
        assert!(!fixture.cache.join("target").exists());
    }

    #[test]
    fn outside_a_lane_a_rustup_home_elsewhere_is_refused_and_named() {
        let fixture = outside_lane("1.99.1");
        generation(&fixture.state, "001", &fixture.report, "test-host");
        let elsewhere = fixture.home.join("elsewhere");
        let seed = fixture.seed(&[("HOME", &fixture.home), ("RUSTUP_HOME", &elsewhere)]);
        assert_eq!(seed.reason, "toolchain_unverified");
        assert_eq!(seed.detail.as_deref(), Some(codes::RUSTUP_HOME_UNEXPECTED));
        let differs = seed.differs.as_ref().unwrap();
        assert_eq!(differs.expected, "RUSTUP_HOME unset or $HOME/.rustup");
        assert_eq!(differs.found, "RUSTUP_HOME set to another directory");
        // The refusal names the variable, never the directory it was set to.
        assert!(!serde_json::to_string(&seed).unwrap().contains("elsewhere"));
    }

    #[test]
    fn other_generation_mismatches_name_expected_and_found() {
        let fixture = outside_lane("1.99.1");
        generation(&fixture.state, "001", &fixture.report, "other-host");
        let seed = fixture.seed(&[("HOME", &fixture.home)]);
        assert_eq!(seed.reason, "triple_mismatch");
        assert_eq!(
            seed.differs,
            Some(Difference::new("test-host", "other-host"))
        );

        let fixture = outside_lane("1.99.1");
        let profile = provenant_config::LaneBuildSettings::default()
            .cargo_build_profile(&[])
            .expect("default Cargo profile is UTF-8");
        let mut different = profile.clone();
        different.debug = "full".into();
        generation_for_profile(
            &fixture.state,
            "001",
            &fixture.report,
            "test-host",
            "",
            different.clone(),
        );
        let seed = fixture.seed(&[("HOME", &fixture.home)]);
        assert_eq!(seed.reason, "cargo_profile_mismatch");
        assert_eq!(
            seed.differs,
            Some(Difference::new(
                describe_profile(&profile),
                describe_profile(&different)
            ))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn outside_a_lane_with_unset_homes_seeds_from_the_matching_generation() {
        let fixture = outside_lane("1.99.1");
        let project_key = crate::target_seed::project_key(&fixture.worktree.join(".git"));
        generation_for_project(
            &fixture.state,
            "001",
            &fixture.report,
            "test-host",
            &project_key,
        );
        let seed = fixture.seed(&[("HOME", &fixture.home)]);
        assert_eq!(
            (seed.status.as_str(), seed.reason.as_str()),
            ("seeded", "cloned"),
            "{seed:?}"
        );
        assert_eq!(seed.generation.as_deref(), Some("001"));
        assert!(seed.warning.is_none() && seed.differs.is_none());
        assert!(fixture.cache.join("target").is_dir());
    }

    // These tests exercise independent outcomes, not filesystem reflink support.
    #[cfg(unix)]
    fn copy_fixture_target(
        source: &Path,
        destination: &provenant_os::fs::Directory,
    ) -> Result<(), &'static str> {
        fn copy_entries(source: &Path, destination: &Path) -> std::io::Result<()> {
            for entry in std::fs::read_dir(source)? {
                let entry = entry?;
                let target = destination.join(entry.file_name());
                if entry.file_type()?.is_dir() {
                    std::fs::create_dir(&target)?;
                    copy_entries(&entry.path(), &target)?;
                } else {
                    std::fs::copy(entry.path(), target)?;
                }
            }
            Ok(())
        }
        copy_entries(source, &destination.anchored_path())
            .map_err(|_| fallback_reasons::CLONE_FAILED)
    }

    #[cfg(unix)]
    #[test]
    fn lint_target_seeds_independently() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let cache = tmp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        generation(&state, "001", "v1", "host");
        let source = crate::target_seed::store_path(&state).join("generations/001/target");
        let fingerprint = unit(
            &source,
            "debug/.fingerprint/dependency",
            "lib-dependency",
            &check_dep_info("debug/.fingerprint/dependency/dep-lib-dependency"),
        );
        std::fs::write(fingerprint.join("dep-lib-dependency"), b"dependency").unwrap();
        let profile = provenant_config::LaneBuildSettings::default()
            .cargo_build_profile(&[])
            .unwrap();
        let (main, lint) = crate::writer::seed_both_cargo_targets(|name| {
            seed_target_cancellable_inner_for_destination(
                &state,
                &cache,
                SeedCompatibility {
                    version: "v1",
                    triple: "host",
                    cargo_profile: &profile,
                    project_key: "",
                },
                None,
                |_| {},
                |src, dst, _| copy_fixture_target(src, dst),
                SeedBudget::for_step,
                name,
                true,
            )
        });
        let directory = provenant_os::fs::Directory::open_anchor(tmp.path()).unwrap();
        crate::writer::record_cargo_setup_file(&directory, "setup.json", &main, &lint).unwrap();
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(tmp.path().join("setup.json")).unwrap()).unwrap();
        for name in ["target", "target-lint"] {
            assert_eq!(
                std::fs::read(
                    cache
                        .join(name)
                        .join("debug/.fingerprint/dependency/dep-lib-dependency")
                )
                .unwrap(),
                b"dependency"
            );
        }
        assert_eq!(record["status"], "seeded");
        assert_eq!(record["lint_target"]["status"], "seeded");
        assert_eq!(record["seed_source"], record["lint_target"]["seed_source"]);
    }

    #[cfg(unix)]
    #[test]
    fn one_fallback_never_blocks_other() {
        for failed in ["target", "target-lint"] {
            let tmp = tempfile::tempdir().unwrap();
            let state = tmp.path().join("state");
            let cache = tmp.path().join("cache");
            std::fs::create_dir(&cache).unwrap();
            generation(&state, "001", "v1", "host");
            let source = crate::target_seed::store_path(&state).join("generations/001/target");
            let fingerprint = unit(
                &source,
                "debug/.fingerprint/dependency",
                "lib-dependency",
                &check_dep_info("debug/.fingerprint/dependency/dep-lib-dependency"),
            );
            std::fs::write(fingerprint.join("dep-lib-dependency"), b"dependency").unwrap();
            let profile = provenant_config::LaneBuildSettings::default()
                .cargo_build_profile(&[])
                .unwrap();
            let (main, lint) = crate::writer::seed_both_cargo_targets(|name| {
                seed_target_cancellable_inner_for_destination(
                    &state,
                    &cache,
                    SeedCompatibility {
                        version: "v1",
                        triple: "host",
                        cargo_profile: &profile,
                        project_key: "",
                    },
                    None,
                    |_| {},
                    |src, dst, _| {
                        if name == failed {
                            return Err(fallback_reasons::CLONE_FAILED);
                        }
                        copy_fixture_target(src, dst)
                    },
                    SeedBudget::for_step,
                    name,
                    true,
                )
            });
            let directory = provenant_os::fs::Directory::open_anchor(tmp.path()).unwrap();
            crate::writer::record_cargo_setup_file(&directory, "setup.json", &main, &lint).unwrap();
            let record: serde_json::Value =
                serde_json::from_slice(&std::fs::read(tmp.path().join("setup.json")).unwrap())
                    .unwrap();
            let (main_status, lint_status, successful) = if failed == "target" {
                ("fallback", "seeded", "target-lint")
            } else {
                ("seeded", "fallback", "target")
            };
            assert_eq!(record["status"], main_status);
            assert_eq!(record["lint_target"]["status"], lint_status);
            assert!(!cache.join(failed).exists());
            assert!(
                cache
                    .join(successful)
                    .join("debug/.fingerprint/dependency/dep-lib-dependency")
                    .is_file()
            );
        }
    }

    /// The #3561 protection against the Cargo that is actually pinned, not a tree
    /// built by hand: a seeded target must rebuild a member whose sources are
    /// older than the seed's, whatever layout Cargo gives its fingerprints, while
    /// a registry-style dependency stays fresh. The control shows the same
    /// target without ageing reuses the stale member, so the test can fail.
    #[cfg(target_os = "macos")]
    #[test]
    #[allow(clippy::disallowed_methods)] // Runs the pinned cargo, and reads the real file clock.
    fn a_seeded_target_rebuilds_members_and_keeps_registry_dependencies_fresh_in_real_cargo() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let write = |path: &str, text: &str| {
            let path = workspace.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            "rust-toolchain.toml",
            include_str!("../../../rust-toolchain.toml"),
        );
        write(
            "Cargo.toml",
            "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nvendored = \"1\"\ninner = { path = \"inner\" }\n\n[workspace]\n",
        );
        write(
            "src/lib.rs",
            "pub fn value() -> u32 { inner::value() + vendored::value() }\n",
        );
        // A member with a build script, that another member depends on: every
        // unit kind Cargo gives a local package, not only its library.
        write(
            "inner/Cargo.toml",
            "[package]\nname = \"inner\"\nversion = \"0.1.0\"\nedition = \"2021\"\nbuild = \"build.rs\"\n\n[dependencies]\nvendored = \"1\"\n",
        );
        write(
            "inner/build.rs",
            "fn main() { println!(\"cargo:rerun-if-changed=build.rs\"); }\n",
        );
        write(
            "inner/src/lib.rs",
            "pub fn value() -> u32 { vendored::value() }\n",
        );
        // A directory source stands in for the registry offline: like registry
        // packages its units list no source files in their dep-info.
        write(
            ".cargo/config.toml",
            "[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n[source.vendored-sources]\ndirectory = \"vendor\"\n",
        );
        write(
            "vendor/vendored/Cargo.toml",
            "[package]\nname = \"vendored\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
        );
        write(
            "vendor/vendored/src/lib.rs",
            "pub fn value() -> u32 { 1 }\n",
        );
        write(
            "vendor/vendored/.cargo-checksum.json",
            &format!("{{\"files\":{{}},\"package\":\"{}\"}}", "0".repeat(64)),
        );
        let build = |target: &Path| -> Vec<(String, bool)> {
            let output = std::process::Command::new(
                std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()),
            )
            .args(["build", "--offline", "--message-format=json"])
            .current_dir(&workspace)
            .env("CARGO_TARGET_DIR", target)
            .env("CARGO_INCREMENTAL", "0")
            .env_remove("CARGO_BUILD_BUILD_DIR")
            .output()
            .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout)
                .unwrap()
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .filter(|message| message["reason"] == "compiler-artifact")
                .map(|message| {
                    (
                        message["target"]["name"].as_str().unwrap().to_owned(),
                        message["fresh"].as_bool().unwrap(),
                    )
                })
                .collect()
        };
        let fresh = |artifacts: &[(String, bool)], name: &str| {
            artifacts
                .iter()
                .find(|(artifact, _)| artifact == name)
                .unwrap_or_else(|| panic!("no {name} artifact in {artifacts:?}"))
                .1
        };

        let built = tmp.path().join("built");
        let first = build(&built);
        for name in ["member", "inner", "build-script-build", "vendored"] {
            assert!(!fresh(&first, name), "{name}: {first:?}");
        }

        // Publish it as a host generation, sealed as seed-build seals one.
        let state = tmp.path().join("state");
        let profile = provenant_config::LaneBuildSettings::default()
            .cargo_build_profile(&[])
            .expect("default Cargo profile is UTF-8");
        publish_generation(
            &state,
            "001",
            &crate::target_seed::Manifest {
                project_key: String::new(),
                rustc_vv: "v1".into(),
                host_triple: "host".into(),
                commit: "abc".into(),
                profile: "dev".into(),
                cargo_profile: profile.clone(),
                features: vec![],
                flags: vec![],
                allocated_bytes: None,
                // This fixture still carries a full target; its manifest is
                // marked deps so selection reaches the dep-info ageing check.
                contents: "deps".into(),
            },
            Some(&built),
        );
        let generations = crate::target_seed::store_path(&state).join("generations");
        let generation_dir = generations.join("001");
        let cancel = provenant_core::CancelToken::new();

        // The control: a clone whose dep-info keeps the seed's timestamps.
        let plain = tmp.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        clone_target_until(
            &generation_dir.join("target"),
            &provenant_os::fs::Directory::open_anchor(&plain).unwrap(),
            &cancel,
            &SeedBudget::new(),
        )
        .unwrap();

        // The seed's own path.
        let cache = tmp.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        let outcome = seed_target_cancellable(
            &state,
            &cache,
            SeedCompatibility {
                version: "v1",
                triple: "host",
                cargo_profile: &profile,
                project_key: "",
            },
            None,
            |_| {},
            |source, destination, budget| clone_target_until(source, destination, &cancel, budget),
        );
        assert_eq!(
            (outcome.status.as_str(), outcome.reason.as_str()),
            ("seeded", "cloned"),
            "{outcome:?}"
        );

        // The member's sources are older than the seed's dep-info, as in a
        // worktree checked out before the seed was built.
        let older = std::fs::metadata(workspace.join("src/lib.rs"))
            .unwrap()
            .modified()
            .unwrap()
            - std::time::Duration::from_secs(3600);
        for path in [
            "src/lib.rs",
            "Cargo.toml",
            "inner/src/lib.rs",
            "inner/Cargo.toml",
            "inner/build.rs",
        ] {
            std::fs::File::open(workspace.join(path))
                .unwrap()
                .set_modified(older)
                .unwrap();
        }
        let control = build(&plain);
        for name in ["member", "inner"] {
            assert!(
                fresh(&control, name),
                "without ageing the stale {name} is reused: {control:?}"
            );
        }
        // Every local unit rebuilds, the build script included, and the
        // registry-style dependency stays fresh.
        let seeded = build(&cache.join("target"));
        for name in ["member", "inner", "build-script-build"] {
            assert!(!fresh(&seeded, name), "{name}: {seeded:?}");
        }
        assert!(fresh(&seeded, "vendored"), "{seeded:?}");
        // Remove the frozen generation as collection does.
        assert!(
            remove_seed_tree(
                &provenant_os::fs::Directory::open_anchor(&generations).unwrap(),
                std::ffi::OsStr::new("001"),
                None,
                &SeedBudget::new(),
            )
            .unwrap()
        );
    }
}
