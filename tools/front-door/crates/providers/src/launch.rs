//! Prepared launches: exact argv, environment allow-list, stdin, cwd and the
//! confinement profile. Nothing here executes a process.
use crate::{Continuation, Prompt, ProviderError, StreamState};
use provenant_confinement::ConfinedPlan;
use provenant_confinement::permissions::{AccessMode, PathGrant, PathScope, ProviderProfile};
use provenant_records::ResolvedRoute;
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
    home: &Path,
    source: OmissionSource,
    omitted: &mut Vec<OmittedGrant>,
) {
    let mut index = start;
    while index < grants.len() {
        let candidate = &grants[index].path;
        let target = std::fs::canonicalize(candidate).ok();
        let omitted_path = target
            .as_ref()
            .filter(|path| provenant_confinement::refuses_lane_root(path, true, Some(home)))
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
}

/// A provider launch ready for confinement. The launcher performs `setup`,
/// anchors `required_anchors(plan, &profile, worktree)`, compiles the plan with
/// `profile`, and runs `ConfinedLaunch::command(&argv)` with exactly `env`.
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
    pub profile: ProviderProfile,
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
    pub fn receipt(&self, home: &Path) -> serde_json::Value {
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
                "reason": if entry.source == OmissionSource::ProviderPackage { "shared_package_prefix" } else if provenant_confinement::refuses_lane_root(&entry.path, true, Some(home)) { "credential_root" } else if !provenant_confinement::permissions::profile_path_representable(&entry.path) { "unrepresentable_path" } else { "protected_root" },
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
}

/// Frozen confinement policy and launch facts, compiled before launch by the
/// composition edge. Paths must be canonical: providers never resolve them.
pub struct PrepareInput<'a> {
    pub route: &'a ResolvedRoute,
    pub confined: &'a ConfinedPlan,
    pub prompt: &'a Prompt,
    /// Canonical image files already granted read access by the caller.
    pub images: &'a [PathBuf],
    /// Trusted named command from Fabric configuration, never prompt argv.
    pub command: Option<crate::command::Invocation<'a>>,
    pub continuation: Option<&'a Continuation>,
    pub grant_digest: &'a str,
    /// Canonical path of the provider CLI resolved at dispatch.
    pub executable: &'a Path,
    /// The user's home directory.
    pub home: &'a Path,
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

impl PrepareInput<'_> {
    pub fn user_skills(&self) -> bool {
        self.route.user_skills()
    }

    pub(crate) fn stage_user_skills(&self) -> crate::StagedUserSkills {
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
        crate::skill_stage::capture(&source, &destination, self.home, self.skill_stage_max_bytes)
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

/// Checks shared by every adapter, returning its access and session mode.
pub(crate) fn admit(
    adapter: &'static str,
    input: &PrepareInput<'_>,
) -> Result<(Access, Session), ProviderError> {
    if input.route.adapter != adapter {
        return Err(ProviderError::IdentityMismatch);
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
        return Err(ProviderError::UnknownIdentity);
    }
    if let Some(continuation) = input.continuation {
        continuation.validate(input.route, input.grant_digest)?;
    }
    for path in [input.executable, input.home, input.cwd] {
        if !path.is_absolute() {
            return Err(ProviderError::InvalidPath);
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
        return Err(ProviderError::InvalidPath);
    }
    let session = if input.continuation.is_some() {
        Session::Resume
    } else {
        Session::Fresh
    };
    Ok((access, session))
}

/// Preserve native tool paths and omit an optional entry that PATH cannot encode.
pub(crate) fn tool_search_path(home: &Path, include_cargo: bool) -> OsString {
    let cargo = home.join(".cargo/bin");
    let mut path = OsString::new();
    if include_cargo {
        if std::env::join_paths([&cargo]).is_ok() {
            path.push(cargo);
            path.push(":");
        } else {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: cannot include the Cargo bin directory in PATH; continuing without it"
            );
        }
    }
    path.push("/usr/bin:/bin");
    path
}

/// Minimal environment: the user's identity, a fixed search path and TMPDIR
/// under the attempt's private tmp directory. zsh ignores TMPDIR for the
/// temporary files behind here-documents and here-strings and defaults
/// TMPPREFIX to `/tmp/zsh`, which a lane cannot write, so TMPPREFIX points
/// into the same directory. Repository-specific Git object configuration
/// belongs to the private directory.
/// Callers append provider keys.
pub(crate) fn base_env(
    input: &PrepareInput<'_>,
    scratch: &Path,
    path: impl AsRef<std::ffi::OsStr>,
) -> Vec<(OsString, OsString)> {
    let mut env = vec![
        ("HOME".into(), input.home.into()),
        ("USER".into(), input.user.into()),
        ("PATH".into(), path.as_ref().to_owned()),
        ("TMPDIR".into(), scratch.into()),
        ("TMPPREFIX".into(), scratch.join("zsh").into()),
        ("LANG".into(), "en_US.UTF-8".into()),
    ];
    env.extend(provenant_os::sys::git_identity(
        input.provenant.git,
        input.cwd,
    ));
    env
}

/// Writer Git uses a complete private directory, including its index and all
/// operation state. Shared metadata is read-only. The Git launcher applies
/// this binding inside the worktree, even when a provider's subprocess clears
/// its environment, and nowhere else.
pub(crate) fn private_git_objects(
    input: &PrepareInput<'_>,
    spec: &mut LaunchSpec,
) -> Result<(), ProviderError> {
    if !matches!(
        input.confined.permissions().mode,
        AccessMode::WorktreeWrite(_)
    ) {
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
        grant_exec_path(git, spec);
        if let Some((_, path)) = spec.env.iter_mut().find(|(key, _)| key == "PATH") {
            match std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&*path))) {
                Ok(value) => *path = value,
                Err(_) => {
                    use std::io::Write;
                    let _ = writeln!(
                        std::io::stderr().lock(),
                        "warning: the read-only Git launcher cannot be included in PATH; continuing without it"
                    );
                }
            }
        }
        spec.profile.spec.process_executables.push(exact(git));
        return Ok(());
    }
    let scratch = &input.confined.permissions().scratch.path;
    let private = scratch.join("git-common");
    // The binding and the lane's commit identity stay out of the ambient
    // environment, so a fixture's `git init` in a temporary directory neither
    // reads nor commits to the lane. The Git shim applies them inside the worktree.
    let mut bound = vec![
        ("GIT_DIR".into(), private.clone().into()),
        ("GIT_COMMON_DIR".into(), private.into()),
        ("GIT_WORK_TREE".into(), input.cwd.into()),
        (
            "GIT_OBJECT_DIRECTORY".into(),
            scratch.join("git-objects").into(),
        ),
    ];
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
        !identity
    });
    spec.git_wrapper.bound = Some(crate::lane::BoundGit {
        worktree: input.cwd.to_owned(),
        env: bound,
    });
    // A single source generates every Git shim, including Claude's scrub shim.
    spec.git_wrapper.scrub = true;
    spec.write_git_wrapper(&scratch.join("bin"), input.provenant.git)?;
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

/// Let the lane read and run the helpers (`git-remote-https`) in the exec path
/// the launcher pins for the selected Git, wherever that host keeps them.
fn grant_exec_path(git: &Path, spec: &mut LaunchSpec) {
    if let Some(core) = crate::lane::host_exec_path(git) {
        spec.profile.spec.runtime_reads.push(subtree(&core));
        spec.profile.spec.tool_exec_roots.push(subtree(core));
    }
}

/// Resolve all exact executable requirements in the profile once, then use
/// those same facts both to add exact read grants and to report preparation.
pub(crate) fn prepare_tool_requirements(input: &PrepareInput<'_>, spec: &mut LaunchSpec) {
    spec.complete_tool_requirements(input.confined, input.home, input.cwd);
}

impl LaunchSpec {
    /// Resolve any additional exact executables added by launch configuration.
    pub fn complete_tool_requirements(&mut self, plan: &ConfinedPlan, home: &Path, cwd: &Path) {
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
                home,
                OmissionSource::Toolchain,
                &mut spec.omitted_discovery,
            );
            let required = &spec.tool_requirements.required_exec;
            for candidate in &spec.tool_requirements.exec {
                let mut one = vec![candidate.clone()];
                if !required.contains(candidate) {
                    filter_optional_grants(
                        &mut one,
                        0,
                        home,
                        OmissionSource::Toolchain,
                        &mut spec.omitted_discovery,
                    );
                }
                exec.extend(one);
            }
            for candidate in &spec.tool_requirements.writes {
                match std::fs::canonicalize(&candidate.path) {
                    Ok(path)
                        if provenant_confinement::refuses_lane_root(&path, false, Some(home)) =>
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
            spec.profile.spec.runtime_reads.extend(reads);
            spec.profile.spec.tool_exec_roots.extend(exec);
            spec.profile.spec.tool_writes.extend(writes);
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
                if provenant_confinement::refuses_lane_root(path, true, Some(home)) {
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
        let receipt = applied.receipt(Path::new("/home/fixture"));
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
/// scratch. Package-manager caches live here, never in shared host stores.
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

/// Rust tools for every shell-capable lane. The user's `~/.cargo` is read-only
/// (its `bin` proxies and the rustup toolchains stay executable); the host
/// later builds from its registry and Git checkouts unconfined, so no lane
/// may write them. Cargo gets a lane-private `CARGO_HOME` in the lane cache
/// and downloads crates into its own registry. A missing toolchain fails
/// clearly instead of installing. Build output stays in the lane's own cache.
/// The compiler hard-denies Cargo credentials.
pub(crate) fn rust_tools(
    input: &PrepareInput<'_>,
    access: Access,
    requirements: &mut ToolRequirements,
    env: &mut Vec<(OsString, OsString)>,
    setup: &mut Vec<SetupStep>,
) {
    // Subtree grants must anchor to existing directories. Keep this decision
    // here so every adapter gets the same system-tool fallback on non-Rust
    // hosts; the grants below it hold either way.
    if rust_tools_available(input.home) {
        rust_toolchain(
            input,
            access,
            &mut requirements.reads,
            &mut requirements.exec,
            env,
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
    env: &mut Vec<(OsString, OsString)>,
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
    runtime_reads.extend([subtree(&cargo), subtree(&rustup)]);
    tool_exec_roots.extend([
        subtree(cargo.join("bin")),
        subtree(rustup.join("toolchains")),
    ]);
    for name in ["cargo", "rustc", "rustfmt", "clippy-driver"] {
        add_existing_tool(&cargo.join("bin").join(name), tool_exec_roots);
    }
    let cargo_home = cache.join("cargo-home");
    if access == Access::ReadOnly {
        // Offline builds from the already-extracted shared sources. The
        // links point at stores the lane cannot write, so it cannot alter
        // them; Cargo's lock files and downloads need writes, so it cannot
        // fetch either.
        setup.push(SetupStep::CreateDir(cargo_home.clone()));
        setup.extend(["registry", "git"].map(|name| SetupStep::Symlink {
            link: cargo_home.join(name),
            target: cargo.join(name),
        }));
    }
    env.extend([
        ("CARGO_HOME".into(), cargo_home.into_os_string()),
        ("RUSTUP_HOME".into(), rustup.into_os_string()),
        ("RUSTUP_AUTO_INSTALL".into(), "0".into()),
        ("CARGO_TARGET_DIR".into(), target.clone().into_os_string()),
        // Override build.build-dir inherited from host or ancestor Cargo config.
        (
            "CARGO_BUILD_BUILD_DIR".into(),
            target.clone().into_os_string(),
        ),
    ]);
    if access == Access::WorktreeWrite {
        // Lint-only runs set both CARGO_TARGET_DIR and CARGO_BUILD_BUILD_DIR to
        // this sibling. Cargo locks the build directory, so they then do not
        // queue behind test and build runs in the lane's own target/.
        env.push((
            "PROVENANT_LANE_CARGO_LINT_TARGET".into(),
            cache.join("target-lint").into_os_string(),
        ));
    }
    if access == Access::ReadOnly {
        // The lane cannot fetch, so Cargo fails fast offline, and its cache
        // clean-up never touches the shared stores behind the links.
        env.extend([
            ("CARGO_NET_OFFLINE".into(), "true".into()),
            ("CARGO_CACHE_AUTO_CLEAN_FREQUENCY".into(), "never".into()),
        ]);
        setup.push(SetupStep::CreateDir(target));
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
    for prefix in ["/opt/homebrew", "/usr/local"].map(Path::new) {
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
pub(crate) fn package_grant(executable: &Path, omitted: &mut Vec<OmittedGrant>) -> PathGrant {
    match package_root(executable) {
        Ok(root) => subtree(root),
        Err(_) => {
            omitted.push(OmittedGrant {
                path: package_candidate(executable)
                    .unwrap_or(executable)
                    .to_owned(),
                source: OmissionSource::ProviderPackage,
            });
            exact(executable)
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
