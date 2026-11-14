//! Git worktree inventory parsing and Git invocations.
use super::*;

/// Command-scoped repository observations. A GC preview consumes its inventory
/// snapshot; mutation paths always perform a fresh read while holding custody.
#[derive(Clone)]
pub struct RepoFacts {
    inventory_limit: usize,
    scalar_limit: usize,
    raw_inventories: std::sync::Arc<std::sync::Mutex<BTreeMap<PathBuf, RawInventorySnapshot>>>,
    inventories: std::sync::Arc<std::sync::Mutex<BTreeMap<PathBuf, InventorySnapshot>>>,
    worktree_listings: std::sync::Arc<std::sync::Mutex<BTreeMap<PathBuf, Vec<u8>>>>,
}

#[derive(PartialEq, Eq)]
struct RegistrationStamp {
    name: OsString,
    device: u64,
    inode: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

// Git may add, remove or lock a registration outside this command. Such a
// change starts a new inventory epoch without spending a Git subprocess on
// every admission that shares the unchanged registrations.
fn registration_stamps(common: &Path) -> Option<Vec<RegistrationStamp>> {
    fn stamp(common: &Path, path: &Path, stamps: &mut Vec<RegistrationStamp>) -> Option<()> {
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) if !metadata.file_type().is_symlink() => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            _ => return None,
        };
        stamps.push(RegistrationStamp {
            name: path.strip_prefix(common).ok()?.as_os_str().to_owned(),
            device: metadata.as_ref().map_or(0, |meta| meta.dev()),
            inode: metadata.as_ref().map_or(0, |meta| meta.ino()),
            modified_seconds: metadata.as_ref().map_or(0, |meta| meta.mtime()),
            modified_nanoseconds: metadata.as_ref().map_or(0, |meta| meta.mtime_nsec()),
        });
        Some(())
    }
    fn head(common: &Path, admin: &Path, stamps: &mut Vec<RegistrationStamp>) -> Option<()> {
        use std::io::Read;
        let path = admin.join("HEAD");
        stamp(common, &path, stamps)?;
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .ok()?
            .take(1025)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > 1024 {
            return None;
        }
        if let Some(reference) = trim_ascii(&bytes).strip_prefix(b"ref: ") {
            let reference = GitRefName::new(std::str::from_utf8(reference).ok()?).ok()?;
            stamp(common, &common.join(reference.as_str()), stamps)?;
        }
        Some(())
    }
    let mut stamps = Vec::new();
    head(common, common, &mut stamps)?;
    stamp(common, &common.join("packed-refs"), &mut stamps)?;
    let entries = match std::fs::read_dir(common.join("worktrees")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(stamps),
        Err(_) => return None,
    };
    for (count, entry) in entries.enumerate() {
        let entry = entry.ok()?;
        if count >= MAX_WORKTREES {
            return None;
        }
        let metadata = std::fs::symlink_metadata(entry.path()).ok()?;
        if !metadata.is_dir() {
            return None;
        }
        stamp(common, &entry.path(), &mut stamps)?;
        head(common, &entry.path(), &mut stamps)?;
        // A registration's lock and pruning markers affect admission even
        // when its branch and commit have not moved.
        for name in ["locked", "prunable", "gitdir", "commondir"] {
            stamp(common, &entry.path().join(name), &mut stamps)?;
        }
    }
    stamps.sort_by(|left, right| left.name.cmp(&right.name));
    Some(stamps)
}

struct RawInventorySnapshot {
    registrations: Option<Vec<RegistrationStamp>>,
    device: u64,
    inode: u64,
    budget: UtilityBudget,
    bytes: Vec<u8>,
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
            raw_inventories: Default::default(),
            inventories: std::sync::Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            worktree_listings: Default::default(),
        }
    }
}

impl RepoFacts {
    /// Effective stdout cap for repository worktree inventory facts.
    pub fn inventory_limit(&self) -> usize {
        self.inventory_limit
    }

    /// Run a read-only Git resolver through this session's fact caps. Retain
    /// incomplete fact evidence even if the resolver maps its subprocess error.
    pub fn with_executor<T, E: From<WorktreeError>>(
        &self,
        executor: &dyn Executor,
        cancel: &CancelToken,
        action: impl FnOnce(&dyn Executor) -> Result<T, E>,
    ) -> Result<T, E> {
        let scoped = FactExecutor {
            executor,
            facts: self,
            cancel,
            failures: std::sync::atomic::AtomicU8::new(0),
        };
        let result = action(&scoped);
        let flags = scoped.failures.load(std::sync::atomic::Ordering::Relaxed);
        let error = if cancel.is_cancelled() || flags & 1 != 0 {
            Some(UtilityError::Cancelled)
        } else if flags & 2 != 0 {
            Some(UtilityError::TimedOut)
        } else if flags & 4 != 0 {
            Some(UtilityError::Truncated { stream: "stdout" })
        } else if flags & 8 != 0 {
            Some(UtilityError::Incomplete)
        } else {
            None
        };
        match error {
            Some(error) => Err(E::from(WorktreeError::from(error))),
            None => result,
        }
    }

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

    /// Reuse a successful worktree listing for the lifetime of this command's
    /// repository facts session. Callers that mutate registrations must use
    /// their guarded fresh-inventory path instead.
    pub fn cached_worktree_listing<E>(
        &self,
        cwd: &Path,
        read: impl FnOnce() -> Result<Vec<u8>, E>,
    ) -> Result<Vec<u8>, E> {
        let key = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
        let mut listings = self
            .worktree_listings
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(listing) = listings.get(&key) {
            return Ok(listing.clone());
        }
        let listing = read()?;
        listings.insert(key, listing.clone());
        Ok(listing)
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

    /// Resolve a commit in a verified repository with this session's scalar cap.
    pub fn resolve_repo_commit(
        &self,
        repo: &VerifiedRepo,
        executor: &dyn Executor,
        utility: &UtilityRuntime,
        budget: UtilityBudget,
        name: GitRefName,
        cancel: &CancelToken,
    ) -> Result<GitOid, WorktreeError> {
        self.resolve_repo_fact(
            repo,
            executor,
            utility,
            budget,
            GitRead::ResolveRef { name },
            cancel,
        )
    }

    /// Read the current checkout commit with this session's scalar cap.
    pub fn repo_head(
        &self,
        repo: &VerifiedRepo,
        executor: &dyn Executor,
        utility: &UtilityRuntime,
        budget: UtilityBudget,
        cancel: &CancelToken,
    ) -> Result<GitOid, WorktreeError> {
        self.resolve_repo_fact(
            repo,
            executor,
            utility,
            budget,
            GitRead::RevParseHead,
            cancel,
        )
    }

    fn resolve_repo_fact(
        &self,
        repo: &VerifiedRepo,
        executor: &dyn Executor,
        utility: &UtilityRuntime,
        budget: UtilityBudget,
        operation: GitRead,
        cancel: &CancelToken,
    ) -> Result<GitOid, WorktreeError> {
        repo.revalidate()?;
        let budget = budget.with_stdout_cap(self.scalar_limit)?;
        let outcome = run_utility(executor, operation, repo, utility, budget, cancel)?;
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
        self.repo_head(
            repo,
            context.executor,
            context.utility,
            context.utility_budget,
            cancel,
        )
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
                let roots = utility.lane_roots(repo)?;
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
        repo.revalidate()?;
        if cancel.is_cancelled() {
            return Err(UtilityError::Cancelled.into());
        }
        let budget = budget.with_stdout_cap(self.inventory_limit)?;
        let path = repo.common_git_dir();
        let metadata = std::fs::metadata(path).map_err(|_| WorktreeError::IdentityChanged)?;
        let registrations = registration_stamps(path);
        let mut snapshots = self
            .raw_inventories
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(snapshot) = snapshots.get(path)
            && snapshot.device == metadata.dev()
            && snapshot.inode == metadata.ino()
            && snapshot.budget == budget
            && registrations.is_some()
            && snapshot.registrations == registrations
        {
            return Ok(snapshot.bytes.clone());
        }
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
        snapshots.insert(
            path.to_owned(),
            RawInventorySnapshot {
                registrations,
                device: metadata.dev(),
                inode: metadata.ino(),
                budget,
                bytes: outcome.stdout.bytes.clone(),
            },
        );
        Ok(outcome.stdout.bytes)
    }

    /// Refresh inventory for mutation checks, discarding the command snapshot.
    pub(super) fn fresh_inventory(
        &self,
        context: &WorktreeContext<'_>,
        cancel: &CancelToken,
    ) -> Result<Vec<GitWorktree>, WorktreeError> {
        self.invalidate_inventory(context);
        let result = self.read_inventory(context, cancel);
        self.invalidate_inventory(context);
        result
    }

    /// Read this command's inventory and invalidate an older parsed advisory snapshot.
    pub(super) fn read_inventory(
        &self,
        context: &WorktreeContext<'_>,
        cancel: &CancelToken,
    ) -> Result<Vec<GitWorktree>, WorktreeError> {
        self.inventories
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(context.repo.common_git_dir());
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
        self.raw_inventories
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(context.repo.common_git_dir());
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

/// Adapter for read-only resolvers whose repository layouts cannot grant
/// protected worktree mutation authority (for example native alternates).
struct FactExecutor<'a> {
    executor: &'a dyn Executor,
    facts: &'a RepoFacts,
    cancel: &'a CancelToken,
    failures: std::sync::atomic::AtomicU8,
}

impl Executor for FactExecutor<'_> {
    fn execute(
        &self,
        mut options: provenant_os::exec::ExecOptions<'_>,
    ) -> Result<ExecOutcome, provenant_os::exec::ExecError> {
        use provenant_os::exec::StreamPolicy;
        use std::sync::atomic::Ordering;
        if self.cancel.is_cancelled() {
            self.failures.fetch_or(1, Ordering::Relaxed);
            return Err(provenant_os::exec::ExecError::Cancelled);
        }
        let args: Vec<_> = options
            .argv
            .iter()
            .skip(1)
            .map(|arg| arg.to_str().unwrap_or(""))
            .collect();
        let fact_limit = provenant_os::git::fact_capture_limit(&args, 0);
        if fact_limit != 0 {
            let limit = if fact_limit == provenant_os::git::REPO_FACT_SCALAR_LIMIT {
                self.facts.scalar_limit
            } else {
                self.facts.inventory_limit
            };
            if let StreamPolicy::Capture { limit: existing } = &mut options.stdout {
                *existing = (*existing).min(limit);
            }
            options.stop_on_output_limit = true;
        }
        let result = self.executor.execute(options);
        if self.cancel.is_cancelled() {
            self.failures.fetch_or(1, Ordering::Relaxed);
            return Err(provenant_os::exec::ExecError::Cancelled);
        }
        if fact_limit != 0
            && let Ok(outcome) = &result
        {
            let flags = u8::from(outcome.cancelled)
                | (u8::from(outcome.timed_out) << 1)
                // The executor stops on either stream's capture limit. A
                // bounded stderr excerpt must not become a stdout fact-limit
                // error and hide the caller's command failure diagnostic.
                | (u8::from(outcome.stdout.truncated
                    || (outcome.overflow_stopped && !outcome.stderr.truncated)) << 2)
                | (u8::from(
                    outcome.drain_truncated
                        || outcome.stdout.read_failed
                        || outcome.stdout.sink_failed,
                ) << 3);
            self.failures.fetch_or(flags, Ordering::Relaxed);
        }
        result
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
    fn scoped_fact_execution_preserves_tighter_caps_and_reports_squashed_truncation()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_os::exec::{ExecError, ExecOptions, StreamPolicy};
        struct Truncated {
            expected: usize,
        }
        impl Executor for Truncated {
            fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                assert!(
                    matches!(options.stdout, StreamPolicy::Capture { limit } if limit == self.expected)
                );
                Ok(ExecOutcome {
                    termination: Termination::Exit(0),
                    stdout: provenant_os::exec::CapturedStream {
                        truncated: true,
                        ..Default::default()
                    },
                    stderr: Default::default(),
                    timed_out: false,
                    drain_truncated: false,
                    overflow_stopped: true,
                    cancelled: false,
                    elapsed_micros: 0,
                })
            }
        }
        let facts = RepoFacts::with_limits(4, 8)?;
        let cancellation = CancelToken::new();
        for (words, caller_cap, expected) in [
            (vec!["git", "rev-parse", "HEAD"], 40, 8),
            (vec!["git", "rev-parse", "HEAD"], 2, 2),
            (vec!["git", "worktree", "list"], 40, 4),
        ] {
            let argv: Vec<OsString> = words.into_iter().map(OsString::from).collect();
            let result: Result<(), WorktreeError> =
                facts.with_executor(&Truncated { expected }, &cancellation, |executor| {
                    let mut options =
                        ExecOptions::new(&argv, Path::new("/"), std::time::Duration::from_secs(1));
                    options.stdout = StreamPolicy::Capture { limit: caller_cap };
                    let _ = executor.execute(options);
                    Ok(()) // Resolver's own error mapping cannot discard fact evidence.
                });
            assert!(matches!(
                result,
                Err(WorktreeError::Utility(UtilityError::Truncated {
                    stream: "stdout"
                }))
            ));
        }
        Ok(())
    }

    #[test]
    fn scoped_fact_execution_preserves_stderr_overflow_failure_without_hiding_stdout_loss()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_os::exec::{CapturedStream, ExecError, ExecOptions};
        struct StderrOverflow {
            stdout_truncated: bool,
        }
        impl Executor for StderrOverflow {
            fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                assert!(options.stop_on_output_limit);
                Ok(ExecOutcome {
                    termination: Termination::Signal(9),
                    stdout: CapturedStream {
                        bytes: b"partial fact".to_vec(),
                        truncated: self.stdout_truncated,
                        ..Default::default()
                    },
                    stderr: CapturedStream {
                        bytes: b"fixture git failure".to_vec(),
                        truncated: true,
                        ..Default::default()
                    },
                    timed_out: false,
                    drain_truncated: false,
                    overflow_stopped: true,
                    cancelled: false,
                    elapsed_micros: 0,
                })
            }
        }
        let facts = RepoFacts::default();
        let cancel = CancelToken::new();
        let argv = [
            OsString::from("git"),
            OsString::from("rev-parse"),
            OsString::from("HEAD"),
        ];
        for stdout_truncated in [false, true] {
            let result: Result<(), WorktreeError> =
                facts.with_executor(&StderrOverflow { stdout_truncated }, &cancel, |executor| {
                    let output = executor
                        .execute(ExecOptions::new(
                            &argv,
                            Path::new("/"),
                            std::time::Duration::from_secs(1),
                        ))
                        .map_err(UtilityError::from)?;
                    assert_eq!(output.stderr.bytes, b"fixture git failure");
                    // Never use partial stdout after the stderr cap stopped the
                    // process; retain the caller's precise command failure.
                    match output.termination {
                        Termination::Signal(signal) => {
                            Err(UtilityError::Signalled { signal }.into())
                        }
                        _ => Err(WorktreeError::InvalidOutput),
                    }
                });
            if stdout_truncated {
                assert!(matches!(
                    result,
                    Err(WorktreeError::Utility(UtilityError::Truncated {
                        stream: "stdout"
                    }))
                ));
            } else {
                assert!(matches!(
                    result,
                    Err(WorktreeError::Utility(UtilityError::Signalled {
                        signal: 9
                    }))
                ));
            }
        }
        Ok(())
    }

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
