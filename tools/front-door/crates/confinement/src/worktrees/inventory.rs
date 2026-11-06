//! Git worktree inventory parsing and Git invocations.
use super::*;

/// Command-scoped repository observations. A GC preview consumes its inventory
/// snapshot; mutation paths always perform a fresh read while holding custody.
pub struct RepoFacts {
    inventory_limit: usize,
    scalar_limit: usize,
    inventories: std::sync::Mutex<BTreeMap<PathBuf, InventorySnapshot>>,
}

struct InventorySnapshot {
    device: u64,
    inode: u64,
    budget: UtilityBudget,
    entries: Vec<GitWorktree>,
}

impl Default for RepoFacts {
    fn default() -> Self {
        Self {
            inventory_limit: provenant_os::exec::DEFAULT_CAPTURE_LIMIT,
            scalar_limit: provenant_os::git::REPO_FACT_SCALAR_LIMIT,
            inventories: std::sync::Mutex::new(BTreeMap::new()),
        }
    }
}

impl RepoFacts {
    /// Override output caps without widening the caller's Git budget.
    pub fn with_limits(inventory_limit: usize, scalar_limit: usize) -> Result<Self, WorktreeError> {
        let budget = UtilityBudget::bounded(std::time::Duration::from_secs(1))?;
        budget.with_stdout_cap(inventory_limit)?;
        budget.with_stdout_cap(scalar_limit)?;
        Ok(Self {
            inventory_limit,
            scalar_limit,
            ..Self::default()
        })
    }

    /// Resolve a commit ref from fresh Git evidence. Ref values can move even
    /// within a command, so this read is never served from an earlier epoch.
    pub fn resolve_commit(
        &self,
        context: &WorktreeContext<'_>,
        name: GitRefName,
        cancel: &CancelToken,
    ) -> Result<GitOid, WorktreeError> {
        validate_binding(context)?;
        self.resolve_repo_commit(
            context.repo,
            context.executor,
            context.utility,
            context.utility_budget,
            name,
            cancel,
        )
    }

    fn resolve_repo_commit(
        &self,
        repo: &VerifiedRepo,
        executor: &dyn Executor,
        utility: &UtilityRuntime,
        budget: UtilityBudget,
        name: GitRefName,
        cancel: &CancelToken,
    ) -> Result<GitOid, WorktreeError> {
        repo.revalidate()?;
        let budget = budget.with_stdout_cap(self.scalar_limit)?;
        let outcome = run_utility(
            executor,
            GitRead::ResolveRef { name },
            repo,
            utility,
            budget,
            cancel,
        )?;
        if exit_code(&outcome)? != 0 {
            return Err(WorktreeError::NotFound);
        }
        parse_oid(trim_ascii(&outcome.stdout.bytes))
    }

    /// Resolve a commit through a read-only repository, including native
    /// alternates, with the same session cap as protected repository reads.
    pub fn resolve_read_commit(
        &self,
        repo: &crate::repo::ReadOnlyRepo,
        executor: &dyn Executor,
        utility: &UtilityRuntime,
        budget: UtilityBudget,
        name: GitRefName,
        cancel: &CancelToken,
    ) -> Result<GitOid, WorktreeError> {
        let budget = budget.with_stdout_cap(self.scalar_limit)?;
        let outcome = crate::utility::run_read_utility(
            executor,
            GitRead::ResolveRef { name },
            repo,
            utility,
            budget,
            cancel,
        )?;
        if exit_code(&outcome)? != 0 {
            return Err(WorktreeError::NotFound);
        }
        parse_oid(trim_ascii(&outcome.stdout.bytes))
    }

    /// Resolve a read-only ref without peeling it, preserving Git's distinction
    /// between an absent ref and a ref to a non-commit object.
    pub fn resolve_read_ref(
        &self,
        repo: &crate::repo::ReadOnlyRepo,
        executor: &dyn Executor,
        utility: &UtilityRuntime,
        budget: UtilityBudget,
        name: GitRefName,
        cancel: &CancelToken,
    ) -> Result<GitOid, WorktreeError> {
        let budget = budget.with_stdout_cap(self.scalar_limit)?;
        let outcome = crate::utility::run_read_utility(
            executor,
            GitRead::ResolveRefOid { name },
            repo,
            utility,
            budget,
            cancel,
        )?;
        if exit_code(&outcome)? != 0 {
            return Err(WorktreeError::NotFound);
        }
        parse_oid(trim_ascii(&outcome.stdout.bytes))
    }

    /// Resolve a commit's tree through the read-only facts boundary.
    pub fn resolve_read_tree(
        &self,
        repo: &crate::repo::ReadOnlyRepo,
        executor: &dyn Executor,
        utility: &UtilityRuntime,
        budget: UtilityBudget,
        commit: GitOid,
        cancel: &CancelToken,
    ) -> Result<GitOid, WorktreeError> {
        let budget = budget.with_stdout_cap(self.scalar_limit)?;
        let outcome = crate::utility::run_read_utility(
            executor,
            GitRead::ResolveCommitTree { oid: commit },
            repo,
            utility,
            budget,
            cancel,
        )?;
        if exit_code(&outcome)? != 0 {
            return Err(WorktreeError::NotFound);
        }
        parse_oid(trim_ascii(&outcome.stdout.bytes))
    }

    /// Resolve a revision from fresh Git evidence using this session's scalar cap.
    pub fn resolve_revision(
        &self,
        context: &WorktreeContext<'_>,
        revision: GitRevision,
        cancel: &CancelToken,
    ) -> Result<GitOid, WorktreeError> {
        validate_binding(context)?;
        context.repo.revalidate()?;
        let budget = context.utility_budget.with_stdout_cap(self.scalar_limit)?;
        let outcome = run_utility(
            context.executor,
            GitRead::ResolveRevision { revision },
            context.repo,
            context.utility,
            budget,
            cancel,
        )?;
        if exit_code(&outcome)? != 0 {
            return Err(WorktreeError::NotFound);
        }
        parse_oid(trim_ascii(&outcome.stdout.bytes))
    }

    /// Read the current commit of a verified checkout in this repository.
    pub(super) fn checkout_head(
        &self,
        context: &WorktreeContext<'_>,
        repo: &VerifiedRepo,
        cancel: &CancelToken,
    ) -> Result<GitOid, WorktreeError> {
        validate_binding(context)?;
        context.repo.revalidate()?;
        repo.revalidate()?;
        if repo.common_git_dir() != context.repo.common_git_dir() {
            return Err(WorktreeError::IdentityChanged);
        }
        let budget = context.utility_budget.with_stdout_cap(self.scalar_limit)?;
        let outcome = run_utility(
            context.executor,
            GitRead::RevParseHead,
            repo,
            context.utility,
            budget,
            cancel,
        )?;
        if exit_code(&outcome)? != 0 {
            return Err(WorktreeError::NotFound);
        }
        parse_oid(trim_ascii(&outcome.stdout.bytes))
    }

    /// Resolve the full symbolic name of a revision, if Git can identify one.
    pub fn symbolic_revision(
        &self,
        context: &WorktreeContext<'_>,
        revision: GitRevision,
        cancel: &CancelToken,
    ) -> Result<Option<String>, WorktreeError> {
        validate_binding(context)?;
        context.repo.revalidate()?;
        let budget = context.utility_budget.with_stdout_cap(self.scalar_limit)?;
        let outcome = run_utility(
            context.executor,
            GitRead::SymbolicRevision { revision },
            context.repo,
            context.utility,
            budget,
            cancel,
        )?;
        if exit_code(&outcome)? != 0 {
            return Ok(None);
        }
        let name = std::str::from_utf8(trim_ascii(&outcome.stdout.bytes))
            .map_err(|_| WorktreeError::InvalidOutput)?;
        Ok(Some(name.to_owned()))
    }

    /// Resolve the trusted local bare setting with the session's scalar cap.
    pub fn bare_setting(
        &self,
        repo: &VerifiedRepo,
        executor: &dyn Executor,
        utility: &UtilityRuntime,
        budget: UtilityBudget,
        cancel: &CancelToken,
    ) -> Result<Option<bool>, WorktreeError> {
        let scalar = budget.with_stdout_cap(self.scalar_limit)?;
        let bare = run_utility(
            executor,
            GitRead::LocalBareOrigins,
            repo,
            utility,
            scalar,
            cancel,
        )?;
        match exit_code(&bare)? {
            0 => {
                let roots = utility.lane_roots(repo);
                let lane_files = crate::utility::lane_configuration_files(
                    executor, repo, utility, budget, cancel,
                )?;
                if !bare.stdout.bytes.ends_with(&[0]) {
                    return Err(WorktreeError::InvalidOutput);
                }
                let mut fields = bare.stdout.bytes.split(|byte| *byte == 0);
                let mut selected = None;
                let mut untrusted_bare = false;
                while let Some(origin) = fields.next() {
                    if origin.is_empty() && fields.clone().next().is_none() {
                        break;
                    }
                    let path = origin
                        .strip_prefix(b"file:")
                        .ok_or(WorktreeError::InvalidOutput)?;
                    if path.is_empty() {
                        return Err(WorktreeError::InvalidOutput);
                    }
                    let path = PathBuf::from(OsString::from_vec(path.to_vec()));
                    let path = if path.is_absolute() {
                        path
                    } else {
                        repo.root_path().join(path)
                    };
                    let in_lane = roots.contains(&path);
                    let path = crate::transport::normalise(&path);
                    let value = fields.next().ok_or(WorktreeError::InvalidOutput)?;
                    let value = match value {
                        b"true" => true,
                        b"false" => false,
                        _ => return Err(WorktreeError::InvalidOutput),
                    };
                    if !in_lane && !lane_files.contains(&path) {
                        selected = Some(value);
                    } else {
                        untrusted_bare |= value;
                    }
                }
                // If a bare setting appears only through lane-controlled
                // provenance, the primary worktree cannot be classified
                // safely: an alias may hide a trusted occurrence of that file.
                if selected != Some(true) && untrusted_bare {
                    return Err(WorktreeError::InvalidOutput);
                }
                Ok(selected)
            }
            1 => Ok(None),
            _ => Err(WorktreeError::InvalidOutput),
        }
    }

    /// Read raw worktree registrations from a read-only checkout. The common
    /// repository's local bare setting controls Git's primary registration;
    /// the utility's normal override otherwise changes that fact.
    pub fn read_only_inventory(
        &self,
        repo: &crate::repo::ReadOnlyRepo,
        executor: &dyn Executor,
        utility: &UtilityRuntime,
        budget: UtilityBudget,
        cancel: &CancelToken,
    ) -> Result<Vec<u8>, WorktreeError> {
        let bare = self
            .bare_setting(&repo.0, executor, utility, budget, cancel)?
            .unwrap_or(false);
        let budget = budget.with_stdout_cap(self.inventory_limit)?;
        let operation = if bare {
            GitRead::WorktreeListBare
        } else {
            GitRead::WorktreeList
        };
        let outcome =
            crate::utility::run_read_utility(executor, operation, repo, utility, budget, cancel)?;
        if exit_code(&outcome)? != 0 {
            return Err(WorktreeError::InvalidOutput);
        }
        Ok(outcome.stdout.bytes)
    }

    /// Discover registrations from a working directory, including a bare
    /// repository or a subdirectory that has not been opened as a checkout.
    /// The caller supplies its already-sanitised Git environment.
    pub fn discover_worktree_paths(
        &self,
        cwd: &Path,
        git: &Path,
        environment: &[(OsString, OsString)],
        executor: &dyn Executor,
        timeout: std::time::Duration,
        cancel: &CancelToken,
    ) -> Result<Vec<PathBuf>, WorktreeError> {
        use provenant_os::exec::{ExecOptions, StreamPolicy};
        let argv = [
            git.as_os_str().to_owned(),
            "-c".into(),
            "core.fsmonitor=false".into(),
            "worktree".into(),
            "list".into(),
            "--porcelain".into(),
            "-z".into(),
        ];
        let mut options = ExecOptions::new(&argv, cwd, timeout);
        options.env = Some(environment);
        options.stdin = Some(b"");
        options.cancel = Some(cancel);
        options.stdout = StreamPolicy::Capture {
            limit: self.inventory_limit,
        };
        options.stderr = StreamPolicy::Capture { limit: 4096 };
        options.stop_on_output_limit = true;
        let output = executor.execute(options).map_err(UtilityError::from)?;
        if output.stdout.truncated {
            return Err(UtilityError::Truncated { stream: "stdout" }.into());
        }
        if output.cancelled {
            return Err(UtilityError::Cancelled.into());
        }
        if output.timed_out {
            return Err(UtilityError::TimedOut.into());
        }
        if output.drain_truncated || output.stdout.read_failed || output.stdout.sink_failed {
            return Err(UtilityError::Incomplete.into());
        }
        if output.termination != Termination::Exit(0) {
            return Err(WorktreeError::InvalidOutput);
        }
        inventory_paths(&output.stdout.bytes)
    }

    /// Read registered checkout paths without interpreting Git's quoted text
    /// format. Bare primary registrations are included, as in Git's inventory.
    pub fn read_only_worktree_paths(
        &self,
        repo: &crate::repo::ReadOnlyRepo,
        executor: &dyn Executor,
        utility: &UtilityRuntime,
        budget: UtilityBudget,
        cancel: &CancelToken,
    ) -> Result<Vec<PathBuf>, WorktreeError> {
        let inventory = self.read_only_inventory(repo, executor, utility, budget, cancel)?;
        inventory_paths(&inventory)
    }

    /// Read bounded worktree registrations for writer membership verification.
    pub fn writer_inventory(
        &self,
        repo: &VerifiedRepo,
        executor: &dyn Executor,
        utility: &UtilityRuntime,
        budget: UtilityBudget,
        cancel: &CancelToken,
    ) -> Result<Vec<u8>, WorktreeError> {
        let outcome = run_utility(
            executor,
            GitRead::WorktreeList,
            repo,
            utility,
            budget.with_stdout_cap(self.inventory_limit)?,
            cancel,
        )?;
        if exit_code(&outcome)? != 0 {
            return Err(WorktreeError::InvalidOutput);
        }
        Ok(outcome.stdout.bytes)
    }

    /// Read inventory afresh and invalidate any older advisory snapshot.
    pub(super) fn read_inventory(
        &self,
        context: &WorktreeContext<'_>,
        cancel: &CancelToken,
    ) -> Result<Vec<GitWorktree>, WorktreeError> {
        let _ = self.take_inventory(context);
        let bytes = self.writer_inventory(
            context.repo,
            context.executor,
            context.utility,
            context.utility_budget,
            cancel,
        )?;
        parse_worktree_list(&bytes)
    }

    pub(super) fn capture_inventory(
        &self,
        context: &WorktreeContext<'_>,
        cancel: &CancelToken,
    ) -> Result<Vec<GitWorktree>, WorktreeError> {
        let budget = context
            .utility_budget
            .with_stdout_cap(self.inventory_limit)?;
        let entries = self.read_inventory(context, cancel)?;
        self.remember_inventory(context, &entries, budget);
        Ok(entries)
    }

    fn remember_inventory(
        &self,
        context: &WorktreeContext<'_>,
        entries: &[GitWorktree],
        budget: UtilityBudget,
    ) {
        let path = context.repo.common_git_dir();
        let mut inventories = self
            .inventories
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        inventories.remove(path);
        if let Ok(metadata) = std::fs::metadata(path) {
            inventories.insert(
                path.to_path_buf(),
                InventorySnapshot {
                    device: metadata.dev(),
                    inode: metadata.ino(),
                    budget,
                    entries: entries.to_vec(),
                },
            );
        }
    }

    /// Reuse an advisory inventory while the repository identity is unchanged.
    /// Mutation paths invalidate it and perform their own guarded rereads.
    pub(super) fn cached_inventory(
        &self,
        context: &WorktreeContext<'_>,
    ) -> Option<Vec<GitWorktree>> {
        let inventories = self
            .inventories
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let snapshot = inventories.get(context.repo.common_git_dir())?;
        let metadata = std::fs::metadata(context.repo.common_git_dir()).ok()?;
        let budget = context
            .utility_budget
            .with_stdout_cap(self.inventory_limit)
            .ok()?;
        (snapshot.device == metadata.dev()
            && snapshot.inode == metadata.ino()
            && snapshot.budget == budget)
            .then(|| snapshot.entries.clone())
    }

    /// Discard an advisory inventory before a command-controlled Git mutation.
    pub fn invalidate_inventory(&self, context: &WorktreeContext<'_>) {
        let _ = self.take_inventory(context);
    }

    pub(super) fn take_inventory(&self, context: &WorktreeContext<'_>) -> Option<Vec<GitWorktree>> {
        let snapshot = self
            .inventories
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(context.repo.common_git_dir())?;
        let metadata = std::fs::metadata(context.repo.common_git_dir()).ok()?;
        let budget = context
            .utility_budget
            .with_stdout_cap(self.inventory_limit)
            .ok()?;
        (snapshot.device == metadata.dev()
            && snapshot.inode == metadata.ino()
            && snapshot.budget == budget)
            .then_some(snapshot.entries)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitWorktree {
    pub(crate) path: PathBuf,
    pub(crate) head: GitOid,
    pub(crate) branch: Option<GitRefName>,
    pub(super) locked: bool,
    pub(super) prunable: bool,
    pub(crate) bare: bool,
}

fn inventory_paths(bytes: &[u8]) -> Result<Vec<PathBuf>, WorktreeError> {
    if !bytes.ends_with(b"\0\0") {
        return Err(WorktreeError::InvalidOutput);
    }
    let mut paths = Vec::new();
    let mut at_record_start = true;
    for field in bytes.split(|byte| *byte == 0) {
        if field.is_empty() {
            at_record_start = true;
        } else if at_record_start {
            let path = field
                .strip_prefix(b"worktree ")
                .filter(|path| path.first() == Some(&b'/'))
                .ok_or(WorktreeError::InvalidOutput)?;
            paths.push(PathBuf::from(OsString::from_vec(path.to_vec())));
            if paths.len() > MAX_WORKTREES {
                return Err(WorktreeError::InvalidOutput);
            }
            at_record_start = false;
        }
    }
    if paths.is_empty() {
        return Err(WorktreeError::InvalidOutput);
    }
    Ok(paths)
}

pub(crate) fn parse_worktree_list(bytes: &[u8]) -> Result<Vec<GitWorktree>, WorktreeError> {
    let mut result = Vec::new();
    let mut fields: Vec<Vec<u8>> = Vec::new();
    for field in bytes.split(|byte| *byte == 0) {
        if field.is_empty() {
            if !fields.is_empty() {
                result.push(parse_worktree_record(&fields)?);
                fields.clear();
                if result.len() > MAX_WORKTREES {
                    return Err(WorktreeError::InvalidOutput);
                }
            }
        } else {
            fields.push(field.to_vec());
        }
    }
    if !fields.is_empty() {
        result.push(parse_worktree_record(&fields)?);
    }
    if result.len() > MAX_WORKTREES {
        return Err(WorktreeError::InvalidOutput);
    }
    Ok(result)
}

pub(super) fn parse_worktree_record(fields: &[Vec<u8>]) -> Result<GitWorktree, WorktreeError> {
    let mut path = None;
    let mut head = None;
    let mut branch = None;
    let mut detached = false;
    let mut locked = false;
    let mut prunable = false;
    let mut bare = false;
    for field in fields {
        if let Some(value) = field.strip_prefix(b"worktree ") {
            if path.is_some() || value.is_empty() || value.first() != Some(&b'/') {
                return Err(WorktreeError::InvalidOutput);
            }
            path = Some(PathBuf::from(OsString::from_vec(value.to_vec())));
        } else if let Some(value) = field.strip_prefix(b"HEAD ") {
            if head.is_some() {
                return Err(WorktreeError::InvalidOutput);
            }
            head = Some(parse_oid(value)?);
        } else if let Some(value) = field.strip_prefix(b"branch ") {
            if branch.is_some() || detached || bare {
                return Err(WorktreeError::InvalidOutput);
            }
            let text = std::str::from_utf8(value).map_err(|_| WorktreeError::InvalidOutput)?;
            branch = Some(GitRefName::new(text).map_err(|_| WorktreeError::InvalidOutput)?);
        } else if field.as_slice() == b"detached" {
            if detached || branch.is_some() {
                return Err(WorktreeError::InvalidOutput);
            }
            detached = true;
        } else if field.as_slice() == b"bare" {
            if bare {
                return Err(WorktreeError::InvalidOutput);
            }
            bare = true;
        } else if field.as_slice() == b"locked" || field.starts_with(b"locked ") {
            if locked {
                return Err(WorktreeError::InvalidOutput);
            }
            locked = true;
        } else if field.as_slice() == b"prunable" || field.starts_with(b"prunable ") {
            if prunable {
                return Err(WorktreeError::InvalidOutput);
            }
            prunable = true;
        } else {
            return Err(WorktreeError::InvalidOutput);
        }
    }
    let path = path.ok_or(WorktreeError::InvalidOutput)?;
    let head = head.ok_or(WorktreeError::InvalidOutput)?;
    if !detached && !bare && branch.is_none() {
        return Err(WorktreeError::InvalidOutput);
    }
    if bare && branch.is_some() {
        return Err(WorktreeError::InvalidOutput);
    }
    Ok(GitWorktree {
        path,
        head,
        branch,
        locked,
        prunable,
        bare,
    })
}

pub(super) fn parse_oid(bytes: &[u8]) -> Result<GitOid, WorktreeError> {
    let text = std::str::from_utf8(bytes).map_err(|_| WorktreeError::InvalidOutput)?;
    GitOid::parse(text).map_err(|_| WorktreeError::InvalidOutput)
}

pub(super) fn parse_branch_tips(
    bytes: &[u8],
) -> Result<BTreeMap<GitRefName, GitOid>, WorktreeError> {
    let mut fields = bytes.split(|byte| *byte == 0);
    let mut result = BTreeMap::new();
    while let Some(name) = fields.next() {
        if trim_ascii(name).is_empty() {
            continue;
        }
        let oid = fields.next().ok_or(WorktreeError::InvalidOutput)?;
        let name =
            std::str::from_utf8(trim_ascii(name)).map_err(|_| WorktreeError::InvalidOutput)?;
        let name = if let Some(remote) = name.strip_prefix("refs/remotes/") {
            GitRefName::remote_tracking(remote)
        } else {
            GitRefName::new(name)
        }
        .map_err(|_| WorktreeError::InvalidOutput)?;
        let oid = parse_oid(trim_ascii(oid))?;
        if result.insert(name, oid).is_some() || result.len() > MAX_BRANCHES {
            return Err(WorktreeError::InvalidOutput);
        }
    }
    Ok(result)
}

pub(super) fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    bytes.get(start..end).unwrap_or_default()
}

pub(super) fn exit_code(outcome: &ExecOutcome) -> Result<i64, WorktreeError> {
    match outcome.termination {
        Termination::Exit(code) => Ok(code),
        Termination::Signal(_) => Err(WorktreeError::InvalidOutput),
    }
}

pub(super) fn run_git(
    context: &WorktreeContext<'_>,
    repo: &VerifiedRepo,
    read: GitRead,
    cancel: &CancelToken,
) -> Result<ExecOutcome, WorktreeError> {
    run_utility(
        context.executor,
        read,
        repo,
        context.utility,
        context.utility_budget,
        cancel,
    )
    .map_err(WorktreeError::from)
}

pub(super) fn read_branch_tips(
    context: &WorktreeContext<'_>,
    cancel: &CancelToken,
) -> Result<BTreeMap<GitRefName, GitOid>, WorktreeError> {
    let outcome = run_git(context, context.repo, GitRead::BranchTips, cancel)?;
    if exit_code(&outcome)? != 0 {
        return Err(WorktreeError::InvalidOutput);
    }
    parse_branch_tips(&outcome.stdout.bytes)
}

pub fn is_ancestor(
    context: &WorktreeContext<'_>,
    ancestor: GitOid,
    tip: GitOid,
    cancel: &CancelToken,
) -> Result<bool, WorktreeError> {
    let outcome = run_git(
        context,
        context.repo,
        GitRead::MergeBaseIsAncestor { ancestor, tip },
        cancel,
    )?;
    match exit_code(&outcome)? {
        0 => Ok(true),
        1 => Ok(false),
        _ => Err(WorktreeError::InvalidOutput),
    }
}

pub(super) fn is_first_parent_commit(
    context: &WorktreeContext<'_>,
    history_tip: GitOid,
    candidate: GitOid,
    cancel: &CancelToken,
) -> Result<bool, WorktreeError> {
    let outcome = run_git(
        context,
        context.repo,
        GitRead::FirstParentHistory { tip: history_tip },
        cancel,
    )?;
    if exit_code(&outcome)? != 0 || !outcome.stdout.bytes.ends_with(b"\n") {
        return Err(WorktreeError::InvalidOutput);
    }
    let history = outcome
        .stdout
        .bytes
        .strip_suffix(b"\n")
        .ok_or(WorktreeError::InvalidOutput)?;
    if history.is_empty() {
        return Err(WorktreeError::InvalidOutput);
    }
    let mut found = false;
    for line in history.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            return Err(WorktreeError::InvalidOutput);
        }
        found |= parse_oid(line)? == candidate;
    }
    Ok(found)
}

pub(super) fn canonical_worktree_paths(
    entries: &[GitWorktree],
) -> Result<Vec<(PathBuf, &GitWorktree)>, WorktreeError> {
    entries
        .iter()
        // A prunable registration names a path that Git has already observed
        // as missing. It has no canonical filesystem path to protect here.
        .filter(|entry| !entry.bare && !entry.prunable)
        .map(|entry| Ok((canonicalize(&entry.path)?, entry)))
        .collect()
}

pub(super) fn path_digest(path: &Path) -> String {
    let digest = Sha256::digest(path.as_os_str().as_bytes());
    hex(&digest)
}

/// The exclusion slot name for the canonical worktree `path`, below
/// `<state>/worktrees/<project>/`.
pub fn state_dir_name(path: &Path) -> OsString {
    path_digest(path).into()
}

pub(super) fn read_bounded(
    directory: &Directory,
    name: &OsStr,
    limit: u64,
) -> Result<Vec<u8>, FsError> {
    crate::stable_read::read_regular(directory, name, limit)
        .map(|(bytes, _)| bytes)
        .map_err(|error| match error {
            crate::stable_read::ReadError::Unsupported => FsError::NotRegular { name: name.into() },
            crate::stable_read::ReadError::TooLarge => FsError::TooLarge { limit },
            crate::stable_read::ReadError::Changed => FsError::RaceRefused { name: name.into() },
            crate::stable_read::ReadError::Filesystem(error) => error,
            crate::stable_read::ReadError::Io(source) => FsError::Io {
                operation: "read worktree state",
                path: name.into(),
                source,
            },
        })
}

#[cfg(test)]
mod path_tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn inventory_paths_preserve_unquoted_native_paths_and_bare_records() {
        let paths =
            inventory_paths(b"worktree /bare\0bare\0\0worktree /lane \xff\nname\0HEAD 123\0\0")
                .expect("valid inventory");
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], PathBuf::from("/bare"));
        assert_eq!(paths[1].as_os_str().as_bytes(), b"/lane \xff\nname");
        assert!(inventory_paths(b"worktree /truncated\0").is_err());
        assert!(inventory_paths(b"worktree relative\0bare\0\0").is_err());
        assert!(inventory_paths(b"\0\0").is_err());
    }
}
