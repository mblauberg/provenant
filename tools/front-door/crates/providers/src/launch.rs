//! Prepared launches: exact argv, environment, stdin, cwd and the
//! confinement profile. Nothing here executes a process.
use crate::{Continuation, Prompt, ProviderError, StreamState};
use provenant_confinement::ConfinedPlan;
use provenant_confinement::permissions::{AccessMode, PathGrant, PathScope, ProviderProfile};
use provenant_records::{
    ResolvedRoute,
    policy::{Effect, Facts, Policy},
};
use serde::Deserialize;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// What the launcher feeds the child on stdin.
#[derive(Debug, PartialEq, Eq)]
pub enum Stdin {
    /// Write these bytes, then close.
    Bytes(Vec<u8>),
    /// Connect `/dev/null`.
    Null,
}

/// Filesystem preparation the launcher performs before anchoring and
/// compiling, all inside roots the plan or profile already names.
#[derive(Debug, PartialEq, Eq)]
pub enum SetupStep {
    /// Create this directory and any missing parents by descriptor-relative
    /// traversal from `/`, opening each component without following links,
    /// so a swapped entry fails instead of redirecting creation. Only
    /// directories created here get mode 0700; existing ones are never
    /// chmodded.
    CreateDir(PathBuf),
    /// Create a mode-0600 file with these bytes, refusing an existing entry.
    /// The launcher writes and syncs it before anchoring the profile.
    WriteFile { path: PathBuf, bytes: Vec<u8> },
    /// Like `WriteFile`, then mark the file owner-executable.
    WriteExecutable { path: PathBuf, bytes: Vec<u8> },
    /// Create `link` as a symbolic link to `target`, replacing nothing.
    Symlink { link: PathBuf, target: PathBuf },
}

/// Where an omitted optional grant was discovered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OmissionSource {
    /// A user skill folder or a resource linked from one.
    UserSkill,
    /// A shared toolchain or tool configuration (Rust, Node, Git).
    Toolchain,
    /// The provider's own configuration discovery.
    ProviderConfig,
    /// A shared package prefix was narrowed to the executable.
    ProviderPackage,
}

/// An optional grant left out of the profile, with its source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OmittedGrant {
    pub path: PathBuf,
    pub source: OmissionSource,
}

/// Resolved facts for one executable requirement in the compiled lane.
/// Receipts consume this value; they never repeat dependency discovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedToolRequirement {
    pub executable: PathBuf,
    pub dependency_closure: crate::dylib::DependencyClosure,
}

/// Discoveries from the toolchain producers. Optional roots are filtered and
/// applied once, after the adapter has assembled its required profile.
#[derive(Debug, Default)]
pub(crate) struct ToolRequirements {
    pub(crate) cargo_home: Option<provenant_confinement::cargo::CargoHome>,
    pub(crate) reads: Vec<PathGrant>,
    pub(crate) exec: Vec<PathGrant>,
    pub(crate) writes: Vec<PathGrant>,
    pub(crate) required_exec: Vec<PathGrant>,
    project_reads: Vec<PathGrant>,
    project_exec: Vec<PathGrant>,
    project_writes: Vec<PathGrant>,
    project_receipt: Option<serde_json::Value>,
    project_applied: bool,
    selected: Vec<PreparedToolRequirement>,
    omitted: Vec<OmittedGrant>,
    applied: bool,
}

impl PreparedToolRequirement {
    /// Public receipt data is bounded to file names; dependency paths stay in
    /// the prepared grant plan and never leak through route explanations.
    pub fn receipt(&self) -> serde_json::Value {
        use crate::{ClosureOutcome, IncompleteReason};
        let name = |path: &Path| {
            path.file_name().map_or_else(String::new, |name| {
                name.to_str()
                    .filter(|name| !name.chars().any(char::is_control))
                    .unwrap_or("<unrepresentable>")
                    .to_owned()
            })
        };
        let outcome = match self.dependency_closure.outcome {
            ClosureOutcome::Complete => "complete",
            ClosureOutcome::Incomplete => "incomplete",
            ClosureOutcome::UnsupportedFormat => "unsupported_format",
        };
        let mut unresolved = Vec::new();
        let mut reasons = Vec::new();
        for reason in &self.dependency_closure.reasons {
            let (kind, path) = match reason {
                IncompleteReason::LoaderRelative(path) => {
                    ("loader_relative", Some(Path::new(path)))
                }
                IncompleteReason::Missing(path) => ("missing", Some(Path::new(path))),
                IncompleteReason::Malformed(path) => ("malformed", Some(path.as_path())),
                IncompleteReason::Limit { .. } => ("limit", None),
                IncompleteReason::OmittedCredential(path) => {
                    ("credential_root", Some(path.as_path()))
                }
                IncompleteReason::OmittedProtected(path) => {
                    ("protected_root", Some(path.as_path()))
                }
                IncompleteReason::OmittedUnrepresentable(path) => {
                    ("unrepresentable_path", Some(path.as_path()))
                }
            };
            if !reasons.contains(&kind) {
                reasons.push(kind);
            }
            if let Some(path) = path {
                let filename = name(path);
                if !unresolved.contains(&filename) {
                    unresolved.push(filename);
                }
            }
        }
        serde_json::json!({
            "tool": name(&self.executable), "outcome": outcome,
            "reason": reasons.first(), "reasons": reasons, "unresolved": unresolved,
            "libraries": self.dependency_closure.libraries.iter().map(|path| name(path)).collect::<Vec<_>>(),
            "already_covered": self.dependency_closure.already_covered.iter().map(|path| name(path)).collect::<Vec<_>>(),
        })
    }
}

impl OmittedGrant {
    #[cfg(test)]
    pub(crate) fn skills(paths: Vec<PathBuf>) -> Vec<Self> {
        paths
            .into_iter()
            .map(|path| Self {
                path,
                source: OmissionSource::UserSkill,
            })
            .collect()
    }
}

/// Omit optional discoveries that cannot safely be represented or reach credentials.
pub(crate) fn filter_optional_grants(
    grants: &mut Vec<PathGrant>,
    start: usize,
    credential_roots: &provenant_core::CredentialRoots,
    policy: &Policy,
    source: OmissionSource,
    omitted: &mut Vec<OmittedGrant>,
) -> Result<(), ProviderError> {
    let mut index = start;
    let mut removed = 0usize;
    while index < grants.len() {
        let candidate = &grants[index].path;
        let target = std::fs::canonicalize(candidate).ok();
        let credential_path = target
            .as_ref()
            .filter(|path| provenant_confinement::refuses_lane_root(path, true, credential_roots))
            .or_else(|| {
                provenant_confinement::refuses_lane_root(candidate, true, credential_roots)
                    .then_some(candidate)
            });
        let omitted_path = credential_path
            .or_else(|| {
                (!provenant_confinement::permissions::profile_path_representable(candidate))
                    .then_some(candidate)
            })
            .or_else(|| {
                target.as_ref().filter(|path| {
                    !provenant_confinement::permissions::profile_path_representable(path)
                })
            })
            .cloned();
        if let Some(path) = omitted_path {
            grants.remove(index);
            removed += 1;
            if !omitted
                .iter()
                .any(|entry| entry.path == path && entry.source == source)
            {
                omitted.push(OmittedGrant { path, source });
            }
        } else {
            index += 1;
        }
    }
    if removed > 0 {
        policy_warning(
            policy,
            "LAUNCH_FILTER_OPTIONAL_GRANTS_NONE",
            format!("omitted {removed} optional grant(s) that could not be safely represented"),
        )?;
    }
    Ok(())
}

/// A provider launch ready for confinement. The launcher performs `setup`,
/// anchors `required_anchors(plan, &profile, worktree)`, compiles the plan with
/// `profile`, and runs `ConfinedLaunch::command(&argv)` with exactly `env`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionStateLocation {
    /// Session state is retained inside the attempt's private provider state.
    AttemptPrivate,
    /// The provider writes session state below this adapter-selected native root.
    Native { root: PathBuf },
}

pub struct LaunchSpec {
    /// `argv[0]` is the canonical executable path.
    pub argv: Vec<OsString>,
    /// The complete child environment; nothing is inherited.
    pub env: Vec<(OsString, OsString)>,
    pub stdin: Stdin,
    /// Credential bytes delivered through an inherited pipe, never the environment.
    pub secret_input: Option<provenant_os::exec::SecretInput>,
    pub cwd: PathBuf,
    pub setup: Vec<SetupStep>,
    /// Discovered optional grants omitted during preparation.
    pub omitted_discovery: Vec<OmittedGrant>,
    /// Exact staged skill content and warnings used by this launch.
    pub user_skills: crate::StagedUserSkills,
    pub(crate) tool_requirements: ToolRequirements,
    pub(crate) git_wrapper: crate::lane::GitWrapper,
    /// The immutable effective policy snapshot used to prepare this launch.
    pub(crate) policy: Policy,
    /// Declared host credential roots used to validate grants after preparation.
    pub(crate) credential_roots: provenant_core::CredentialRoots,
    pub profile: ProviderProfile,
    /// Adapter-selected location where this launch retains its session state.
    pub session_state: SessionStateLocation,
    /// Decoder state bound to any continuation identity.
    pub stream: StreamState,
}

/// The prepared facts which the profile and receipt use for launch access.
#[derive(Debug, PartialEq, Eq)]
pub struct AppliedAccess {
    pub libraries: Vec<PathGrant>,
    pub staged_content: Option<PathGrant>,
    pub toolchain_roots: Vec<PathGrant>,
    pub omitted_grants: Vec<OmittedGrant>,
}

impl AppliedAccess {
    /// Portable receipt projection; host paths outside HOME have no portable
    /// anchor, so only their final component is retained.
    pub fn receipt(
        &self,
        home: &Path,
        credential_roots: &provenant_core::CredentialRoots,
    ) -> serde_json::Value {
        use sha2::{Digest, Sha256};
        use std::os::unix::ffi::OsStrExt;
        let display = |path: &Path| {
            if path
                .to_str()
                .is_none_or(|text| text.chars().any(char::is_control))
            {
                return "<unrepresentable path>".into();
            }
            path.strip_prefix(home)
                .map(|path| format!("~/{}", path.display()))
                .unwrap_or_else(|_| {
                    format!(
                        "<external>/{}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    )
                })
        };
        let path_id = |path: &Path| {
            let mut digest = Sha256::new();
            digest.update(b"provenant.applied-path.v1\0");
            digest.update(path.as_os_str().as_bytes());
            format!("sha256:{}", provenant_core::hex(&digest.finalize()))
        };
        let grant = |grant: &PathGrant| {
            serde_json::json!({
                "path": display(&grant.path), "scope": grant.scope,
                "path_id": path_id(&grant.path),
            })
        };
        serde_json::json!({
            "libraries": self.libraries.iter().map(grant).collect::<Vec<_>>(),
            "staged_content": self.staged_content.as_ref().map(|_| "<attempt>/skills"),
            "toolchain_roots": self.toolchain_roots.iter().map(grant).collect::<Vec<_>>(),
            "omitted_grants": self.omitted_grants.iter().map(|entry| serde_json::json!({
                "code": "discovered_grant_omitted",
                "reason": if entry.source == OmissionSource::ProviderPackage { "shared_package_prefix" } else if provenant_confinement::refuses_lane_root(&entry.path, true, credential_roots) { "credential_root" } else if !provenant_confinement::permissions::profile_path_representable(&entry.path) { "unrepresentable_path" } else { "protected_root" },
                "credential_source": credential_roots.refusal(
                    &entry.path, provenant_core::CredentialScope::Subtree, true,
                ).map(|root| &root.source),
                "path": display(&entry.path),
                "path_id": path_id(&entry.path),
                "source": match entry.source { OmissionSource::UserSkill => "user_skill", OmissionSource::Toolchain => "toolchain", OmissionSource::ProviderConfig => "provider_config", OmissionSource::ProviderPackage => "provider_package" },
            })).collect::<Vec<_>>(),
        })
    }
}

impl LaunchSpec {
    /// Record already-validated project access without rediscovering paths.
    /// The confinement compiler still validates the resulting profile.
    pub fn set_project_requirements(
        &mut self,
        reads: Vec<PathGrant>,
        exec: Vec<PathGrant>,
        writes: Vec<PathGrant>,
        receipt: serde_json::Value,
    ) {
        self.tool_requirements.project_reads = reads;
        self.tool_requirements.project_exec = exec;
        self.tool_requirements.project_writes = writes;
        self.tool_requirements.project_receipt = Some(receipt);
        self.tool_requirements.project_applied = false;
    }

    pub fn project_requirements_receipt(&self) -> Option<&serde_json::Value> {
        self.tool_requirements.project_receipt.as_ref()
    }

    /// Selected executable facts, after the requirements resolver ran.
    pub fn prepared_tools(&self) -> &[PreparedToolRequirement] {
        &self.tool_requirements.selected
    }

    pub fn applied_access(&self) -> AppliedAccess {
        debug_assert!(
            self.tool_requirements
                .omitted
                .iter()
                .all(|entry| self.omitted_discovery.contains(entry))
        );
        let reads = &self.profile.spec.runtime_reads;
        let mut libraries = Vec::new();
        for tool in self.prepared_tools() {
            for path in &tool.dependency_closure.libraries {
                let grant = exact(path);
                if reads.contains(&grant) && !libraries.contains(&grant) {
                    libraries.push(grant);
                }
            }
        }
        let staged_content = self
            .user_skills
            .root
            .as_ref()
            .map(subtree)
            .filter(|grant| reads.contains(grant));
        AppliedAccess {
            libraries,
            staged_content,
            toolchain_roots: self.profile.spec.tool_exec_roots.clone(),
            omitted_grants: self.omitted_discovery.clone(),
        }
    }

    /// Credential declarations used to decide which prepared grants were omitted.
    pub fn credential_roots(&self) -> &provenant_core::CredentialRoots {
        &self.credential_roots
    }
}

/// Frozen confinement policy and launch facts, compiled before launch by the
/// composition edge. Paths must be canonical: providers never resolve them.
pub struct PrepareInput<'a> {
    pub route: &'a ResolvedRoute,
    /// Provider-facing spelling for the invocation only. Route identity and
    /// continuation validation always use the canonical `route.model`.
    pub provider_model: Option<&'a str>,
    pub confined: &'a ConfinedPlan,
    pub prompt: &'a Prompt,
    /// Canonical image files already granted read access by the caller.
    pub images: &'a [PathBuf],
    /// Trusted named command from Fabric configuration, never prompt argv.
    pub command: Option<crate::command::Invocation<'a>>,
    pub continuation: Option<&'a Continuation>,
    pub grant_digest: &'a str,
    /// Resolved host/project/dispatch policy for this launch.
    pub policy: &'a Policy,
    /// Canonical path of the provider CLI resolved at dispatch.
    pub executable: &'a Path,
    /// The user's home directory.
    pub home: &'a Path,
    /// All declared host credential roots, including provider and account roots.
    pub credential_roots: provenant_core::CredentialRoots,
    /// The user's login name.
    pub user: &'a OsStr,
    /// Reads one host environment variable. Production passes
    /// `std::env::var_os`; tests pass a fixed lookup so the profiles do not
    /// depend on the runner.
    pub host_var: &'a dyn Fn(&str) -> Option<OsString>,
    pub skill_stage_max_bytes: u64,
    /// Claude's login token, read by the host outside the sandbox.
    /// Selected registered identity. Absent preserves the default login source.
    pub account: Option<&'a crate::identity::RegisteredAccount>,
    pub token: Option<&'a crate::claude::OauthToken>,
    /// Working directory: the worktree for writers, a read root otherwise.
    pub cwd: &'a Path,
    /// Whether this lane received a captured Git context in its prompt.
    pub git_context: bool,
    /// Caller-supplied randomness; Claude uses it as a fresh session id.
    pub nonce: [u8; 16],
    pub provenant: crate::LaneRuntime<'a>,
}

/// Apply a warning-only provider rule to a failed optional operation.
///
/// A configured Queue or Refuse effect cannot be fulfilled at these call
/// sites. Treat an unexpected effect as an unsupported launch policy instead
/// of silently dropping it; the policy asset constrains these rules to
/// Warn/Off.
pub(crate) fn policy_warning(
    policy: &Policy,
    code: &'static str,
    detail: impl Into<String>,
) -> Result<(), ProviderError> {
    let report = |message: &str| {
        use std::io::Write;
        let _ = writeln!(std::io::stderr().lock(), "warning: {message}");
    };
    match policy.check(code, Facts::violation(detail)) {
        Effect::Proceed => Ok(()),
        Effect::Warn(message) => {
            report(&message);
            Ok(())
        }
        Effect::Refuse(diagnostic) => {
            report(
                diagnostic
                    .detail
                    .as_deref()
                    .unwrap_or("provider policy refused an operation"),
            );
            Err(ProviderError::PolicyActionUnsupported { code })
        }
    }
}

/// Use the attempt scratch directory when a provider's separate private state
/// could not be represented. These optional rules are Warn/Off only, so the
/// confined scratch directory remains the usable fallback for both actions.
pub(crate) fn private_state_or_scratch<'a>(
    private_state: Option<&'a Path>,
    scratch: &'a Path,
    policy: &Policy,
    code: &'static str,
) -> Result<&'a Path, ProviderError> {
    if let Some(private_state) = private_state {
        return Ok(private_state);
    }
    policy_warning(
        policy,
        code,
        "separate private provider state is unavailable; using confined attempt scratch",
    )?;
    Ok(scratch)
}

impl PrepareInput<'_> {
    pub(crate) fn boundary_error<E>(
        &self,
        code: &'static str,
        detail: impl Into<String>,
        typed_error: E,
    ) -> E {
        self.policy
            .boundary_error(code, Facts::violation(detail), typed_error)
    }

    pub(crate) fn sandboxed(&self) -> bool {
        (self.host_var)(provenant_core::consumer_env::SANDBOX).is_none_or(|value| value != "off")
    }

    pub fn user_skills(&self) -> bool {
        self.route.user_skills()
    }

    pub(crate) fn stage_user_skills(&self) -> crate::StagedUserSkills {
        if !self.sandboxed() {
            return crate::StagedUserSkills::default();
        }
        let Some(source) = self
            .user_skills()
            .then(|| locate_user_skills(&self.route.adapter, self.home, self.host_var))
            .flatten()
        else {
            return crate::StagedUserSkills::default();
        };
        let scratch = &self.confined.permissions().scratch.path;
        let Some(name) = scratch.file_name() else {
            return crate::StagedUserSkills::default();
        };
        let mut name = name.to_os_string();
        name.push(format!("-skills-{}", provenant_core::hex(&self.nonce)));
        let destination = scratch.with_file_name(name);
        crate::skill_stage::capture(
            &source,
            &destination,
            self.home,
            &self.credential_roots,
            self.skill_stage_max_bytes,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    ReadOnly,
    WorktreeWrite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Session {
    Fresh,
    Resume,
}

/// OpenCode's own prompt defaults. Path-specific entries still come from the
/// launch plan and stay subject to the outer confinement profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OpenCodeNativePermissions {
    pub(crate) default_key: &'static str,
    pub(crate) default_value: &'static str,
    pub(crate) external_directory_key: &'static str,
    pub(crate) read_key: &'static str,
    pub(crate) path_value: &'static str,
}

pub(crate) const CLAUDE_NATIVE_ARGUMENTS: &[&str] = &[
    "--permission-mode",
    "bypassPermissions",
    "--tools",
    "default",
];
pub(crate) const CODEX_NATIVE_ARGUMENTS: &[&str] = &["--dangerously-bypass-approvals-and-sandbox"];
pub(crate) const AGY_NATIVE_ARGUMENTS: &[&str] = &["--dangerously-skip-permissions"];
pub(crate) const CURSOR_NATIVE_ARGUMENTS: &[&str] = &["--force"];
pub(crate) const OPENCODE_NATIVE_PERMISSIONS: OpenCodeNativePermissions =
    OpenCodeNativePermissions {
        default_key: "*",
        default_value: "allow",
        external_directory_key: "external_directory",
        read_key: "read",
        path_value: "allow",
    };

/// Checks shared by every adapter, returning its access and session mode.
pub(crate) fn admit(
    adapter: &'static str,
    input: &PrepareInput<'_>,
) -> Result<(Access, Session), ProviderError> {
    if input.route.adapter != adapter {
        return Err(input.policy.boundary_error(
            "LAUNCH_ADMIT_AUTHORITY",
            Facts::violation("the provider adapter does not match the resolved route"),
            ProviderError::IdentityMismatch,
        ));
    }
    if let Some(account) = input.account {
        account.validate_route(input.route)?;
    } else if provenant_records::AdapterSpec::get(adapter).is_some_and(|spec| {
        spec.registered_account
            && input
                .route
                .account_key
                .as_deref()
                .is_some_and(|key| !spec.is_unregistered_local(key))
    }) {
        return Err(input.policy.boundary_error(
            "LAUNCH_ADMIT_AUTHORITY",
            Facts::violation(
                "the resolved route requires a registered account that was not supplied",
            ),
            ProviderError::UnknownIdentity,
        ));
    }
    if let Some(continuation) = input.continuation {
        continuation.validate(input.route, input.grant_digest)?;
    }
    if !input.home.is_absolute() {
        return Err(input.policy.boundary_error(
            "LAUNCH_ADMIT_CREDENTIALS",
            Facts::violation(
                "the host home is not absolute, so credential exclusions cannot be evaluated",
            ),
            ProviderError::InvalidPath,
        ));
    }
    for path in [input.executable, input.cwd] {
        if !path.is_absolute() {
            return Err(input.policy.boundary_error(
                "LAUNCH_ADMIT_AUTHORITY_2",
                Facts::violation("the selected executable and working directory must be absolute"),
                ProviderError::InvalidPath,
            ));
        }
    }
    let access = match input.confined.permissions().mode {
        AccessMode::ReadOnly => Access::ReadOnly,
        AccessMode::WorktreeWrite(_) => Access::WorktreeWrite,
    };
    if access == Access::ReadOnly
        && !input
            .confined
            .permissions()
            .read_roots
            .iter()
            .any(|root| input.cwd.starts_with(&root.path))
    {
        return Err(input.policy.boundary_error(
            "LAUNCH_ADMIT_AUTHORITY",
            Facts::violation("the read-only working directory is outside every granted read root"),
            ProviderError::InvalidPath,
        ));
    }
    let session = if input.continuation.is_some() {
        Session::Resume
    } else {
        Session::Fresh
    };
    Ok((access, session))
}

/// Provide fixed tool-search fallbacks and omit an optional entry that PATH
/// cannot encode.
pub(crate) const SYSTEM_TOOL_SEARCH_PATH: &str = "/usr/bin:/bin";
pub(crate) const HOMEBREW_NODE_PREFIXES: [&str; 2] = ["/opt/homebrew", "/usr/local"];

/// Resolve the Node installation used by the first npm and node on requester
/// PATH. Native package config lookup must not execute npm to discover its
/// prefix. Unknown shims and split installations remain unresolved.
pub(crate) fn selected_node_prefix(host_var: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let path = host_var("PATH")?;
    let homebrew_prefixes = HOMEBREW_NODE_PREFIXES.map(PathBuf::from);
    selected_node_prefix_from_path(&path, &homebrew_prefixes)
}

fn selected_node_prefix_from_path(path: &OsStr, homebrew_prefixes: &[PathBuf]) -> Option<PathBuf> {
    let npm = first_path_executable(path, "npm")?;
    let node = first_path_executable(path, "node")?;
    let selected_bin = npm.parent()?;
    let npm_bin = std::fs::canonicalize(npm.parent()?).ok()?;
    let node_bin = std::fs::canonicalize(node.parent()?).ok()?;
    if npm_bin != node_bin {
        return None;
    }
    let npm = std::fs::canonicalize(&npm).ok()?;
    let node = std::fs::canonicalize(node).ok()?;

    for prefix in homebrew_prefixes {
        if let Some(prefix) =
            homebrew_prefix_for_selected_bin(selected_bin, &npm_bin, &npm, &node, prefix)
        {
            return Some(prefix);
        }
    }

    let prefix = node.parent()?.parent()?.to_owned();
    if !npm.starts_with(&prefix) || !node.starts_with(&prefix) {
        return None;
    }
    (nvm_installation_prefix(&prefix) || npm_cli_package_layout(&prefix, &npm)).then_some(prefix)
}

fn homebrew_prefix_for_selected_bin(
    selected_bin: &Path,
    canonical_bin: &Path,
    npm: &Path,
    node: &Path,
    prefix: &Path,
) -> Option<PathBuf> {
    let canonical_prefix = std::fs::canonicalize(prefix).ok()?;
    let stable_bin = prefix.join("bin");
    if selected_bin == stable_bin && canonical_bin == std::fs::canonicalize(&stable_bin).ok()? {
        return homebrew_node_cellar_formula(&canonical_prefix, npm, node)
            .map(|_| prefix.to_owned());
    }

    let mut components = selected_bin.strip_prefix(prefix).ok()?.components();
    if components.next()?.as_os_str() != OsStr::new("opt") {
        return None;
    }
    let formula = components.next()?.as_os_str().to_str()?;
    if components.next()?.as_os_str() != OsStr::new("bin")
        || components.next().is_some()
        || (formula != "node" && !formula.starts_with("node@"))
    {
        return None;
    }
    let canonical_formula = canonical_prefix.join("Cellar").join(formula);
    let resolved_formula = homebrew_node_cellar_formula(&canonical_prefix, npm, node)?;
    (resolved_formula == canonical_formula && canonical_bin.starts_with(&canonical_formula))
        .then(|| prefix.to_owned())
}

fn homebrew_node_cellar_formula(
    canonical_prefix: &Path,
    npm: &Path,
    node: &Path,
) -> Option<PathBuf> {
    let cellar = canonical_prefix.join("Cellar");
    let npm_formula = npm
        .strip_prefix(&cellar)
        .ok()?
        .components()
        .next()?
        .as_os_str()
        .to_str()?;
    let node_formula = node
        .strip_prefix(&cellar)
        .ok()?
        .components()
        .next()?
        .as_os_str()
        .to_str()?;
    if npm_formula != node_formula || (node_formula != "node" && !node_formula.starts_with("node@"))
    {
        return None;
    }
    let formula = cellar.join(node_formula);
    (npm.starts_with(&formula) && node.starts_with(&formula)).then_some(formula)
}

fn nvm_installation_prefix(prefix: &Path) -> bool {
    let versioned = prefix.file_name().and_then(OsStr::to_str);
    let node = prefix.parent().and_then(Path::file_name);
    let versions = prefix
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name);
    versioned.is_some_and(|name| name.starts_with('v'))
        && node == Some(OsStr::new("node"))
        && versions == Some(OsStr::new("versions"))
}

fn npm_cli_package_layout(prefix: &Path, npm: &Path) -> bool {
    npm.strip_prefix(prefix)
        .is_ok_and(|path| path == Path::new("lib/node_modules/npm/bin/npm-cli.js"))
}

pub(crate) fn first_path_executable(path: &OsStr, name: &str) -> Option<PathBuf> {
    for directory in std::env::split_paths(path) {
        // Relative and empty PATH entries depend on the launch cwd. Preserve
        // the existing selector if that earlier lookup cannot be resolved.
        if directory.as_os_str().is_empty() || !directory.is_absolute() {
            return None;
        }
        let executable = directory.join(name);
        if provenant_os::sys::is_executable(&executable) {
            return Some(executable);
        }
    }
    None
}

pub(crate) fn tool_search_path(
    home: &Path,
    include_cargo: bool,
    policy: &Policy,
) -> Result<OsString, ProviderError> {
    let cargo = home.join(".cargo/bin");
    let mut path = OsString::new();
    if include_cargo {
        if std::env::join_paths([&cargo]).is_ok() {
            path.push(cargo);
            path.push(":");
        } else {
            policy_warning(
                policy,
                "LAUNCH_TOOL_SEARCH_PATH_NONE",
                "the optional Cargo bin directory cannot be encoded in PATH; it was omitted",
            )?;
        }
    }
    path.push(SYSTEM_TOOL_SEARCH_PATH);
    Ok(path)
}

/// Native launches inherit the host environment in memory; provider selectors
/// are applied by each adapter. Sandboxed launches use a minimal environment:
/// the user's identity, filtered requester PATH followed by fixed tool-search
/// fallbacks, and TMPDIR under the attempt's private tmp directory. Required
/// private wrappers are prepended during finalisation. zsh ignores TMPDIR for the
/// temporary files behind here-documents and here-strings and defaults
/// TMPPREFIX to `/tmp/zsh`, which a lane cannot write, so TMPPREFIX points
/// into the same directory. Repository-specific Git object configuration
/// belongs to the private directory.
/// Callers append provider keys.
pub(crate) fn base_env(
    input: &PrepareInput<'_>,
    scratch: &Path,
    path: impl AsRef<std::ffi::OsStr>,
) -> Result<Vec<(OsString, OsString)>, ProviderError> {
    if !input.sandboxed() {
        let mut env: Vec<_> = std::env::vars_os()
            .filter(|(name, _)| name != provenant_core::consumer_env::SANDBOX && name != "HOME")
            .collect();
        // Apply the requester's path before adapters add their own wrappers.
        if let Some(path) = (input.host_var)("PATH") {
            env.retain(|(name, _)| name != "PATH");
            env.push(("PATH".into(), path));
        }
        // Establish the host's selected home before an adapter may isolate it.
        env.push(("HOME".into(), input.home.into()));
        env.push((provenant_core::consumer_env::SANDBOX.into(), "off".into()));
        return Ok(env);
    }
    let fallbacks = path.as_ref();
    let path = match (input.host_var)("PATH") {
        Some(requester) if !requester.is_empty() => std::env::join_paths(
            std::env::split_paths(&requester).chain(std::env::split_paths(fallbacks)),
        )
        .map_err(|_| ProviderError::InvalidPath)?,
        _ => fallbacks.to_owned(),
    };
    let mut env = vec![
        (provenant_core::consumer_env::SANDBOX.into(), "on".into()),
        ("HOME".into(), input.home.into()),
        ("USER".into(), input.user.into()),
        ("PATH".into(), path),
        ("TMPDIR".into(), scratch.into()),
        ("TMPPREFIX".into(), scratch.join("zsh").into()),
        ("LANG".into(), "en_US.UTF-8".into()),
    ];
    env.extend(provenant_os::sys::git_identity(
        input.provenant.git,
        input.cwd,
    ));
    Ok(env)
}

/// Repository lanes use a complete private Git directory, including its index
/// and operation state. Shared metadata is read-only. The Git launcher applies
/// this binding inside the worktree, even when a provider's subprocess clears
/// its environment, and nowhere else.
pub(crate) fn private_git_objects(
    input: &PrepareInput<'_>,
    spec: &mut LaunchSpec,
) -> Result<(), ProviderError> {
    if matches!(input.confined.permissions().mode, AccessMode::ReadOnly)
        && input.confined.permissions().read_only.is_none()
    {
        let git = input.provenant.git;
        let bin = input
            .confined
            .permissions()
            .scratch
            .path
            .join("read-only-git");
        spec.setup.push(SetupStep::CreateDir(bin.clone()));
        spec.setup.push(SetupStep::WriteExecutable {
            path: bin.join("git"),
            bytes: crate::lane::git_wrapper(git, &Default::default())?,
        });
        spec.env
            .retain(|(key, _)| key != provenant_core::consumer_env::GIT);
        spec.env.push((
            provenant_core::consumer_env::GIT.into(),
            bin.join("git").into(),
        ));
        grant_exec_path(git, spec);
        spec.profile.spec.tool_exec_roots.push(subtree(&bin));
        spec.profile
            .spec
            .process_executables
            .extend([exact("/bin/sh"), exact(git)]);
        if let Some((_, path)) = spec.env.iter_mut().find(|(key, _)| key == "PATH") {
            match std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&*path))) {
                Ok(value) => *path = value,
                Err(_) => {
                    policy_warning(
                        input.policy,
                        "LAUNCH_PRIVATE_GIT_OBJECTS_NONE",
                        "the optional read-only Git launcher cannot be encoded in PATH; the selected Git remains explicitly granted",
                    )?;
                }
            }
        }
        return Ok(());
    }
    let scratch = &input.confined.permissions().scratch.path;
    let private = scratch.join("git-common");
    let config = scratch.join("gitconfig");
    let identity = provenant_os::sys::git_global_identity(input.provenant.git);
    let identity = identity
        .iter()
        .filter(|(key, _)| matches!(key.to_str(), Some("GIT_AUTHOR_NAME" | "GIT_AUTHOR_EMAIL")))
        .collect::<Vec<_>>();
    // Git normally reads the XDG file before ~/.gitconfig. An explicit host
    // override replaces both, just as it does for Git itself.
    let globals = if let Some(path) = (input.host_var)("GIT_CONFIG_GLOBAL") {
        vec![PathBuf::from(path)]
    } else {
        let xdg = (input.host_var)("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| input.home.join(".config"));
        vec![xdg.join("git/config"), input.home.join(".gitconfig")]
    };
    let mut config_text = String::new();
    for path in globals.iter().filter(|path| path.is_file()) {
        // Both Git's include and the exact sandbox grant must name the same
        // physical file: anchor validation rejects symlink spellings.
        let path = std::fs::canonicalize(path).map_err(|_| {
            input.policy.boundary_error(
                "LAUNCH_PRIVATE_GIT_OBJECTS_GIT_ISOLATION",
                Facts::violation("the host Git configuration could not be bound into the private writer Git view"),
                ProviderError::InvalidPath,
            )
        })?;
        let value = path.to_str().ok_or_else(|| {
            input.policy.boundary_error(
                "LAUNCH_PRIVATE_GIT_OBJECTS_GIT_ISOLATION",
                Facts::violation("the host Git configuration path cannot be represented for the private writer Git view"),
                ProviderError::InvalidPath,
            )
        })?;
        if value.chars().any(char::is_control) {
            return Err(input.policy.boundary_error(
                "LAUNCH_PRIVATE_GIT_OBJECTS_GIT_ISOLATION",
                Facts::violation("the host Git configuration path contains control characters"),
                ProviderError::InvalidPath,
            ));
        }
        config_text.push_str(&format!(
            "[include]\n\tpath = {}\n",
            git_config_quote(value)
        ));
        spec.profile.spec.runtime_reads.push(exact(&path));
    }
    config_text.push_str("[user]\n\tuseConfigOnly = true\n");
    for (key, value) in &identity {
        let name = match key.to_str() {
            Some("GIT_AUTHOR_NAME") => "name",
            Some("GIT_AUTHOR_EMAIL") => "email",
            _ => continue,
        };
        let value = value.to_str().ok_or_else(|| {
            input.policy.boundary_error(
                "LAUNCH_PRIVATE_GIT_OBJECTS_GIT_ISOLATION",
                Facts::violation("the selected Git identity cannot be represented in the private writer Git view"),
                ProviderError::InvalidPath,
            )
        })?;
        if value.chars().any(char::is_control) {
            return Err(input.policy.boundary_error(
                "LAUNCH_PRIVATE_GIT_OBJECTS_GIT_ISOLATION",
                Facts::violation("the selected Git identity contains control characters"),
                ProviderError::InvalidPath,
            ));
        }
        config_text.push_str(&format!("\t{name} = {}\n", git_config_quote(value)));
    }
    spec.setup.push(SetupStep::WriteFile {
        path: config.clone(),
        bytes: config_text.into_bytes(),
    });
    if input.sandboxed() {
        spec.env.retain(|(key, _)| key != "GIT_CONFIG_NOSYSTEM");
        crate::lane::install_token_config(&mut spec.env).map_err(|error| {
            input.policy.boundary_error(
                "LAUNCH_PRIVATE_GIT_OBJECTS_GIT_ISOLATION",
                Facts::violation(
                    "Git credentials could not be isolated in the private writer Git environment",
                ),
                error,
            )
        })?;
    }
    if let Some((_, current)) = spec
        .env
        .iter_mut()
        .find(|(name, _)| name == "GIT_CONFIG_GLOBAL")
    {
        *current = config.as_os_str().to_owned();
    } else {
        spec.env
            .push(("GIT_CONFIG_GLOBAL".into(), config.as_os_str().to_owned()));
    }
    // The repository binding stays out of the ambient environment.
    // Git's private global config carries the host
    // identity to any Git executable, while `useConfigOnly` prevents fallback
    // to a generated hostname identity. The wrapper limits the private binding
    // to this worktree.
    let worktree = input
        .confined
        .permissions()
        .read_only
        .as_ref()
        .map(|grants| grants.root())
        .filter(|root| input.cwd.starts_with(root))
        .unwrap_or(input.cwd);
    let mut bound = vec![
        ("GIT_DIR".into(), private.clone().into()),
        ("GIT_COMMON_DIR".into(), private.into()),
        ("GIT_WORK_TREE".into(), worktree.into()),
        (
            "GIT_OBJECT_DIRECTORY".into(),
            scratch.join("git-objects").into(),
        ),
    ];
    spec.git_wrapper.identity_config = Some(config);
    spec.env.retain(|(key, value)| {
        let identity = matches!(
            key.to_str(),
            Some(
                "GIT_AUTHOR_NAME"
                    | "GIT_AUTHOR_EMAIL"
                    | "GIT_COMMITTER_NAME"
                    | "GIT_COMMITTER_EMAIL"
            )
        );
        if identity {
            bound.push((key.clone(), value.clone()));
        }
        !identity || !input.sandboxed()
    });
    spec.git_wrapper.bound = Some(crate::lane::BoundGit {
        worktree: worktree.to_owned(),
        env: bound,
    });
    // A single source generates every Git shim, including Claude's scrub shim.
    spec.git_wrapper.scrub = true;
    spec.write_git_wrapper(&scratch.join("bin"), input.provenant.git)?;
    // The generated wrapper embeds the real executable, so this cannot recurse.
    spec.env
        .retain(|(key, _)| key != provenant_core::consumer_env::GIT);
    spec.env.push((
        provenant_core::consumer_env::GIT.into(),
        scratch.join("bin/git").into(),
    ));
    grant_exec_path(input.provenant.git, spec);
    spec.profile
        .spec
        .tool_exec_roots
        .push(subtree(scratch.join("bin")));
    spec.profile
        .spec
        .process_executables
        .extend([exact("/bin/sh"), exact(input.provenant.git)]);
    Ok(())
}

/// Quote a value for a Git config file. The config always enables
/// `user.useConfigOnly`, so an absent or malformed host identity cannot fall
/// back to Git's generated username and hostname.
fn git_config_quote(value: &str) -> String {
    let mut quoted = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\t' => quoted.push_str("\\t"),
            '\u{0008}' => quoted.push_str("\\b"),
            control if control.is_control() => {
                unreachable!("control characters are rejected before quoting")
            }
            character => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

/// Let the lane read and run the helpers (`git-remote-https`) in the exec path
/// the launcher pins for the selected Git, wherever that host keeps them.
fn grant_exec_path(git: &Path, spec: &mut LaunchSpec) {
    let Some(raw) = crate::lane::probe_exec_path(git) else {
        return;
    };
    // The wrapper pins the same resolved path, or none when it cannot be
    // represented; an omitted grant is recorded rather than failing the lane.
    match crate::lane::resolve_exec_path(&raw) {
        Some(core) => {
            spec.profile.spec.runtime_reads.push(subtree(&core));
            spec.profile.spec.tool_exec_roots.push(subtree(core));
            if let Some(git) = crate::lane::developer_git_for_exec_path(&raw) {
                if let Some(bin) = git.parent() {
                    spec.profile.spec.runtime_reads.push(subtree(bin));
                    spec.profile.spec.tool_exec_roots.push(subtree(bin));
                }
                spec.profile.spec.process_executables.push(exact(git));
            }
        }
        None => {
            if !spec
                .omitted_discovery
                .iter()
                .any(|entry| entry.path == raw && entry.source == OmissionSource::Toolchain)
            {
                spec.omitted_discovery.push(OmittedGrant {
                    path: raw,
                    source: OmissionSource::Toolchain,
                });
            }
        }
    }
}

/// Resolve all exact executable requirements in the profile once, then use
/// those same facts both to add exact read grants and to report preparation.
fn extend_unique(grants: &mut Vec<PathGrant>, additions: Vec<PathGrant>) {
    for grant in additions {
        if !grants.contains(&grant) {
            grants.push(grant);
        }
    }
}

pub(crate) fn prepare_tool_requirements(
    input: &PrepareInput<'_>,
    spec: &mut LaunchSpec,
) -> Result<(), ProviderError> {
    spec.complete_tool_requirements(input.confined, input.cwd)?;
    Ok(())
}

/// Every adapter finishes here, after provider-specific environment changes.
/// Identity, temporary files and Cargo configuration are lane bindings even
/// when the host environment is sparse or sandboxing is disabled.
pub(crate) fn finalise_provider_env(
    input: &PrepareInput<'_>,
    spec: &mut LaunchSpec,
) -> Result<(), ProviderError> {
    let scratch = &input.confined.permissions().scratch.path;
    let home = spec
        .env
        .iter()
        .find(|(key, _)| key == "HOME")
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| input.home.into());
    spec.env
        .retain(|(key, _)| !matches!(key.to_str(), Some("HOME" | "TMPDIR" | "TMPPREFIX")));
    spec.env.extend([
        ("HOME".into(), home),
        ("TMPDIR".into(), scratch.into()),
        ("TMPPREFIX".into(), scratch.join("zsh").into()),
    ]);
    if !input.sandboxed() {
        crate::package_cache::bind_native(input.home, input.cwd, input.host_var, &mut spec.env);
    }
    lane_instructions(input, spec)?;
    let hooks = lane_hooks(input, spec)?;
    let access = match input.confined.permissions().mode {
        provenant_confinement::permissions::AccessMode::ReadOnly => Access::ReadOnly,
        provenant_confinement::permissions::AccessMode::WorktreeWrite(_) => Access::WorktreeWrite,
    };
    let cache = lane_cache(input);
    let binding = match spec.tool_requirements.cargo_home.take() {
        Some(binding) => binding,
        None => {
            let binding =
                provenant_confinement::cargo::CargoHome::resolve(input.home, input.host_var)
                    .map_err(|_| ProviderError::InvalidPath)?;
            if input.sandboxed() {
                spec.tool_requirements.reads.extend(binding.reads());
                spec.tool_requirements.writes.extend(binding.writes());
                spec.tool_requirements.applied = false;
            }
            binding
        }
    };
    cargo_environment(
        &binding,
        input.home,
        cache,
        access,
        !input.sandboxed(),
        &mut spec.env,
    );
    binding.source_environment(input.confined.permissions().network, &mut spec.env);
    spec.tool_requirements.cargo_home = Some(binding);
    if input.sandboxed() {
        // Native shell tools are one lane capability. Adapters supply their
        // private runtime roots; this common layer supplies shared toolchains
        // and caches with identical access and network boundaries.
        spec.tool_requirements.applied = false;
        rust_tools(
            input,
            access,
            &mut spec.tool_requirements,
            &mut spec.env,
            &mut spec.setup,
        );
        node_tools(
            input.home,
            access,
            cache,
            &mut spec.tool_requirements,
            &mut spec.env,
        );
    }
    input
        .provenant
        .grant(&mut spec.profile.spec, &mut spec.setup, hooks);
    Ok(())
}

/// Project the instruction content bound into this product build into each
/// sandboxed harness's private configuration home, using only its install data.
fn lane_instructions(input: &PrepareInput<'_>, spec: &mut LaunchSpec) -> Result<(), ProviderError> {
    if !input.sandboxed() {
        return Ok(());
    }
    let Some(install) = provenant_records::AdapterSpec::get(&input.route.adapter)
        .and_then(|adapter| adapter.install)
    else {
        return Ok(());
    };
    if !install.instructions.is_supported() {
        return Ok(());
    }
    let instruction = install.instruction.ok_or(ProviderError::InvalidPath)?;
    let relative = Path::new(instruction);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(ProviderError::InvalidPath);
    }
    let (home_variable, suffix) = install.lane_home;
    let home = spec
        .env
        .iter()
        .find(|(key, _)| key == home_variable)
        .map(|(_, value)| PathBuf::from(value))
        .filter(|path| path.is_absolute())
        .ok_or(ProviderError::InvalidPath)?;
    let config_home = home.join(suffix);
    let destination = config_home.join(relative);
    let parent = destination.parent().ok_or(ProviderError::InvalidPath)?;
    spec.setup.push(SetupStep::CreateDir(parent.to_owned()));
    spec.setup.push(SetupStep::WriteFile {
        path: destination,
        bytes: provenant_records::INSTRUCTION_CONTENT.to_vec(),
    });
    Ok(())
}

/// Render Provenant's declared hooks into the isolated harness configuration.
/// Native Claude uses its explicit settings file in scratch, and OpenCode
/// always uses a private config home; other native harnesses keep their
/// install-managed host configuration untouched.
fn lane_hooks(input: &PrepareInput<'_>, spec: &mut LaunchSpec) -> Result<bool, ProviderError> {
    let Some(adapter) = provenant_records::AdapterSpec::get(&input.route.adapter) else {
        return Ok(false);
    };
    let Some(install) = adapter.install else {
        return Ok(false);
    };
    if !install.hooks.is_supported() {
        return Ok(false);
    }
    let warn_skip = |detail| {
        policy_warning(
            input.policy,
            "LANE_LANERUNTIME_HOOK_PROJECTION",
            format!(
                "{} {detail}; continuing without its lane hooks",
                adapter.name
            ),
        )
    };
    let Some(protocol) = install.hook_protocol else {
        warn_skip("hook protocol is not declared")?;
        return Ok(false);
    };
    let Some(hook_file) = install.hook_file else {
        warn_skip("hook file is not declared")?;
        return Ok(false);
    };
    let relative = Path::new(hook_file);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        warn_skip("hook file path is invalid")?;
        return Ok(false);
    }

    let config_home =
        if input.sandboxed() || protocol == provenant_records::HookProtocol::OpenCodePlugin {
            let (home_variable, suffix) = install.lane_home;
            let Some(home) = spec
                .env
                .iter()
                .find(|(key, _)| key == home_variable)
                .map(|(_, value)| PathBuf::from(value))
                .filter(|path| path.is_absolute())
            else {
                warn_skip("hook config home is unavailable")?;
                return Ok(false);
            };
            home.join(suffix)
        } else if install.hook_settings_arg.is_some() {
            input.confined.permissions().scratch.path.clone()
        } else {
            // This native harness keeps its install-managed host hook file. The
            // launcher must not create or rewrite a user configuration file.
            return Ok(true);
        };
    let destination = config_home.join(relative);
    let Some(parent) = destination.parent() else {
        warn_skip("hook file has no parent directory")?;
        return Ok(false);
    };
    let bytes = match protocol {
        provenant_records::HookProtocol::OpenCodePlugin => {
            provenant_config::hook_plugin_source(input.provenant.executable, input.provenant.state)
        }
        _ => provenant_config::lane_hook_settings(
            adapter.id,
            input.provenant.executable,
            input.provenant.state,
        )
        .and_then(|mut settings| {
            if install.hook_settings_arg.is_some() {
                settings["attribution"] = serde_json::json!({"commit": "", "pr": ""});
            }
            serde_json::to_vec(&settings)
                .map_err(|_| provenant_config::InstallError::InvalidConfig("hook settings"))
        }),
    };
    let bytes = match bytes {
        Ok(bytes) => bytes,
        Err(_) => {
            warn_skip(
                "hook settings cannot represent the selected Provenant executable or state path",
            )?;
            return Ok(false);
        }
    };
    spec.setup.push(SetupStep::CreateDir(parent.to_owned()));
    spec.setup.push(SetupStep::WriteFile {
        path: destination.clone(),
        bytes,
    });
    if let Some(argument) = install.hook_settings_arg {
        spec.argv
            .extend([argument.into(), destination.into_os_string()]);
    }
    Ok(true)
}

impl LaunchSpec {
    pub(crate) fn native_environment(&self) -> bool {
        self.env
            .iter()
            .any(|(name, value)| name == provenant_core::consumer_env::SANDBOX && value == "off")
    }

    /// The host Cargo binding, present once the host has finalised Cargo.
    pub fn cargo_home(&self) -> Option<&provenant_confinement::cargo::CargoHome> {
        self.tool_requirements.cargo_home.as_ref()
    }

    /// Effective host Cargo identity and source-write setting, for launch evidence.
    pub fn cargo_home_receipt(&self) -> serde_json::Value {
        serde_json::to_value(self.cargo_home()).unwrap_or_default()
    }

    pub fn configure_cargo_sources(
        &mut self,
        setting: Option<bool>,
        network: provenant_confinement::permissions::NetworkAccess,
    ) {
        let Some(binding) = self.tool_requirements.cargo_home.as_mut() else {
            return;
        };
        let sources: Vec<_> = {
            let mut widened = binding.clone();
            widened.configure_source_writes(Some(true));
            widened.source_writes().collect()
        };
        self.tool_requirements
            .writes
            .retain(|grant| !sources.contains(grant));
        self.profile
            .spec
            .tool_writes
            .retain(|grant| !sources.contains(grant));
        binding.configure_source_writes(setting);
        let source_writes = binding.source_writes;
        if !self.native_environment() && source_writes {
            self.tool_requirements.writes.extend(sources.clone());
            if self.tool_requirements.applied {
                self.profile.spec.tool_writes.extend(sources);
            }
        }
        if let Some(binding) = self.tool_requirements.cargo_home.as_ref() {
            binding.source_environment(network, &mut self.env);
        }
    }

    /// Resolve any additional exact executables added by launch configuration.
    pub fn complete_tool_requirements(
        &mut self,
        plan: &ConfinedPlan,
        cwd: &Path,
    ) -> Result<(), ProviderError> {
        if self.native_environment() {
            return Ok(());
        }
        let spec = self;
        use provenant_confinement::permissions::PathScope;

        if !spec.tool_requirements.applied {
            let omitted_before = spec.omitted_discovery.len();
            let mut reads = spec.tool_requirements.reads.clone();
            let mut exec = Vec::new();
            let mut writes = Vec::new();
            filter_optional_grants(
                &mut reads,
                0,
                &spec.credential_roots,
                &spec.policy,
                OmissionSource::Toolchain,
                &mut spec.omitted_discovery,
            )?;
            let required = &spec.tool_requirements.required_exec;
            for candidate in &spec.tool_requirements.exec {
                let mut one = vec![candidate.clone()];
                if !required.contains(candidate) {
                    filter_optional_grants(
                        &mut one,
                        0,
                        &spec.credential_roots,
                        &spec.policy,
                        OmissionSource::Toolchain,
                        &mut spec.omitted_discovery,
                    )?;
                }
                exec.extend(one);
            }
            let omitted_before_writes = spec.omitted_discovery.len();
            for candidate in &spec.tool_requirements.writes {
                match std::fs::canonicalize(&candidate.path) {
                    Ok(path)
                        if provenant_confinement::refuses_lane_root(
                            &path,
                            false,
                            &spec.credential_roots,
                        ) =>
                    {
                        if !spec.omitted_discovery.iter().any(|entry| {
                            entry.path == path && entry.source == OmissionSource::Toolchain
                        }) {
                            spec.omitted_discovery.push(OmittedGrant {
                                path,
                                source: OmissionSource::Toolchain,
                            });
                        }
                    }
                    Err(_)
                        if provenant_confinement::refuses_lane_root(
                            &candidate.path,
                            false,
                            &spec.credential_roots,
                        ) =>
                    {
                        if !spec.omitted_discovery.iter().any(|entry| {
                            entry.path == candidate.path
                                && entry.source == OmissionSource::Toolchain
                        }) {
                            spec.omitted_discovery.push(OmittedGrant {
                                path: candidate.path.clone(),
                                source: OmissionSource::Toolchain,
                            });
                        }
                    }
                    _ if !provenant_confinement::permissions::profile_path_representable(
                        &candidate.path,
                    ) =>
                    {
                        if !spec.omitted_discovery.iter().any(|entry| {
                            entry.path == candidate.path
                                && entry.source == OmissionSource::Toolchain
                        }) {
                            spec.omitted_discovery.push(OmittedGrant {
                                path: candidate.path.clone(),
                                source: OmissionSource::Toolchain,
                            });
                        }
                    }
                    Ok(path)
                        if !provenant_confinement::permissions::profile_path_representable(
                            &path,
                        ) =>
                    {
                        if !spec.omitted_discovery.iter().any(|entry| {
                            entry.path == path && entry.source == OmissionSource::Toolchain
                        }) {
                            spec.omitted_discovery.push(OmittedGrant {
                                path,
                                source: OmissionSource::Toolchain,
                            });
                        }
                    }
                    _ => writes.push(candidate.clone()),
                }
            }
            let omitted_writes = spec.omitted_discovery[omitted_before_writes..]
                .iter()
                .filter(|entry| entry.source == OmissionSource::Toolchain)
                .count();
            if omitted_writes > 0 {
                policy_warning(
                    &spec.policy,
                    "LAUNCH_LAUNCHSPEC_COMPLETE_TOOL_REQUIREMENTS_NONE",
                    format!("omitted {omitted_writes} optional tool write grant(s)"),
                )?;
            }
            // Applying is idempotent: a later stage (host Cargo, private Git
            // objects) reopens the requirements, and each grant lands once.
            let profile = &mut spec.profile.spec;
            extend_unique(&mut profile.runtime_reads, reads);
            extend_unique(&mut profile.tool_exec_roots, exec);
            extend_unique(&mut profile.tool_writes, writes);
            spec.tool_requirements.omitted = spec.omitted_discovery[omitted_before..].to_vec();
            spec.tool_requirements.applied = true;
        }
        if !spec.tool_requirements.project_applied {
            spec.profile
                .spec
                .runtime_reads
                .extend(spec.tool_requirements.project_reads.clone());
            spec.profile
                .spec
                .tool_exec_roots
                .extend(spec.tool_requirements.project_exec.clone());
            spec.profile
                .spec
                .tool_writes
                .extend(spec.tool_requirements.project_writes.clone());
            spec.tool_requirements.project_applied = true;
        }

        let mut executables = Vec::new();
        for grant in spec
            .profile
            .spec
            .process_executables
            .iter()
            .chain(&spec.profile.spec.tool_exec_roots)
        {
            if grant.scope == PathScope::Exact && !executables.contains(&grant.path) {
                executables.push(grant.path.clone());
            }
        }
        let already_prepared: std::collections::HashSet<_> = spec
            .tool_requirements
            .selected
            .iter()
            .map(|tool| tool.executable.clone())
            .collect();
        for executable in executables {
            if already_prepared.contains(&executable) {
                continue;
            }
            let mut excluded = vec![cwd, &plan.permissions().scratch.path];
            if let Some(private_state) = &plan.permissions().private_state {
                excluded.push(&private_state.path);
            }
            let closure = crate::dylib::dependency_closure(&executable, |path| {
                use crate::dylib::LibraryAccess;
                if provenant_confinement::refuses_lane_root(path, true, &spec.credential_roots) {
                    LibraryAccess::OmitCredential
                } else if plan
                    .permissions()
                    .protected_roots
                    .iter()
                    .chain(&plan.permissions().secret_roots)
                    .any(|root| {
                        path == root.path
                            || (root.scope == PathScope::Subtree && path.starts_with(&root.path))
                    })
                {
                    LibraryAccess::OmitProtected
                } else if excluded.iter().any(|root| path.starts_with(root))
                    || spec.profile.spec.runtime_reads.iter().any(|grant| {
                        grant.scope == PathScope::Subtree && path.starts_with(&grant.path)
                    })
                {
                    LibraryAccess::AlreadyCovered
                } else if !provenant_confinement::permissions::profile_path_representable(path) {
                    LibraryAccess::OmitUnrepresentable
                } else {
                    LibraryAccess::Grant
                }
            });
            if closure.outcome != crate::dylib::ClosureOutcome::Complete
                || !closure.omitted.is_empty()
            {
                policy_warning(
                    &spec.policy,
                    "LAUNCH_LAUNCHSPEC_COMPLETE_TOOL_REQUIREMENTS_NONE",
                    "an optional tool dependency closure is incomplete; omitted dependencies are recorded",
                )?;
            }
            for path in &closure.omitted {
                if !spec.omitted_discovery.iter().any(|omitted| {
                    omitted.path == *path && omitted.source == OmissionSource::Toolchain
                }) {
                    let omitted = OmittedGrant {
                        path: path.clone(),
                        source: OmissionSource::Toolchain,
                    };
                    spec.tool_requirements.omitted.push(omitted.clone());
                    spec.omitted_discovery.push(omitted);
                }
            }
            for library in &closure.libraries {
                let grant = exact(library);
                if !spec.profile.spec.runtime_reads.contains(&grant) {
                    spec.profile.spec.runtime_reads.push(grant);
                }
            }
            spec.tool_requirements
                .selected
                .push(PreparedToolRequirement {
                    executable,
                    dependency_closure: closure,
                });
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod git_tests {
    use super::*;

    #[test]
    fn incomplete_closure_receipt_keeps_only_file_names() {
        let tool = PreparedToolRequirement {
            executable: "/private/fixture/bin/rg".into(),
            dependency_closure: crate::DependencyClosure {
                outcome: crate::ClosureOutcome::Incomplete,
                libraries: vec!["/private/fixture/lib/libok.dylib".into()],
                reasons: vec![
                    crate::IncompleteReason::Missing("/secret/path/libmissing.dylib".into()),
                    crate::IncompleteReason::LoaderRelative("@rpath/librelative.dylib".into()),
                ],
                omitted: Vec::new(),
                already_covered: Vec::new(),
            },
        };
        let receipt = tool.receipt();
        assert_eq!(receipt["tool"], "rg");
        assert_eq!(receipt["outcome"], "incomplete");
        assert_eq!(receipt["reason"], "missing");
        assert_eq!(
            receipt["unresolved"],
            serde_json::json!(["libmissing.dylib", "librelative.dylib"])
        );
        assert!(!receipt.to_string().contains("/secret/"));
        assert!(!receipt.to_string().contains("/private/"));
    }

    #[test]
    fn applied_receipt_distinguishes_external_paths_without_exposing_them() {
        let applied = AppliedAccess {
            libraries: vec![
                exact("/external/first/libsame.dylib"),
                exact("/external/second/libsame.dylib"),
            ],
            staged_content: None,
            toolchain_roots: Vec::new(),
            omitted_grants: Vec::new(),
        };
        let roots = provenant_core::CredentialRoots::for_home(Path::new("/home/fixture"));
        let receipt = applied.receipt(Path::new("/home/fixture"), &roots);
        assert_eq!(
            receipt["libraries"][0]["path"],
            receipt["libraries"][1]["path"]
        );
        assert_ne!(
            receipt["libraries"][0]["path_id"],
            receipt["libraries"][1]["path_id"]
        );
        assert!(!receipt.to_string().contains("/external/first"));
    }

    #[test]
    fn reads_effective_identity_and_omits_missing_values() {
        let dir = tempfile::tempdir().unwrap();
        let git = Path::new("/usr/bin/git");
        assert!(
            provenant_os::fixture::git_command_at(git)
                .args(["init", "-q"])
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success()
        );
        for (key, value) in [
            ("user.name", "Lane O'Brien"),
            ("user.email", "lane@example.org"),
        ] {
            assert!(
                provenant_os::fixture::git_command_at(git)
                    .args(["config", "--local", key, value])
                    .current_dir(dir.path())
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let env = provenant_os::sys::git_identity(git, dir.path());
        for (key, value) in [
            ("GIT_AUTHOR_NAME", "Lane O'Brien"),
            ("GIT_COMMITTER_NAME", "Lane O'Brien"),
            ("GIT_AUTHOR_EMAIL", "lane@example.org"),
            ("GIT_COMMITTER_EMAIL", "lane@example.org"),
        ] {
            assert!(env.iter().any(|(k, v)| k == key && v == value));
        }
        assert!(
            provenant_os::sys::git_identity(Path::new("/nonexistent/git"), dir.path()).is_empty()
        );
    }
}

/// The lane's private, writable cache root: the writer's lane cache
/// (`Library/Caches/provenant-lane-cargo/<sha256>`) or a read-only lane's
/// scratch. Build output and mutable package-manager configuration live here.
pub(crate) fn lane_cache<'a>(input: &'a PrepareInput<'_>) -> &'a Path {
    match &input.confined.permissions().mode {
        provenant_confinement::permissions::AccessMode::ReadOnly => {
            &input.confined.permissions().scratch.path
        }
        provenant_confinement::permissions::AccessMode::WorktreeWrite(authority) => {
            &authority.cargo_cache().path
        }
    }
}

/// Keep the selected files visible to the single closure resolver even when
/// their broader toolchain root is also granted.
pub(crate) fn add_existing_tool(executable: &Path, tool_exec_roots: &mut Vec<PathGrant>) {
    if let Ok(executable) = std::fs::canonicalize(executable)
        && executable.is_file()
        && !tool_exec_roots.iter().any(|grant| grant.path == executable)
    {
        tool_exec_roots.push(exact(executable));
    }
}

/// Rust tools for every shell-capable lane. Installed toolchains and the host's
/// Cargo sources stay read-only unless configured otherwise. Build output stays
/// private. Native lanes use the same environment for target seed validation.
pub(crate) fn rust_tools(
    input: &PrepareInput<'_>,
    access: Access,
    requirements: &mut ToolRequirements,
    env: &mut Vec<(OsString, OsString)>,
    setup: &mut Vec<SetupStep>,
) {
    if !input.sandboxed() {
        return;
    }
    // Subtree grants must anchor to existing directories. Keep this decision
    // here so every adapter gets the same system-tool fallback on non-Rust
    // hosts; the grants below it hold either way.
    if rust_tools_available(input.home) {
        rust_toolchain(
            input,
            access,
            &mut requirements.reads,
            &mut requirements.exec,
            setup,
        );
    }
    system_tools(access, &mut requirements.reads, &mut requirements.exec, env);
    crate::swift::swift_tools(input, access, requirements, env, setup);
    let runtime_reads = &mut requirements.reads;
    let tool_exec_roots = &mut requirements.exec;
    runtime_reads.extend(git_global_files(&input.home.join(".config")));
    // Homebrew resolves bin/git into its versioned keg. Its remote helpers
    // live beside bin/, independently of whether Node enables Homebrew tools.
    // Grant only the selected Git's helper directory, and only to writers.
    if access == Access::WorktreeWrite
        && let Some(prefix) = input.provenant.git.parent().and_then(Path::parent)
    {
        let core = prefix.join("libexec/git-core");
        if core.is_dir() {
            runtime_reads.push(subtree(&core));
            tool_exec_roots.push(subtree(core));
        }
    }
    // Repository scripts (a Cargo rustc wrapper, test runners, `./scripts/*`)
    // run directly, whether or not Rust is installed. Children inherit this
    // profile, so exec stays confined; read and write grants and the protected
    // and credential denials are unchanged.
    let cwd = subtree(input.cwd);
    tool_exec_roots.push(cwd.clone());
    requirements.required_exec.push(cwd);
}

/// The Rust toolchain grants and Cargo environment of an installed Rust home.
fn rust_toolchain(
    input: &PrepareInput<'_>,
    access: Access,
    runtime_reads: &mut Vec<PathGrant>,
    tool_exec_roots: &mut Vec<PathGrant>,
    setup: &mut Vec<SetupStep>,
) {
    let cargo = input.home.join(".cargo");
    let rustup = input.home.join(".rustup");
    let cache = lane_cache(input);
    let target = cache.join("target");
    // The host prepares the writer cache itself under custody. Cargo
    // creates target/ after launch, so anchor the cache.
    // A read-only lane's cache is its scratch, which rustdoc also uses (via
    // TMPDIR) for the doc-test binaries it compiles and runs. The lane can
    // already execute everything it builds and its read-only worktree.
    tool_exec_roots.push(subtree(cache));
    runtime_reads.extend([subtree(cargo.join("bin")), subtree(&rustup)]);
    tool_exec_roots.extend([
        subtree(cargo.join("bin")),
        subtree(rustup.join("toolchains")),
    ]);
    for name in ["cargo", "rustc", "rustfmt", "clippy-driver"] {
        add_existing_tool(&cargo.join("bin").join(name), tool_exec_roots);
    }
    if access == Access::ReadOnly {
        setup.push(SetupStep::CreateDir(target));
    }
}

/// One finalisation seam for all native adapters, including those that skip
/// sandbox-only tool discovery. This must run before Run verifies target seeds.
fn cargo_environment(
    binding: &provenant_confinement::cargo::CargoHome,
    home: &Path,
    cache: &Path,
    access: Access,
    native: bool,
    env: &mut Vec<(OsString, OsString)>,
) {
    env.retain(|(key, _)| key != provenant_core::consumer_env::LANE_CARGO_LINT_TARGET);
    if native {
        for (name, value) in [
            (
                "CARGO_BUILD_JOBS",
                provenant_config::LaneBuildSettings::default()
                    .jobs()
                    .to_string(),
            ),
            ("CARGO_INCREMENTAL", "0".into()),
            ("CARGO_PROFILE_DEV_DEBUG", "line-tables-only".into()),
        ] {
            if !env.iter().any(|(key, _)| key == name) {
                env.push((name.into(), value.into()));
            }
        }
    }
    binding.lane_cargo_environment(home, cache, env);
    if access == Access::WorktreeWrite {
        // Lint-only runs set both CARGO_TARGET_DIR and CARGO_BUILD_BUILD_DIR to
        // this sibling. Cargo locks the build directory, so they then do not
        // queue behind test and build runs in the lane's own target/.
        env.push((
            provenant_core::consumer_env::LANE_CARGO_LINT_TARGET.into(),
            cache.join("target-lint").into_os_string(),
        ));
    }
}

fn rust_tools_available(home: &Path) -> bool {
    // Unit-test fixtures name a synthetic Rust host with no directories on
    // disk; fallback tests use real temporary homes.
    if cfg!(test) && home == Path::new("/fixture/home") {
        return true;
    }
    home.join(".cargo/bin").is_dir() && home.join(".rustup/toolchains").is_dir()
}

#[cfg(test)]
mod rust_tools_tests {
    use super::rust_tools_available;

    #[test]
    fn missing_rust_home_directories_select_system_tools() {
        let home = tempfile::tempdir().unwrap();
        assert!(!rust_tools_available(home.path()));
        std::fs::create_dir_all(home.path().join(".cargo/bin")).unwrap();
        assert!(!rust_tools_available(home.path()));
        std::fs::create_dir_all(home.path().join(".rustup/toolchains")).unwrap();
        assert!(rust_tools_available(home.path()));
    }
}

/// Public TLS files and macOS developer tools, independent of whether Rust
/// is installed. Keep these grants shared by Codex and Claude.
// The non-macOS no-op keeps the append-capable macOS signature.
#[cfg_attr(not(target_os = "macos"), allow(unused_variables, clippy::ptr_arg))]
pub(crate) fn system_tools(
    access: Access,
    runtime_reads: &mut Vec<PathGrant>,
    tool_exec_roots: &mut Vec<PathGrant>,
    env: &mut Vec<(OsString, OsString)>,
) {
    // Cargo's system LibreSSL reads its public default configuration
    // (openssl.cnf) and the TLS trust roots (cert.pem, certs/) to download
    // crates. Read-only; the whole public directory holds no secrets.
    #[cfg(target_os = "macos")]
    runtime_reads.push(subtree("/private/etc/ssl"));
    // Rust's system linker also goes through xcrun. Select the real CLT
    // installation when present, rather than opening the selected Xcode app.
    #[cfg(target_os = "macos")]
    {
        let developer = Path::new("/Library/Developer/CommandLineTools");
        if developer.is_dir() {
            runtime_reads.push(subtree(developer));
            tool_exec_roots.push(subtree(developer.join("usr/bin")));
            // `/usr/bin/python3` runs the tools' python3, a symlink into this
            // framework, and Seatbelt checks the resolved path.
            let python = developer.join("Library/Frameworks/Python3.framework");
            if python.is_dir() {
                tool_exec_roots.push(subtree(python));
            }
            if access == Access::WorktreeWrite {
                tool_exec_roots.push(subtree(developer.join("usr/libexec/git-core")));
            }
            env.extend([
                ("DEVELOPER_DIR".into(), developer.into()),
                ("SDKROOT".into(), developer.join("SDKs/MacOSX.sdk").into()),
            ]);
        }
    }
}

/// Git's default XDG global ignore and attributes files, read-only. Cargo's
/// built-in Git library lists files of path dependencies outside the
/// workspace and fails hard when it cannot read them. They hold no
/// credentials. Granted only when present, as a grant needs an existing path.
pub(crate) fn git_global_files(config_home: &Path) -> Vec<PathGrant> {
    ["git/ignore", "git/attributes"]
        .map(|name| config_home.join(name))
        .into_iter()
        .filter(|path| path.is_file())
        // XDG configuration commonly lives behind a dotfiles symlink. Exact
        // grants and their held parent anchors must use the physical spelling.
        .filter_map(|path| std::fs::canonicalize(path).ok())
        .map(exact)
        .collect()
}

/// A Homebrew Node toolchain (node, npm, pnpm, corepack) for every
/// shell-capable lane. The host's pnpm store, npm cache and Corepack cache are
/// never writable: the host later runs what they hold unconfined. Writers
/// redirect the npm and pnpm caches and the Corepack home into the lane-private
/// cache. pnpm derives the store from `PNPM_HOME` or the home default and
/// records it in `node_modules/.modules.yaml`, so lanes leave `PNPM_HOME` and
/// the store unset: an override makes a worktree the chair installed natively
/// look stale, and the lane cannot reinstall into the shared store. Read-only
/// lanes get a scratch npm and pnpm cache, and read and exec (no write) on the
/// host's existing Corepack cache so pnpm runs.
pub(crate) fn node_tools(
    home: &Path,
    access: Access,
    cache: &Path,
    requirements: &mut ToolRequirements,
    env: &mut Vec<(OsString, OsString)>,
) {
    for prefix in HOMEBREW_NODE_PREFIXES.map(Path::new) {
        if prefix.join("bin/node").exists() {
            homebrew_tools(prefix, requirements, env);
            let runtime_reads = &mut requirements.reads;
            let tool_exec_roots = &mut requirements.exec;
            let host_corepack = home.join(".cache/node/corepack");
            let corepack = if access == Access::ReadOnly && host_corepack.is_dir() {
                runtime_reads.push(subtree(&host_corepack));
                tool_exec_roots.push(subtree(&host_corepack));
                host_corepack
            } else {
                cache.join("corepack")
            };
            // The host store is readable but never writable, and stays at the
            // default pnpm derives when `PNPM_HOME` and the store are unset.
            for store in [home.join("Library/pnpm"), home.join(".local/share/pnpm")] {
                if store.is_dir() {
                    runtime_reads.push(subtree(store));
                }
            }
            // pnpm 12 reads the pnpm_config_* names; older pnpm and npm read
            // npm_config_*. Set both so every version keeps its cache in the lane.
            let pnpm_cache = cache.join("pnpm-cache");
            env.extend([
                ("npm_config_cache".into(), cache.join("npm").into()),
                ("npm_config_cache_dir".into(), pnpm_cache.clone().into()),
                ("pnpm_config_cache_dir".into(), pnpm_cache.into()),
                ("COREPACK_HOME".into(), corepack.into()),
                // pnpm 11 checks the dependency configuration before `run` and
                // installs by default. The lane must run what the host installed,
                // not reinstall into a store it cannot write.
                ("pnpm_config_verify_deps_before_run".into(), "false".into()),
            ]);
            // Run the installed pnpm whatever version a project pins. Switching
            // downloads `@pnpm/exe`, whose launcher stays a non-executable
            // placeholder when hard links are denied, so running it fails
            // with ENOEXEC.
            for name in ["manage_package_manager_versions", "package_manager_strict"] {
                for prefix in ["npm", "pnpm"] {
                    env.push((format!("{prefix}_config_{name}").into(), "false".into()));
                }
            }
            // Corepack may wrap pnpm as a shim. Ignore a project's
            // `packageManager` field and refuse downloads, so a pinned version
            // can never arrive as the non-executable placeholder above.
            env.extend([
                ("COREPACK_ENABLE_PROJECT_SPEC".into(), "0".into()),
                ("COREPACK_ENABLE_DOWNLOAD".into(), "0".into()),
            ]);
            return;
        }
    }
}

/// The prefix is readable, including `etc/` (OpenSSL configuration and CA
/// certificates), and its programs executable. `var/` stays unreadable: it
/// holds service data such as Postgres clusters. Nothing in it is writable.
pub(crate) fn homebrew_tools(
    prefix: &Path,
    requirements: &mut ToolRequirements,
    env: &mut [(OsString, OsString)],
) {
    let runtime_reads = &mut requirements.reads;
    let tool_exec_roots = &mut requirements.exec;
    let present = |names: &[&str]| -> Vec<PathBuf> {
        names
            .iter()
            .map(|name| prefix.join(name))
            .filter(|directory| directory.is_dir())
            .collect()
    };
    runtime_reads.extend(
        present(&[
            "bin",
            "sbin",
            "Cellar",
            "opt",
            "lib",
            "libexec",
            "etc",
            "share",
            "include",
            "Frameworks",
        ])
        .into_iter()
        .map(subtree),
    );
    tool_exec_roots.extend(
        present(&["bin", "sbin", "Cellar", "opt", "lib", "libexec"])
            .into_iter()
            .map(subtree),
    );
    for name in ["node", "npm", "pnpm", "corepack"] {
        add_existing_tool(&prefix.join("bin").join(name), tool_exec_roots);
    }
    if let Some((_, path)) = env.iter_mut().find(|(key, _)| key == "PATH") {
        path.push(":");
        path.push(prefix.join("bin"));
    }
}

/// The CLI's own install directory: the parent of a `bin/` directory, or the
/// executable's directory. A `bin/` directly under a hidden home tool tree
/// (`~/.cargo/bin`, `~/.local/bin`) grants only that `bin/`, never the tree
/// (`credentials.toml`, `share/`). A shared prefix (`/opt/homebrew`, `/usr/local`,
/// `/usr`) is refused: its `var/` must never become readable.
fn package_candidate(executable: &Path) -> Option<&Path> {
    match executable.parent() {
        Some(bin) if bin.file_name() == Some("bin".as_ref()) => {
            let tree = bin.parent();
            let hidden = tree
                .and_then(Path::file_name)
                .is_some_and(|name| name.as_encoded_bytes().starts_with(b"."));
            if hidden { Some(bin) } else { tree }
        }
        parent => parent,
    }
}

pub(crate) fn package_root(executable: &Path) -> Result<&Path, ProviderError> {
    let package = package_candidate(executable).ok_or(ProviderError::InvalidPath)?;
    let shared = [
        "/",
        "/opt",
        "/opt/homebrew",
        "/usr",
        "/usr/local",
        "/bin",
        "/usr/bin",
    ];
    if shared.iter().any(|prefix| package == Path::new(prefix)) {
        return Err(ProviderError::InvalidPath);
    }
    Ok(package)
}

/// Keep shared installation prefixes out of the profile without refusing launch.
/// The executable remains readable and executable; the omitted prefix is reported.
pub(crate) fn package_grant(
    executable: &Path,
    omitted: &mut Vec<OmittedGrant>,
    policy: &Policy,
) -> Result<PathGrant, ProviderError> {
    match package_root(executable) {
        Ok(root) => Ok(subtree(root)),
        Err(_) => {
            omitted.push(OmittedGrant {
                path: package_candidate(executable)
                    .unwrap_or(executable)
                    .to_owned(),
                source: OmissionSource::ProviderPackage,
            });
            policy_warning(
                policy,
                "LAUNCH_PACKAGE_GRANT_NONE",
                "a shared provider package prefix was omitted; the executable keeps an exact grant",
            )?;
            Ok(exact(executable))
        }
    }
}

pub(crate) fn exact(path: impl Into<PathBuf>) -> PathGrant {
    PathGrant {
        path: path.into(),
        scope: PathScope::Exact,
    }
}

pub(crate) fn subtree(path: impl Into<PathBuf>) -> PathGrant {
    PathGrant {
        path: path.into(),
        scope: PathScope::Subtree,
    }
}

/// The native user skills location, without exposing the rest of host config.
fn locate_user_skills(
    adapter: &str,
    home: &Path,
    host_var: &dyn Fn(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    let location = provenant_records::AdapterSpec::get(adapter)?.skills?;
    let path = location
        .environment
        .and_then(host_var)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(location.fallback))
        .join(location.suffix);
    path.is_dir().then_some(path)
}

/// Lowercase RFC 4122 version 4 spelling of caller randomness.
pub(crate) fn uuid_v4(mut bytes: [u8; 16]) -> String {
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = provenant_core::hex(&bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[cfg(test)]
mod git_config_quote_tests {
    #[test]
    fn git_config_identity_round_trips_metacharacters() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("identity.gitconfig");
        let identity = "Ann \"Lee\"; $HOME `git` \\ path\twith tab";
        std::fs::write(
            &config,
            format!("[user]\n\tname = {}\n", super::git_config_quote(identity)),
        )
        .unwrap();
        let output = provenant_os::fixture::git_command()
            .args(["config", "--file"])
            .arg(&config)
            .args(["--get", "user.name"])
            .env_remove("GIT_CONFIG_COUNT")
            .env_remove("GIT_CONFIG_KEY_0")
            .env_remove("GIT_CONFIG_KEY_1")
            .env_remove("GIT_CONFIG_KEY_2")
            .env_remove("GIT_CONFIG_KEY_3")
            .env_remove("GIT_CONFIG_VALUE_0")
            .env_remove("GIT_CONFIG_VALUE_1")
            .env_remove("GIT_CONFIG_VALUE_2")
            .env_remove("GIT_CONFIG_VALUE_3")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.strip_suffix(b"\n"), Some(identity.as_bytes()));
    }
}

#[cfg(test)]
mod selected_node_prefix_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn executable(path: &Path) {
        std::fs::create_dir_all(path.parent().expect("executable parent")).unwrap();
        std::fs::write(path, b"fixture executable").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn homebrew_node(prefix: &Path) -> (PathBuf, PathBuf) {
        let cellar = prefix.join("Cellar/node@22/22.18.0");
        let cellar_bin = cellar.join("bin");
        executable(&cellar_bin.join("node"));
        executable(&cellar_bin.join("npm"));
        let opt = prefix.join("opt");
        std::fs::create_dir_all(&opt).unwrap();
        symlink(&cellar, opt.join("node@22")).unwrap();
        let stable_bin = prefix.join("bin");
        std::fs::create_dir_all(&stable_bin).unwrap();
        symlink(cellar_bin.join("node"), stable_bin.join("node")).unwrap();
        symlink(cellar_bin.join("npm"), stable_bin.join("npm")).unwrap();
        (opt.join("node@22/bin"), cellar_bin)
    }

    fn nvm_node(prefix: &Path) -> PathBuf {
        let bin = prefix.join("bin");
        executable(&bin.join("node"));
        executable(&bin.join("npm"));
        bin
    }

    // Spec: docs/specs/supervisor.md#native-environment-and-sandbox-tool-stores
    #[test]
    fn homebrew_opt_node_formula_uses_stable_global_config_prefix() {
        let root = tempfile::tempdir().unwrap();
        let prefix = root.path().join("homebrew");
        let (selected_bin, cellar_bin) = homebrew_node(&prefix);
        let npm = std::fs::canonicalize(cellar_bin.join("npm")).unwrap();
        let node = std::fs::canonicalize(cellar_bin.join("node")).unwrap();
        let canonical_bin = std::fs::canonicalize(&selected_bin).unwrap();

        assert_eq!(
            homebrew_prefix_for_selected_bin(&selected_bin, &canonical_bin, &npm, &node, &prefix,),
            Some(prefix.clone()),
            "opt/node@22 must resolve to the stable Homebrew prefix, not the Cellar version"
        );
        let path = std::env::join_paths([&selected_bin]).unwrap();
        assert_eq!(
            selected_node_prefix_from_path(&path, std::slice::from_ref(&prefix)),
            Some(prefix.clone())
        );
        let stable_path = std::env::join_paths([prefix.join("bin")]).unwrap();
        assert_eq!(
            selected_node_prefix_from_path(&stable_path, std::slice::from_ref(&prefix)),
            Some(prefix)
        );
    }

    #[test]
    fn unsupported_same_bin_shims_do_not_become_node_installation_prefixes() {
        let root = tempfile::tempdir().unwrap();
        let volta_bin = root.path().join("volta/bin");
        executable(&volta_bin.join("node"));
        executable(&volta_bin.join("npm"));
        let path = std::env::join_paths([&volta_bin]).unwrap();

        assert_eq!(selected_node_prefix_from_path(&path, &[]), None);
    }

    #[test]
    fn canonical_npm_cli_package_layout_is_a_supported_node_prefix() {
        let root = tempfile::tempdir().unwrap();
        let prefix = root.path().join("node-install");
        let bin = prefix.join("bin");
        let npm_cli = prefix.join("lib/node_modules/npm/bin/npm-cli.js");
        executable(&bin.join("node"));
        executable(&npm_cli);
        std::fs::create_dir_all(&bin).unwrap();
        symlink(&npm_cli, bin.join("npm")).unwrap();
        let path = std::env::join_paths([&bin]).unwrap();

        assert_eq!(selected_node_prefix_from_path(&path, &[]), Some(prefix));
    }

    #[test]
    fn first_nvm_installation_wins_over_a_later_homebrew_opt_installation() {
        let root = tempfile::tempdir().unwrap();
        let nvm_prefix = root.path().join("nvm/versions/node/v24.0.1");
        let nvm_bin = nvm_node(&nvm_prefix);
        let homebrew_prefix = root.path().join("homebrew");
        let (homebrew_bin, cellar_bin) = homebrew_node(&homebrew_prefix);
        let npm = std::fs::canonicalize(cellar_bin.join("npm")).unwrap();
        let node = std::fs::canonicalize(cellar_bin.join("node")).unwrap();
        assert_eq!(
            homebrew_prefix_for_selected_bin(
                &homebrew_bin,
                &std::fs::canonicalize(&homebrew_bin).unwrap(),
                &npm,
                &node,
                &homebrew_prefix,
            ),
            Some(homebrew_prefix.clone()),
            "the later Homebrew fixture must be a recognized install layout"
        );

        let path = std::env::join_paths([&nvm_bin, &homebrew_bin]).unwrap();
        assert_eq!(
            selected_node_prefix_from_path(&path, std::slice::from_ref(&homebrew_prefix)),
            Some(nvm_prefix),
            "later Homebrew cache configuration must not override the first PATH installation"
        );
    }
}
