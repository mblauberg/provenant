//! Host-only delivery. Credential environment travels on the private handoff
//! pipe, never in provider environment, launch records or the delivery receipt.
use crate::{
    PathCheck, Result, RunError, WriterDeliveryReceipt, WriterDeliveryRequest,
    WriterDeliveryStage as Stage, WriterDeliveryState, WriterRequest,
};
use provenant_confinement::redact::Redactor;
use provenant_confinement::repo::GitRefName;
use provenant_core::GitOid;
use provenant_os::{
    exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination},
    fs::Directory,
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

/// Transport-only context. Debug deliberately omits all environment values.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostDeliveryContext {
    environment: Vec<(Vec<u8>, Vec<u8>)>,
    tools: Option<HostTools>,
    bound: Option<BoundDelivery>,
    /// Verification belongs to one host operation and never crosses handoff.
    #[serde(skip)]
    verification: Mutex<crate::objects::VerificationCache>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundDelivery {
    /// Lane head when delivery was prepared, before the writer ran.
    base_head: GitOid,
    push_url: String,
    lane_branch: String,
    pull_request_repo: Option<String>,
    git_dir: PathBuf,
    /// Host repository authority, independent of lane-writable `commondir`.
    #[serde(default)]
    common_git_dir: Option<PathBuf>,
    host_cwd: PathBuf,
    /// Destination sealed at launch; a later request edit cannot redirect it.
    destination: WriterDeliveryRequest,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostTools {
    git: PathBuf,
    gh: Option<PathBuf>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PickupReceipt {
    schema_version: u64,
    request: WriterDeliveryRequest,
    receipt: WriterDeliveryReceipt,
}

fn publish_pickup(
    attempt: &Directory,
    request: &WriterDeliveryRequest,
    receipt: &WriterDeliveryReceipt,
) -> Result<()> {
    attempt
        .atomic_write(
            OsStr::new("delivery-pickup.json"),
            ".pickup-",
            &crate::db::bytes(&PickupReceipt {
                schema_version: 1,
                request: request.clone(),
                receipt: receipt.clone(),
            })?,
            provenant_os::fs::PublishMode::Replace,
        )?
        .require_durable()
        .map_err(RunError::from)
}
// Verification is operation-local host state, absent from handoffs and equality.
impl Clone for HostDeliveryContext {
    fn clone(&self) -> Self {
        Self {
            environment: self.environment.clone(),
            tools: self.tools.clone(),
            bound: self.bound.clone(),
            verification: Mutex::default(),
        }
    }
}
impl PartialEq for HostDeliveryContext {
    fn eq(&self, other: &Self) -> bool {
        self.environment == other.environment
            && self.tools == other.tools
            && self.bound == other.bound
    }
}
impl Eq for HostDeliveryContext {}

impl std::fmt::Debug for HostDeliveryContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostDeliveryContext([private handoff])")
    }
}

impl HostDeliveryContext {
    /// Integration authority never includes the default or primary checkout ref.
    pub fn validate_integration_target(project: &Path, target: &str) -> Result<()> {
        let branch = target.strip_prefix("origin/").unwrap_or(target);
        Self::check_delivery_branch(project, branch)
    }
    /// A publication ref is distinct from a PR's base, which may be protected.
    pub fn check_delivery_branch(project: &Path, target: &str) -> Result<()> {
        let writer = WriterRequest {
            project: project.into(),
            worktree: project.into(),
        };
        let mut host = Self::capture();
        host.host_environment(&writer)?;
        host.tools = Some(HostTools {
            git: host.executable("git", &writer)?,
            gh: None,
        });
        host.refuse_protected_target(&writer, target)
    }

    fn refuse_protected_target(&self, writer: &WriterRequest, target: &str) -> Result<()> {
        if !self
            .command(
                &writer.worktree,
                "git",
                &["symbolic-ref", "--quiet", &format!("refs/heads/{target}")],
                Stage::Prepare,
            )?
            .is_empty()
        {
            return Err(prepare_error(
                "integration target",
                "symbolic branch targets are refused",
            ));
        }
        let remotes = self.command(&writer.worktree, "git", &["remote"], Stage::Prepare)?;
        let mut has_default = false;
        let mut protected_default = false;
        for remote in remotes.lines() {
            let default = self.command(
                &writer.worktree,
                "git",
                &[
                    "symbolic-ref",
                    "--quiet",
                    &format!("refs/remotes/{remote}/HEAD"),
                ],
                Stage::Prepare,
            )?;
            if let Some(branch) = default.strip_prefix(&format!("refs/remotes/{remote}/")) {
                has_default = true;
                protected_default |= branch == target;
            }
        }
        protected_default |= !has_default && matches!(target, "main" | "master");
        let trees = self.command(
            &writer.worktree,
            "git",
            &["worktree", "list", "--porcelain"],
            Stage::Prepare,
        )?;
        let primary_branch = trees
            .split("\n\n")
            .next()
            .unwrap_or_default()
            .lines()
            .find_map(|line| line.strip_prefix("branch refs/heads/"));
        if protected_default || primary_branch == Some(target) {
            return Err(prepare_error(
                "integration target",
                "default and primary checkout branches require PR delivery",
            ));
        }
        Ok(())
    }

    fn refuse_remote_symbolic_target(
        &self,
        cwd: &Path,
        remote: &str,
        target: &str,
        stage: Stage,
    ) -> Result<()> {
        let refs = self.command(
            cwd,
            "git",
            &[
                "ls-remote",
                "--symref",
                "--",
                remote,
                &format!("refs/heads/{target}"),
            ],
            stage,
        )?;
        if refs.lines().any(|line| line.starts_with("ref: ")) {
            return Err(RunError::Delivery(stage));
        }
        Ok(())
    }
    /// Final graph delivery publishes its integration ref, with a body built
    /// from settled node receipts. No checkout is switched or edited.
    pub fn deliver_graph_pr(
        self,
        project: &Path,
        branch: &str,
        base: GitOid,
        body: &str,
    ) -> Result<String> {
        Self::validate_integration_target(project, branch)?;
        let local = !branch.starts_with("origin/");
        let source = if let Some(branch) = branch.strip_prefix("origin/") {
            format!("refs/remotes/origin/{branch}")
        } else {
            format!("refs/heads/{branch}")
        };
        let branch = branch.strip_prefix("origin/").unwrap_or(branch);
        let request = Self::destination(crate::DeliveryMode::Pr, branch)?
            .ok_or(RunError::Validation("graph PR destination"))?;
        let writer = WriterRequest {
            project: project.into(),
            worktree: project.into(),
        };
        let host = self.prepare_with_fetch(&writer, &request, false)?;
        let bound = host
            .bound
            .as_ref()
            .ok_or(RunError::Delivery(Stage::Prepare))?;
        let head = if local {
            host.command(
                project,
                "git",
                &["rev-parse", "--verify", &source],
                Stage::Head,
            )?
        } else {
            host.refresh_target_and_resolve(&writer, branch, Stage::Head)?
        };
        let head = GitOid::parse(&head).map_err(|_| RunError::Delivery(Stage::Head))?;
        host.verify_objects(&writer, head, Stage::Head)?;
        host.verify_objects(&writer, base, Stage::Head)?;
        // A local integration ref may belong to a human. Publish a separate
        // graph-owned ref; never push that existing local branch by name.
        let publication = if local {
            Some(host.graph_publication_branch(project, head)?)
        } else {
            None
        };
        let branch = publication.as_deref().unwrap_or(branch);
        host.refuse_protected_target(&writer, branch)?;
        let reference = format!("refs/heads/{branch}");
        if local {
            host.refuse_remote_symbolic_target(project, &bound.push_url, branch, Stage::Push)?;
            host.command(
                project,
                "git",
                &[
                    "push",
                    "--no-follow-tags",
                    "--recurse-submodules=no",
                    "--",
                    &bound.push_url,
                    &format!("{head}:{reference}"),
                ],
                Stage::Push,
            )?;
        }
        let remote = host.command(
            project,
            "git",
            &[
                "ls-remote",
                "--exit-code",
                "--",
                &bound.push_url,
                &reference,
            ],
            Stage::Verify,
        )?;
        if remote.split_whitespace().next() != Some(head.to_string().as_str()) {
            return Err(RunError::Delivery(Stage::Verify));
        }
        let repo = bound
            .pull_request_repo
            .as_deref()
            .ok_or(RunError::Delivery(Stage::PullRequest))?;
        if let Some(url) = host.existing_pr(project, repo, branch, head)? {
            return Ok(url);
        }
        let commits = host.commit_range_body(&writer, &format!("{base}..{head}"), body)?;
        let url = host.command_with_input(
            project,
            "gh",
            &[
                "pr",
                "create",
                "--repo",
                repo,
                "--head",
                branch,
                "--title",
                &format!("Graph delivery {branch}"),
                "--body-file",
                "-",
            ],
            Stage::PullRequest,
            commits.as_bytes(),
        )?;
        if !url.starts_with("https://") || url.lines().count() != 1 {
            return Err(RunError::Delivery(Stage::PullRequest));
        }
        Ok(url)
    }

    pub(super) fn graph_publication_branch(&self, project: &Path, head: GitOid) -> Result<String> {
        use provenant_core::{CancelToken, Clock, WaitBudget};
        use provenant_os::{clock::SystemClock, fs::LockMode};
        let stage = Stage::Push;
        let common = self.command(
            project,
            "git",
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            stage,
        )?;
        let directory = Directory::open_anchor(Path::new(&common))?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let _lock = directory.lock(
            OsStr::new("provenant-graph-publication.lock"),
            LockMode::Exclusive,
            WaitBudget::cli(clock.mono_now(), 60).map_err(|_| RunError::Delivery(stage))?,
            &cancel,
            &clock,
        )?;
        let branch = format!("provenant/graph/{head}");
        let reference = format!("refs/heads/{branch}");
        let marker = format!("provenant-graph-publication-{head}.json");
        if !self
            .command(
                project,
                "git",
                &["symbolic-ref", "--quiet", &reference],
                stage,
            )?
            .is_empty()
        {
            return Err(RunError::Delivery(stage));
        }
        if let Some(raw) = crate::artifacts::read_sealed(&directory, &marker, crate::RECORD_LIMIT)?
            && raw != branch.as_bytes()
        {
            return Err(RunError::Delivery(stage));
        }
        // Recover an unmarked publication or a marked ref deleted after a PR.
        // Both updates compare the old value, so a different head refuses.
        if self
            .command(
                project,
                "git",
                &[
                    "update-ref",
                    "--no-deref",
                    &reference,
                    &head.to_string(),
                    "",
                ],
                stage,
            )
            .is_err()
        {
            self.command(
                project,
                "git",
                &[
                    "update-ref",
                    "--no-deref",
                    &reference,
                    &head.to_string(),
                    &head.to_string(),
                ],
                stage,
            )?;
        }
        crate::artifacts::seal(&directory, &marker, branch.as_bytes(), crate::RECORD_LIMIT)?;
        Ok(branch)
    }
    /// Resolve the concise mode into the existing explicit destination contract.
    /// Integration names are local unless qualified as `origin/<branch>`.
    pub fn destination(
        mode: crate::DeliveryMode,
        branch: &str,
    ) -> Result<Option<WriterDeliveryRequest>> {
        let (remote, destination, open_pr) = match &mode {
            crate::DeliveryMode::None => return Ok(None),
            crate::DeliveryMode::Push => ("origin".to_owned(), branch.to_owned(), false),
            crate::DeliveryMode::Pr => ("origin".to_owned(), branch.to_owned(), true),
            crate::DeliveryMode::Integrate(target) => match target.strip_prefix("origin/") {
                Some(branch) => ("origin".to_owned(), branch.to_owned(), false),
                None => (String::new(), target.clone(), false),
            },
        };
        let request = WriterDeliveryRequest {
            mode,
            remote,
            branch: destination,
            open_pr,
        };
        validate(&request)?;
        Ok(Some(request))
    }
    pub(crate) fn new(environment: Vec<(OsString, OsString)>) -> Self {
        Self {
            environment: environment
                .into_iter()
                .map(|(k, v)| (k.into_vec(), v.into_vec()))
                .collect(),
            bound: None,
            tools: None,
            verification: Mutex::default(),
        }
    }
    /// Preserve normal host credential/config discovery without Git's ambient
    /// repository/index overrides or any provider-specific environment.
    pub fn capture() -> Self {
        const KEYS: &[&str] = &[
            "HOME",
            "PATH",
            "XDG_CONFIG_HOME",
            "GH_CONFIG_DIR",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "SSH_AUTH_SOCK",
            "SSH_AGENT_PID",
            "SSH_ASKPASS",
            "SSH_ASKPASS_REQUIRE",
            "DISPLAY",
            "GIT_ASKPASS",
            "GIT_SSH",
            "GIT_SSH_COMMAND",
            "GIT_SSH_VARIANT",
            "GIT_TERMINAL_PROMPT",
            "PROVENANT_GIT",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM",
            "GIT_CONFIG_NOSYSTEM",
            "GCM_INTERACTIVE",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "GIT_SSL_CAINFO",
            "GIT_SSL_CAPATH",
            "HTTPS_PROXY",
            "HTTP_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "https_proxy",
            "http_proxy",
            "all_proxy",
            "no_proxy",
            "TMPDIR",
            "LANG",
            "LC_ALL",
        ];
        Self::new(
            std::env::vars_os()
                .filter(|(key, _)| KEYS.iter().any(|k| key == OsStr::new(k)))
                .collect(),
        )
    }
    /// Under writer custody, before the provider launches. Bind the single
    /// push destination and refresh shared refs with the host's Git authority.
    pub(crate) fn prepare(
        self,
        writer: &WriterRequest,
        request: &WriterDeliveryRequest,
    ) -> Result<Self> {
        self.prepare_with_fetch(writer, request, true)
    }

    fn prepare_with_fetch(
        mut self,
        writer: &WriterRequest,
        request: &WriterDeliveryRequest,
        fetch: bool,
    ) -> Result<Self> {
        self.verification = Mutex::default();
        validate(request)?;
        self.host_environment(writer)?;
        let stage = Stage::Prepare;
        let git = self.executable("git", writer)?;
        let gh = if request.open_pr {
            Some(self.executable("gh", writer)?)
        } else {
            None
        };
        self.tools = Some(HostTools { git, gh });
        let common_git_dir = self.trusted_common_git_dir(writer, stage)?;
        self.refuse_protected_target(writer, &request.branch)?;
        if matches!(request.mode, crate::DeliveryMode::Integrate(_)) && request.remote.is_empty() {
            let branch = self.command(
                &writer.worktree,
                "git",
                &["symbolic-ref", "--quiet", "--short", "HEAD"],
                stage,
            )?;
            let git_dir = self.command(
                &writer.worktree,
                "git",
                &["rev-parse", "--absolute-git-dir"],
                stage,
            )?;
            let base_head = self.head(writer)?;
            self.bound = Some(BoundDelivery {
                base_head,
                push_url: String::new(),
                lane_branch: branch,
                pull_request_repo: None,
                git_dir: std::fs::canonicalize(git_dir)
                    .map_err(|error| prepare_error("git dir", error))?,
                common_git_dir: Some(common_git_dir),
                host_cwd: writer.project.clone(),
                destination: request.clone(),
            });
            return Ok(self);
        }
        let urls = self.command(
            &writer.worktree,
            "git",
            &["remote", "get-url", "--push", "--all", &request.remote],
            stage,
        )?;
        let mut urls = urls.lines();
        let url = urls
            .next()
            .filter(|v| !v.is_empty() && !v.starts_with('-'))
            .ok_or_else(|| prepare_error("remote URL", "no usable push URL"))?;
        if urls.next().is_some() {
            return Err(prepare_error("remote URL", "multiple push URLs"));
        }
        // Embedded HTTP credentials would become process arguments below.
        // Host credential helpers, gh config and the SSH agent remain available.
        if let Some((scheme, rest)) = url.split_once("://")
            && ((scheme != "ssh"
                && rest
                    .split('/')
                    .next()
                    .is_some_and(|authority| authority.contains('@')))
                || rest.contains(['?', '#'])
                || scheme == "ssh"
                    && rest
                        .split('@')
                        .next()
                        .is_some_and(|user| user.contains(':')))
        {
            return Err(prepare_error(
                "remote URL",
                "embedded credentials, query or fragment refused",
            ));
        }
        let branch = self.command(
            &writer.worktree,
            "git",
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            stage,
        )?;
        GitRefName::branch(&branch).map_err(|error| prepare_error("HEAD", error))?;
        let git_dir = self.command(
            &writer.worktree,
            "git",
            &["rev-parse", "--absolute-git-dir"],
            stage,
        )?;
        let push_url = if !url.contains(':') && !Path::new(url).is_absolute() {
            std::fs::canonicalize(writer.worktree.join(url))
                .map_err(|error| prepare_error("remote URL", error))?
                .to_str()
                .ok_or_else(|| prepare_error("remote URL", "path is not UTF-8"))?
                .to_owned()
        } else {
            url.to_owned()
        };
        let base = self.command(
            &writer.worktree,
            "git",
            &["rev-parse", "--verify", "HEAD"],
            stage,
        )?;
        self.bound = Some(BoundDelivery {
            base_head: GitOid::parse(&base).map_err(|error| prepare_error("HEAD", error))?,
            push_url,
            lane_branch: branch,
            pull_request_repo: request
                .open_pr
                .then(|| pull_request_repo(url))
                .transpose()
                .map_err(|error| prepare_error("remote URL", error))?,
            git_dir: std::fs::canonicalize(git_dir)
                .map_err(|error| prepare_error("git dir", error))?,
            common_git_dir: Some(common_git_dir),
            host_cwd: writer.project.clone(),
            destination: request.clone(),
        });
        self.validate_transport_helpers(writer)?;
        if fetch && !matches!(request.mode, crate::DeliveryMode::Integrate(_)) {
            let uploadpack = format!("remote.{}.uploadpack=git-upload-pack", request.remote);
            let refspec = format!("+refs/heads/*:refs/remotes/{}/*", request.remote);
            self.command(
                &writer.worktree,
                "git",
                &[
                    "-c",
                    "protocol.allow=never",
                    "-c",
                    "protocol.ssh.allow=always",
                    "-c",
                    "protocol.https.allow=always",
                    "-c",
                    "protocol.http.allow=always",
                    "-c",
                    "protocol.file.allow=always",
                    "-c",
                    &uploadpack,
                    "-c",
                    "core.logAllRefUpdates=false",
                    "fetch",
                    "--no-auto-maintenance",
                    "--no-tags",
                    "--no-recurse-submodules",
                    "--refmap=",
                    "--",
                    &request.remote,
                    &refspec,
                ],
                stage,
            )?;
        }
        Ok(self)
    }
    /// Refresh origin under writer custody with delivery's host credential and
    /// executable checks, while constraining the refspec and transport helpers.
    pub(crate) fn refresh_origin(writer: &WriterRequest, git_dir: &Path) -> Result<()> {
        Self::capture().refresh_origin_with(writer, git_dir)
    }
    pub(super) fn refresh_origin_with(self, writer: &WriterRequest, git_dir: &Path) -> Result<()> {
        let mut host = self;
        host.host_environment(writer)?;
        host.environment
            .retain(|(key, _)| key != b"GIT_TERMINAL_PROMPT");
        host.environment
            .push((b"GIT_TERMINAL_PROMPT".to_vec(), b"0".to_vec()));
        host.tools = Some(HostTools {
            git: host.executable("git", writer)?,
            gh: None,
        });
        let git_dir = git_dir
            .to_str()
            .ok_or_else(|| prepare_error("git dir", "path is not UTF-8"))?;
        let worktree = writer
            .worktree
            .to_str()
            .ok_or_else(|| prepare_error("worktree", "path is not UTF-8"))?;
        let remotes = host.command(
            &writer.worktree,
            "git",
            &["--git-dir", git_dir, "--work-tree", worktree, "remote"],
            Stage::Prepare,
        )?;
        if !remotes.lines().any(|line| line == "origin") {
            return Ok(());
        }
        host.validate_transport_helpers(writer)?;
        let advertised = host.command(
            &writer.worktree,
            "git",
            &[
                "--git-dir",
                git_dir,
                "--work-tree",
                worktree,
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.ssh.allow=always",
                "-c",
                "protocol.https.allow=always",
                "-c",
                "protocol.http.allow=always",
                "-c",
                "protocol.file.allow=always",
                "-c",
                "remote.origin.uploadpack=git-upload-pack",
                "ls-remote",
                "--symref",
                "--",
                "origin",
                "HEAD",
                "refs/heads/*",
            ],
            Stage::Prepare,
        )?;
        let mut default = None;
        let mut heads = Vec::new();
        for line in advertised.lines() {
            let (value, name) = line
                .split_once('\t')
                .ok_or_else(|| prepare_error("origin refresh", "malformed remote advertisement"))?;
            if name == "HEAD" {
                if let Some(branch) = value.strip_prefix("ref: refs/heads/") {
                    default = Some(branch.to_owned());
                } else if GitOid::parse(value).is_err() {
                    return Err(prepare_error("origin refresh", "malformed remote HEAD"));
                }
            } else if let Some(branch) = name.strip_prefix("refs/heads/") {
                if GitOid::parse(value).is_err() || GitRefName::branch(branch).is_err() {
                    return Err(prepare_error("origin refresh", "malformed advertised head"));
                }
                heads.push(branch.to_owned());
            } else {
                return Err(prepare_error("origin refresh", "unexpected advertised ref"));
            }
        }
        let refused: Vec<_> = heads
            .iter()
            .filter(|name| {
                heads
                    .iter()
                    .any(|other| provenant_core::refs_collide(name, other))
            })
            .collect();
        use std::io::Write;
        for name in &refused {
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: refusing colliding origin branch {name:?}"
            );
        }
        if default
            .as_ref()
            .is_some_and(|branch| refused.contains(&branch))
        {
            return Err(prepare_error(
                "origin refresh",
                "default branch has a case or Unicode-normalisation collision",
            ));
        }
        let refspecs: Vec<_> = heads
            .iter()
            .filter(|name| !refused.contains(name))
            .map(|name| format!("+refs/heads/{name}:refs/remotes/origin/{name}"))
            .collect();
        if refspecs.is_empty() {
            return Ok(());
        }
        let mut args = vec![
            "--git-dir",
            git_dir,
            "--work-tree",
            worktree,
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.ssh.allow=always",
            "-c",
            "protocol.https.allow=always",
            "-c",
            "protocol.http.allow=always",
            "-c",
            "protocol.file.allow=always",
            "-c",
            "remote.origin.uploadpack=git-upload-pack",
            "-c",
            "core.logAllRefUpdates=false",
            "fetch",
            "--no-auto-maintenance",
            "--no-tags",
            "--no-recurse-submodules",
            "--refmap=",
            "--",
            "origin",
        ];
        args.extend(refspecs.iter().map(String::as_str));
        host.command(&writer.worktree, "git", &args, Stage::Prepare)?;
        Ok(())
    }

    /// Refresh under setup custody and use the verified remote-advertised object ID.
    fn refresh_target_and_resolve(
        &self,
        writer: &WriterRequest,
        branch: &str,
        stage: Stage,
    ) -> Result<String> {
        use provenant_core::{CancelToken, Clock, WaitBudget};
        use provenant_os::{clock::SystemClock, fs::LockMode};
        let branch_ref = GitRefName::branch(branch).map_err(|_| RunError::Delivery(stage))?;
        let common = self.command(
            &writer.worktree,
            "git",
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            stage,
        )?;
        let directory = Directory::open_anchor(Path::new(&common))?;
        let name = OsStr::new("provenant-lane-setup.lock");
        match directory.create_new(name) {
            Ok(file) => {
                file.sync()?;
                directory.sync()?;
            }
            Err(failure) if failure.is_exists() => {}
            Err(failure) => return Err(failure.into()),
        }
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let _lock = directory.lock(
            name,
            LockMode::Exclusive,
            WaitBudget::cli(clock.mono_now(), 120).map_err(|_| RunError::Delivery(stage))?,
            &cancel,
            &clock,
        )?;
        crate::writer::verify_writer_git_paths(Path::new(&common), &branch_ref)?;
        self.validate_transport_helpers(writer)?;
        let source = format!("refs/heads/{branch}");
        // Read and fetch the same sealed destination used for the later push.
        // A configured origin may have different fetch and push URLs.
        let remote = &self
            .bound
            .as_ref()
            .ok_or(RunError::Delivery(stage))?
            .push_url;
        let query = || {
            self.command(
                &writer.worktree,
                "git",
                &[
                    "-c",
                    "protocol.allow=never",
                    "-c",
                    "protocol.ssh.allow=always",
                    "-c",
                    "protocol.https.allow=always",
                    "-c",
                    "protocol.http.allow=always",
                    "-c",
                    "protocol.file.allow=always",
                    "-c",
                    "remote.origin.uploadpack=git-upload-pack",
                    "ls-remote",
                    "--exit-code",
                    "--",
                    remote,
                    &source,
                ],
                stage,
            )
        };
        let advertised = query()?;
        let (oid, reference) = advertised
            .split_once('\t')
            .ok_or(RunError::Delivery(stage))?;
        if reference != source || GitOid::parse(oid).is_err() {
            return Err(RunError::Delivery(stage));
        }
        let tracking = format!("refs/remotes/origin/{branch}");
        let refspec = format!("+{source}:{tracking}");
        self.command(
            &writer.worktree,
            "git",
            &[
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.ssh.allow=always",
                "-c",
                "protocol.https.allow=always",
                "-c",
                "protocol.http.allow=always",
                "-c",
                "protocol.file.allow=always",
                "-c",
                "remote.origin.uploadpack=git-upload-pack",
                "-c",
                "core.logAllRefUpdates=false",
                "fetch",
                "--no-auto-maintenance",
                "--no-tags",
                "--no-recurse-submodules",
                "--refmap=",
                "--",
                remote,
                &refspec,
            ],
            stage,
        )?;
        if query()? != advertised {
            return Err(RunError::Delivery(stage));
        }
        self.verify_objects(
            writer,
            GitOid::parse(oid).map_err(|_| RunError::Delivery(stage))?,
            stage,
        )?;
        Ok(oid.to_owned())
    }

    fn validate_transport_helpers(&self, writer: &WriterRequest) -> Result<()> {
        let stage = Stage::Prepare;
        // Free-form shell helpers can select writer code. Support normal
        // executable helpers and gh's standard helper, otherwise refuse early.
        let helpers = self.command(
            &writer.worktree,
            "git",
            &[
                "config",
                "--get-regexp",
                "^(credential(\\..*)?\\.helper|core\\.sshcommand)$",
            ],
            stage,
        )?;
        for line in helpers.lines() {
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            if value.trim().is_empty() {
                continue;
            }
            if key.eq_ignore_ascii_case("core.sshcommand") {
                return Err(prepare_error(
                    "credential helper",
                    "delivery does not support core.sshCommand; use host SSH config",
                ));
            }
            if let Some(command) = value.strip_prefix('!') {
                let parts: Vec<_> = command.split_whitespace().collect();
                if parts.len() != 3
                    || parts[1..] != ["auth", "git-credential"]
                    || (parts[0] != "gh" && !Path::new(parts[0]).is_absolute())
                    || !parts[0]
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"/_-.=~:@".contains(&b))
                {
                    return Err(prepare_error(
                        "credential helper",
                        "delivery refuses an opaque shell credential helper",
                    ));
                }
                if Path::new(parts[0]).is_absolute() {
                    credential_helper(Path::new(parts[0]), writer)?;
                } else {
                    self.executable("gh", writer)?;
                }
            } else {
                let parts: Vec<_> = value.split_whitespace().collect();
                if !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b" /_-.=~:@".contains(&b))
                {
                    return Err(prepare_error(
                        "credential helper",
                        "delivery credential helper needs a plain executable",
                    ));
                }
                if Path::new(parts[0]).is_absolute() {
                    credential_helper(Path::new(parts[0]), writer)?;
                } else if parts[0].contains('/') {
                    return Err(prepare_error(
                        "credential helper",
                        "delivery credential helper must resolve outside the lane",
                    ));
                }
            }
        }
        Ok(())
    }
    fn host_environment(&mut self, writer: &WriterRequest) -> Result<()> {
        for (key, value) in &mut self.environment {
            let name = std::str::from_utf8(key).unwrap_or_default();
            if name == "GIT_SSH_COMMAND" && !value.is_empty() {
                return Err(prepare_error(
                    "environment",
                    "GIT_SSH_COMMAND is not supported; use host SSH config",
                ));
            }
            let path = OsString::from_vec(value.clone());
            if name == "PATH" {
                let paths: Vec<_> = std::env::split_paths(&path)
                    .filter(|p| p.is_absolute())
                    .filter_map(|p| std::fs::canonicalize(p).ok())
                    .filter(|p| !p.starts_with(&writer.worktree) && !p.starts_with(&writer.project))
                    .collect();
                *value = std::env::join_paths(paths)
                    .map_err(|_| prepare_error("environment", "PATH is invalid"))?
                    .into_vec();
            } else if matches!(
                name,
                "HOME"
                    | "XDG_CONFIG_HOME"
                    | "GH_CONFIG_DIR"
                    | "GIT_CONFIG_GLOBAL"
                    | "GIT_CONFIG_SYSTEM"
                    | "GIT_SSH"
                    | "GIT_ASKPASS"
                    | "SSH_ASKPASS"
            ) && !value.is_empty()
            {
                *value = host_path(Path::new(&path), writer)
                    .map_err(|error| prepare_error("environment", format!("{name}: {error}")))?
                    .into_os_string()
                    .into_vec();
            }
        }
        Ok(())
    }
    fn executable(&self, tool: &str, writer: &WriterRequest) -> Result<PathBuf> {
        if tool == "git" {
            let override_path = self
                .environment
                .iter()
                .find(|(key, _)| key == b"PROVENANT_GIT")
                .map(|(_, value)| OsString::from_vec(value.clone()));
            let path =
                provenant_os::git::resolve_git_executable(override_path.as_deref().map(Path::new))
                    .ok_or_else(|| prepare_error("git executable", "git is unavailable"))?;
            return host_path(&path, writer)
                .map_err(|error| prepare_error("git executable", error));
        }
        let path = self
            .environment
            .iter()
            .find(|(k, _)| k == b"PATH")
            .map(|(_, v)| OsString::from_vec(v.clone()))
            .ok_or_else(|| {
                prepare_error(
                    "gh executable",
                    format!("{tool} is unavailable: PATH is unset"),
                )
            })?;
        std::env::split_paths(&path)
            .filter(|p| p.is_absolute())
            .map(|p| p.join(tool))
            .filter_map(|p| std::fs::canonicalize(p).ok())
            .find(|p| !p.starts_with(&writer.worktree) && provenant_os::sys::is_executable(p))
            .ok_or_else(|| prepare_error("gh executable", format!("{tool} is unavailable on PATH")))
    }
    pub(super) fn command(
        &self,
        cwd: &Path,
        tool: &str,
        args: &[&str],
        stage: Stage,
    ) -> Result<String> {
        self.command_with_input(cwd, tool, args, stage, b"")
    }
    fn command_with_input(
        &self,
        cwd: &Path,
        tool: &str,
        args: &[&str],
        stage: Stage,
        input: &[u8],
    ) -> Result<String> {
        let failure = |reason: String| {
            if stage == Stage::Prepare {
                let step = match args.first().copied() {
                    Some("remote") => "remote URL",
                    Some("symbolic-ref") => "HEAD",
                    Some("rev-parse") if args.contains(&"--absolute-git-dir") => "git dir",
                    Some("rev-parse") => "HEAD",
                    Some("config") => "credential helper",
                    _ if args.contains(&"fetch") => "fetch",
                    _ => "git executable",
                };
                prepare_error(step, reason)
            } else {
                RunError::Delivery(stage)
            }
        };
        let mut environment: Vec<_> = self
            .environment
            .iter()
            .map(|(k, v)| (OsString::from_vec(k.clone()), OsString::from_vec(v.clone())))
            .collect();
        let cwd = if let Some(bound) = &self.bound {
            environment.push(("GIT_DIR".into(), bound.git_dir.as_os_str().to_owned()));
            environment.push((
                "GIT_WORK_TREE".into(),
                bound.host_cwd.as_os_str().to_owned(),
            ));
            bound.host_cwd.as_path()
        } else {
            cwd
        };
        let tools = self
            .tools
            .as_ref()
            .ok_or_else(|| failure("host tools are unavailable".into()))?;
        let executable = match tool {
            "git" => &tools.git,
            "gh" => tools
                .gh
                .as_ref()
                .ok_or_else(|| failure("gh is unavailable".into()))?,
            _ => return Err(failure(format!("{tool} is unavailable"))),
        };
        let git_options: &[&str] = if tool == "git" {
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
            ]
        } else {
            &[]
        };
        let argv: Vec<OsString> = std::iter::once(executable.as_os_str().to_owned())
            .chain(git_options.iter().chain(args).map(OsString::from))
            .collect();
        let executor = SystemExecutor::new(1).map_err(|error| failure(error.to_string()))?;
        let mut redactor = if stage == Stage::Prepare {
            Some(
                Redactor::new(
                    self.environment
                        .iter()
                        .filter(|(key, value)| {
                            matches!(key.as_slice(), b"GH_TOKEN" | b"GITHUB_TOKEN")
                                && !value.is_empty()
                        })
                        .map(|(_, value)| value.clone()),
                )
                .map_err(|_| failure("diagnostic redaction unavailable".into()))?,
            )
        } else {
            None
        };
        let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(30));
        options.env = Some(&environment);
        options.stdin = Some(input);
        let commit_log = tool == "git" && args.first() == Some(&"log");
        options.stdout = StreamPolicy::Capture {
            limit: if commit_log { 56_000 } else { 16 * 1024 },
        };
        options.stop_on_output_limit = commit_log;
        options.stderr = if stage == Stage::Prepare {
            StreamPolicy::Capture { limit: 4096 }
        } else {
            StreamPolicy::Discard
        };
        options.stderr_filter = redactor
            .as_mut()
            .map(|redactor| redactor as &mut dyn provenant_os::exec::CaptureFilter);
        let output = executor
            .execute(options)
            .map_err(|error| failure(format!("{}: {error}", executable.display())))?;
        let absent_helpers = args.starts_with(&["config", "--get-regexp"])
            && output.termination == Termination::Exit(1)
            && output.stdout.bytes.is_empty();
        let absent_default = args.starts_with(&["symbolic-ref", "--quiet"])
            && output.termination == Termination::Exit(1)
            && output.stdout.bytes.is_empty();
        let bounded_log = commit_log && output.stdout.truncated && output.overflow_stopped;
        if (output.termination != Termination::Exit(0)
            && !absent_helpers
            && !absent_default
            && !bounded_log)
            || output.timed_out
            || output.stdout.truncated && !commit_log
            || output.stdout.read_failed
            || output.drain_truncated
        {
            let stderr = if output.stderr.filter_failed {
                "diagnostic redaction failed".into()
            } else {
                redact_url_userinfo(&String::from_utf8_lossy(&output.stderr.bytes))
            };
            return Err(failure(format!(
                "{tool} {:?} (timed out: {}): {stderr}",
                output.termination, output.timed_out
            )));
        }
        if commit_log && output.stdout.truncated {
            return Ok(format!(
                "{}\n\n[Commit summary truncated.]",
                String::from_utf8_lossy(&output.stdout.bytes)
            ));
        }
        String::from_utf8(output.stdout.bytes)
            .map(|v| v.trim().into())
            .map_err(|error| failure(error.to_string()))
    }
    pub(super) fn head(&self, writer: &WriterRequest) -> Result<GitOid> {
        let head = self.command(
            &writer.worktree,
            "git",
            &["rev-parse", "--verify", "HEAD"],
            Stage::Head,
        )?;
        let head = GitOid::parse(&head).map_err(|_| RunError::Delivery(Stage::Head))?;
        self.verify_objects(writer, head, Stage::Head)?;
        Ok(head)
    }
    fn verify_objects(&self, writer: &WriterRequest, head: GitOid, stage: Stage) -> Result<()> {
        let git = &self.tools.as_ref().ok_or(RunError::Delivery(stage))?.git;
        let common = self
            .bound
            .as_ref()
            .and_then(|bound| bound.common_git_dir.clone())
            .map(Ok)
            .unwrap_or_else(|| self.trusted_common_git_dir(writer, stage))?;
        self.verification
            .lock()
            .map_err(|_| RunError::Delivery(stage))?
            .verify_graph(git, &common, &writer.project, head)
            .map_err(|_| RunError::Delivery(stage))
    }
    fn trusted_common_git_dir(&self, writer: &WriterRequest, stage: Stage) -> Result<PathBuf> {
        // Old handoffs have no sealed common directory. Resolve through the
        // protected host checkout, without installing the lane's GIT_DIR.
        let mut host = self.clone();
        host.bound = None;
        let common = host.command(
            &writer.project,
            "git",
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            stage,
        )?;
        std::fs::canonicalize(common).map_err(|_| RunError::Delivery(stage))
    }
    fn clean(&self, writer: &WriterRequest) -> Result<()> {
        let path = writer.worktree.to_str().ok_or_else(|| {
            RunError::unsafe_path(writer.worktree.clone(), PathCheck::Containment)
        })?;
        let status = self.command(
            &writer.worktree,
            "git",
            &[
                "--work-tree",
                path,
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
            ],
            Stage::Head,
        )?;
        if !status.is_empty() {
            return Err(RunError::Delivery(Stage::Head));
        }
        Ok(())
    }

    pub(super) fn commit_body(
        &self,
        writer: &WriterRequest,
        head: GitOid,
        identity: &str,
    ) -> Result<String> {
        let bound = self.bound.as_ref().ok_or(RunError::Delivery(Stage::Head))?;
        self.verify_objects(writer, head, Stage::Head)?;
        self.verify_objects(writer, bound.base_head, Stage::Head)?;
        let range = if bound.base_head == head {
            head.to_string()
        } else {
            format!("{}..{head}", bound.base_head)
        };
        self.commit_range_body(writer, &range, identity)
    }

    fn commit_range_body(
        &self,
        writer: &WriterRequest,
        range: &str,
        identity: &str,
    ) -> Result<String> {
        let commits = self.command(
            &writer.worktree,
            "git",
            &["log", "--max-count=100", "--format=%s%n%b", range, "--"],
            Stage::Head,
        )?;
        Ok(cap_pr_body(format!("{identity}\n\n{commits}")))
    }

    /// Serialise by destination in the common Git directory. merge-tree and
    /// commit-tree create objects only; CAS publishes after a conflict-free
    /// result, so conflicts never leave an index, sequencer or checkout changed.
    pub(super) fn integrate(
        &self,
        writer: &WriterRequest,
        request: &WriterDeliveryRequest,
        head: GitOid,
    ) -> Result<()> {
        use provenant_core::{CancelToken, Clock, WaitBudget};
        use provenant_os::{clock::SystemClock, fs::LockMode};
        let stage = Stage::Integrate;
        self.refuse_protected_target(writer, &request.branch)
            .map_err(|_| RunError::Delivery(stage))?;
        let bound = self.bound.as_ref().ok_or(RunError::Delivery(stage))?;
        let common = self.command(
            &writer.worktree,
            "git",
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            stage,
        )?;
        let directory =
            Directory::open_anchor(Path::new(&common)).map_err(|_| RunError::Delivery(stage))?;
        let clock = SystemClock;
        let cancel = CancelToken::new();
        let lock_name = format!(
            "provenant-integrate-{}.lock",
            provenant_core::hex(
                provenant_records::CanonicalV1::file_digest(
                    format!("{}/{}", request.remote, request.branch).as_bytes()
                )
                .bytes()
            )
        );
        // Establish the owner inode before concurrent acquisitions. Native
        // macOS can refuse a racing O_CREAT open with ENOENT.
        match directory.create_new(OsStr::new(&lock_name)) {
            Ok(file) => {
                file.sync()?;
                directory.sync()?;
            }
            Err(error) if error.is_exists() => {}
            Err(_) => return Err(RunError::Delivery(stage)),
        }
        let _lock = directory
            .lock_existing(
                OsStr::new(&lock_name),
                LockMode::Exclusive,
                // Each of at most 24 commands under this lock has a 30s deadline.
                WaitBudget::cli(clock.mono_now(), 24 * 30 + 60)
                    .map_err(|_| RunError::Delivery(stage))?,
                &cancel,
                &clock,
            )
            .map_err(|_| RunError::Delivery(stage))?;
        self.refuse_protected_target(writer, &request.branch)
            .map_err(|_| RunError::Delivery(stage))?;
        self.verify_objects(writer, head, stage)?;
        self.clean(writer)?;
        if self.head(writer)? != head {
            return Err(RunError::Delivery(Stage::Head));
        }
        let destination = format!("refs/heads/{}", request.branch);
        let remote = !request.remote.is_empty();
        let target = if remote {
            let tracking = format!("refs/remotes/{}/{}", request.remote, request.branch);
            if request.remote != "origin" {
                self.command(
                    &writer.worktree,
                    "git",
                    &[
                        "fetch",
                        "--no-tags",
                        "--no-recurse-submodules",
                        "--",
                        &bound.push_url,
                        &format!("+{destination}:{tracking}"),
                    ],
                    stage,
                )?;
            }
            tracking
        } else {
            destination.clone()
        };
        let expected = if remote && request.remote == "origin" {
            self.refresh_target_and_resolve(writer, &request.branch, stage)?
        } else {
            self.command(
                &writer.worktree,
                "git",
                &["rev-parse", "--verify", &target],
                stage,
            )?
        };
        let expected_oid = GitOid::parse(&expected).map_err(|_| RunError::Delivery(stage))?;
        self.verify_objects(writer, expected_oid, stage)?;
        if self
            .command(
                &writer.worktree,
                "git",
                &["merge-base", "--is-ancestor", &head.to_string(), &expected],
                stage,
            )
            .is_ok()
        {
            return Ok(());
        }
        if !remote {
            let trees = self.command(
                &writer.worktree,
                "git",
                &["worktree", "list", "--porcelain"],
                stage,
            )?;
            if trees
                .lines()
                .any(|line| line == format!("branch {destination}"))
            {
                return Err(RunError::Delivery(stage));
            }
        }
        let candidate = if self
            .command(
                &writer.worktree,
                "git",
                &["merge-base", "--is-ancestor", &expected, &head.to_string()],
                stage,
            )
            .is_ok()
        {
            head
        } else {
            // A configured merge driver may execute arbitrary repository code.
            if !self
                .command(
                    &writer.worktree,
                    "git",
                    &["config", "--get-regexp", "^merge\\..*\\.driver$"],
                    stage,
                )?
                .is_empty()
            {
                return Err(RunError::Delivery(stage));
            }
            let tree = self.command(
                &writer.worktree,
                "git",
                &["merge-tree", "--write-tree", &expected, &head.to_string()],
                stage,
            )?;
            let tree = GitOid::parse(tree.lines().next().unwrap_or_default())
                .map_err(|_| RunError::Delivery(stage))?;
            let commit = self.command(
                &writer.worktree,
                "git",
                &[
                    "-c",
                    "user.name=Provenant",
                    "-c",
                    "user.email=provenant@localhost",
                    "commit-tree",
                    &tree.to_string(),
                    "-p",
                    &expected,
                    "-p",
                    &head.to_string(),
                    "-m",
                    &format!("Merge lane into {}", request.branch),
                ],
                stage,
            )?;
            GitOid::parse(&commit).map_err(|_| RunError::Delivery(stage))?
        };
        self.refuse_protected_target(writer, &request.branch)
            .map_err(|_| RunError::Delivery(stage))?;
        if remote {
            self.refuse_remote_symbolic_target(
                &writer.worktree,
                &bound.push_url,
                &request.branch,
                stage,
            )?;
        }
        if remote {
            self.command(
                &writer.worktree,
                "git",
                &[
                    "push",
                    "--no-follow-tags",
                    "--recurse-submodules=no",
                    &format!("--force-with-lease={destination}:{expected_oid}"),
                    "--",
                    &bound.push_url,
                    &format!("{candidate}:{destination}"),
                ],
                stage,
            )?;
            let verified = if request.remote == "origin" {
                self.refresh_target_and_resolve(writer, &request.branch, Stage::Verify)?
            } else {
                self.command(
                    &writer.worktree,
                    "git",
                    &[
                        "fetch",
                        "--no-tags",
                        "--no-recurse-submodules",
                        "--",
                        &bound.push_url,
                        &format!("+{destination}:{target}"),
                    ],
                    Stage::Verify,
                )?;
                self.command(
                    &writer.worktree,
                    "git",
                    &["rev-parse", "--verify", &target],
                    Stage::Verify,
                )?
            };
            if verified != candidate.to_string() {
                return Err(RunError::Delivery(Stage::Verify));
            }
        } else {
            self.command(
                &writer.worktree,
                "git",
                &[
                    "update-ref",
                    "--no-deref",
                    &destination,
                    &candidate.to_string(),
                    &expected,
                ],
                stage,
            )?;
        }
        if self.head(writer)? != head {
            return Err(RunError::Delivery(Stage::Head));
        }
        Ok(())
    }
    fn existing_pr(
        &self,
        cwd: &Path,
        repo: &str,
        branch: &str,
        head: GitOid,
    ) -> Result<Option<String>> {
        let Ok(raw) = self.command(
            cwd,
            "gh",
            &[
                "pr",
                "view",
                branch,
                "--repo",
                repo,
                "--json",
                "url,state,headRefOid",
            ],
            Stage::PullRequest,
        ) else {
            return Ok(None);
        };
        let value: serde_json::Value =
            serde_json::from_str(&raw).map_err(|_| RunError::Delivery(Stage::PullRequest))?;
        if value["state"] != "OPEN" {
            return Ok(None);
        }
        if value["headRefOid"].as_str() != Some(head.to_string().as_str()) {
            return Err(RunError::Delivery(Stage::Verify));
        }
        let url = value["url"]
            .as_str()
            .filter(|url| url.starts_with("https://") && url.lines().count() == 1)
            .ok_or(RunError::Delivery(Stage::PullRequest))?;
        Ok(Some(url.to_owned()))
    }
    fn push(
        &self,
        writer: &WriterRequest,
        request: &WriterDeliveryRequest,
        head: GitOid,
        body: &str,
    ) -> Result<Option<String>> {
        if self.head(writer)? != head {
            return Err(RunError::Delivery(Stage::Head));
        }
        self.refuse_protected_target(writer, &request.branch)
            .map_err(|_| RunError::Delivery(Stage::Prepare))?;
        self.clean(writer)?;
        if matches!(request.mode, crate::DeliveryMode::Integrate(_)) {
            self.integrate(writer, request, head)?;
            return Ok(None);
        }
        let bound = self
            .bound
            .as_ref()
            .ok_or(RunError::Delivery(Stage::Prepare))?;
        let cwd = &writer.worktree;
        let branch = self.command(
            cwd,
            "git",
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            Stage::Head,
        )?;
        if branch != bound.lane_branch {
            return Err(RunError::Delivery(Stage::Head));
        }
        let destination = format!("refs/heads/{}", request.branch);
        self.refuse_remote_symbolic_target(cwd, &bound.push_url, &request.branch, Stage::Push)?;
        let refspec = format!("{head}:{destination}");
        self.command(
            cwd,
            "git",
            &[
                "push",
                "--no-follow-tags",
                "--recurse-submodules=no",
                "--",
                &bound.push_url,
                &refspec,
            ],
            Stage::Push,
        )?;
        let tracking = format!("refs/remotes/{}/{}", request.remote, request.branch);
        let fetchspec = format!("+{destination}:{tracking}");
        self.command(
            cwd,
            "git",
            &[
                "fetch",
                "--no-tags",
                "--no-recurse-submodules",
                "--",
                &bound.push_url,
                &fetchspec,
            ],
            Stage::Fetch,
        )?;
        let pushed = self.command(
            cwd,
            "git",
            &["rev-parse", "--verify", &tracking],
            Stage::Verify,
        )?;
        if pushed != head.to_string() || self.head(writer)? != head {
            return Err(RunError::Delivery(Stage::Verify));
        }
        for (key, value) in [
            ("remote", request.remote.as_str()),
            ("merge", destination.as_str()),
            ("pushRemote", request.remote.as_str()),
        ] {
            self.command(
                cwd,
                "git",
                &[
                    "config",
                    "--local",
                    &format!("branch.{}.{key}", bound.lane_branch),
                    value,
                ],
                Stage::Upstream,
            )?;
        }
        // Read the upstream back from config: `@{upstream}` and `status -sb`
        // only resolve when the remote's fetch refspec covers the branch, and a
        // narrowed clone (for example main only) is legitimate.
        for (key, value) in [
            ("remote", request.remote.as_str()),
            ("merge", destination.as_str()),
            ("pushRemote", request.remote.as_str()),
        ] {
            let stored = self.command(
                cwd,
                "git",
                &[
                    "config",
                    "--local",
                    "--get",
                    &format!("branch.{}.{key}", bound.lane_branch),
                ],
                Stage::Verify,
            )?;
            if stored != value {
                return Err(RunError::Delivery(Stage::Verify));
            }
        }
        if request.open_pr {
            let repo = bound
                .pull_request_repo
                .as_deref()
                .ok_or(RunError::Delivery(Stage::PullRequest))?;
            if let Some(url) = self.existing_pr(cwd, repo, &request.branch, head)? {
                return Ok(Some(url));
            }
            let url = self.command_with_input(
                cwd,
                "gh",
                &[
                    "pr",
                    "create",
                    "--repo",
                    bound
                        .pull_request_repo
                        .as_deref()
                        .ok_or(RunError::Delivery(Stage::PullRequest))?,
                    "--head",
                    &request.branch,
                    "--title",
                    &format!("Lane delivery {}", &head.to_string()[..12]),
                    "--body-file",
                    "-",
                ],
                Stage::PullRequest,
                body.as_bytes(),
            )?;
            if !url.starts_with("https://") || url.lines().count() != 1 {
                return Err(RunError::Delivery(Stage::PullRequest));
            }
            Ok(Some(url))
        } else {
            Ok(None)
        }
    }
}

pub(super) fn cap_pr_body(mut body: String) -> String {
    const NOTE: &str = "\n\n[Commit summary truncated.]\n";
    if body.len() > 60_000 - NOTE.len() {
        body.truncate(body.floor_char_boundary(60_000 - NOTE.len()));
        body.push_str(NOTE);
    }
    body
}

// Git may include remote URLs in diagnostics. Usernames alone can be access
// tokens, so remove all URL userinfo, including passwordless userinfo.
pub(super) fn redact_url_userinfo(detail: &str) -> String {
    let mut result = String::with_capacity(detail.len());
    let mut rest = detail;
    while let Some(scheme_end) = rest.find("://") {
        let authority_start = scheme_end + 3;
        result.push_str(&rest[..authority_start]);
        rest = &rest[authority_start..];
        let token_end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '<' | '>' | '(' | ')'))
            .unwrap_or(rest.len());
        let token = &rest[..token_end];
        if let Some(at) = token.rfind('@') {
            result.push_str("[REDACTED]");
            result.push_str(&token[at..]);
            rest = &rest[token_end..];
        } else {
            // A malformed or truncated authority may have lost its '@'.
            result.push_str("[REDACTED]");
            let authority_end = token.find(['/', '?', '#']).unwrap_or(token.len());
            rest = &rest[authority_end..];
        }
    }
    result.push_str(rest);
    // Git also accepts scp-style remotes such as user@host:path. The user
    // can itself be a credential, so redact it even without a URL scheme.
    let mut safe = String::with_capacity(result.len());
    for token in result.split_inclusive(|c: char| {
        c.is_whitespace() || matches!(c, '\'' | '"' | '<' | '>' | '(' | ')')
    }) {
        if let Some(at) = token.rfind('@')
            && token[at + 1..].contains(':')
            && !token.contains("://")
        {
            safe.push_str("[REDACTED]");
            safe.push_str(&token[at..]);
        } else {
            safe.push_str(token);
        }
    }
    safe
}

pub(super) fn prepare_error(step: &'static str, detail: impl std::fmt::Display) -> RunError {
    let detail = detail.to_string();
    let safe = provenant_confinement::redact::known_shapes(detail.as_bytes())
        .unwrap_or_else(|_| b"diagnostic redaction failed".to_vec());
    let redacted = redact_url_userinfo(&String::from_utf8_lossy(&safe));
    let mut reason = redacted.split_whitespace().collect::<Vec<_>>().join(" ");
    if reason.len() > 1024 {
        reason.truncate(reason.floor_char_boundary(1024));
        reason.push_str("...");
    }
    RunError::DeliveryPrepare { step, reason }
}

fn credential_helper(path: &Path, writer: &WriterRequest) -> Result<()> {
    let name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    let helper = host_path(path, writer)
        .map_err(|error| prepare_error("credential helper", format!("{name}: {error}")))?;
    if !helper.is_file() || !provenant_os::sys::is_executable(&helper) {
        return Err(prepare_error(
            "credential helper",
            format!("{name} is unavailable ({})", path.display()),
        ));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum HostPathError {
    #[error("not found")]
    NotFound,
    #[error("inside the writer worktree")]
    InsideWorktree,
}

fn host_path(path: &Path, writer: &WriterRequest) -> std::result::Result<PathBuf, HostPathError> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        writer.project.join(path)
    };
    let canonical = match std::fs::canonicalize(&path) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or(HostPathError::NotFound)?;
            std::fs::canonicalize(parent)
                .map_err(|_| HostPathError::NotFound)?
                .join(path.file_name().ok_or(HostPathError::NotFound)?)
        }
        Err(_) => return Err(HostPathError::NotFound),
    };
    if canonical.starts_with(&writer.worktree) {
        return Err(HostPathError::InsideWorktree);
    }
    Ok(canonical)
}

fn pull_request_repo(url: &str) -> Result<String> {
    let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
        if !matches!(scheme, "https" | "ssh") {
            return Err(RunError::Delivery(Stage::Prepare));
        }
        let (authority, path) = rest
            .split_once('/')
            .ok_or(RunError::Delivery(Stage::Prepare))?;
        (authority.rsplit('@').next().unwrap_or_default(), path)
    } else {
        let (authority, path) = url
            .split_once(':')
            .ok_or(RunError::Delivery(Stage::Prepare))?;
        (authority.rsplit('@').next().unwrap_or_default(), path)
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
        || path.split('/').count() != 2
        || path.split('/').any(|p| {
            p.is_empty()
                || p == "."
                || p == ".."
                || !p
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
    {
        return Err(RunError::Delivery(Stage::Prepare));
    }
    Ok(format!("{host}/{path}"))
}

pub(crate) fn validate(request: &WriterDeliveryRequest) -> Result<()> {
    if matches!(request.mode, crate::DeliveryMode::None) {
        return Err(RunError::Validation(
            "none delivery must omit the destination",
        ));
    }
    if matches!(request.mode, crate::DeliveryMode::Integrate(_)) && request.remote.is_empty() {
        GitRefName::branch(&request.branch)
            .map_err(|_| RunError::Validation("integration branch"))?;
        if request.open_pr {
            return Err(RunError::Validation(
                "integration cannot also open a lane PR",
            ));
        }
        return Ok(());
    }
    if request.remote.is_empty()
        || request.remote.len() > 64
        || request.remote.starts_with('-')
        || !request
            .remote
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(RunError::Validation(
            "delivery needs a configured remote name",
        ));
    }
    GitRefName::branch(&request.branch).map_err(|_| RunError::Validation("delivery branch"))?;
    Ok(())
}

impl crate::RunStore {
    pub fn delivered(
        &self,
        id: provenant_core::AttemptId,
        call: &crate::Request<'_>,
    ) -> Result<Option<(WriterDeliveryRequest, WriterDeliveryReceipt)>> {
        let record = self.record(id, call)?;
        let attempt = self.attempt_dir(record.run_id, id, false)?;
        match attempt.read_stable(OsStr::new("delivery-pickup.json"), crate::RECORD_LIMIT) {
            Ok(raw) => {
                let pickup: PickupReceipt = crate::db::decode(&raw)?;
                return Ok(Some((pickup.request, pickup.receipt)));
            }
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
        let Some(receipt) = record
            .delivery
            .filter(|receipt| matches!(receipt.state, WriterDeliveryState::Delivered { .. }))
        else {
            return Ok(None);
        };
        let request = self
            .request(record.run_id, call)?
            .tasks
            .into_iter()
            .find(|task| task.task_id == record.task_id)
            .and_then(|task| task.deliver)
            .ok_or(RunError::Validation("delivered destination missing"))?;
        Ok(Some((request, receipt)))
    }
    pub fn worktree_settlement(
        &self,
        id: provenant_core::AttemptId,
        call: &crate::Request<'_>,
    ) -> Result<Option<serde_json::Value>> {
        let record = self.record(id, call)?;
        let attempt = self.attempt_dir(record.run_id, id, false)?;
        match attempt.read_stable(OsStr::new("worktree-settlement.json"), crate::RECORD_LIMIT) {
            Ok(raw) => Ok(Some(crate::db::decode(&raw)?)),
            Err(error) if error.is_not_found() => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    /// Host lifecycle receipt, separate from the immutable terminal decision.
    pub fn record_worktree_settlement(
        &self,
        id: provenant_core::AttemptId,
        receipt: &serde_json::Value,
        call: &crate::Request<'_>,
    ) -> Result<()> {
        let record = self.record(id, call)?;
        let attempt = self.attempt_dir(record.run_id, id, false)?;
        let _lock = attempt.lock(
            OsStr::new("worktree-settlement.lock"),
            provenant_os::fs::LockMode::Exclusive,
            call.budget,
            call.cancel,
            call.clock,
        )?;
        match attempt.read_stable(OsStr::new("worktree-settlement.json"), crate::RECORD_LIMIT) {
            Ok(raw) => {
                let prior: serde_json::Value = crate::db::decode(&raw)?;
                if prior["state"] == "pruned" {
                    return Ok(());
                }
            }
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
        attempt
            .atomic_write(
                OsStr::new("worktree-settlement.json"),
                ".settlement-",
                &crate::db::bytes(receipt)?,
                provenant_os::fs::PublishMode::Replace,
            )?
            .require_durable()
            .map_err(RunError::from)
    }
    /// Explicit host pickup of a settled writer. Its immutable terminal record
    /// stays intact; the keyed sealed receipt records this later delivery.
    pub fn deliver(
        &self,
        id: provenant_core::AttemptId,
        override_request: Option<WriterDeliveryRequest>,
        host: HostDeliveryContext,
        accept_review: bool,
        call: &crate::Request<'_>,
    ) -> Result<WriterDeliveryReceipt> {
        use provenant_os::fs::LockMode;
        let record = self.record(id, call)?;
        let review_acceptance = match record.state {
            crate::AttemptState::Terminal {
                outcome: crate::Outcome::Succeeded,
                ..
            } => None,
            crate::AttemptState::Terminal {
                outcome: crate::Outcome::ReviewRequired,
                ..
            } if accept_review => Some(crate::WriterReviewAcceptance {
                actor: self.context.actor_id,
                protected_paths: record.protected_paths.clone(),
            }),
            crate::AttemptState::Terminal {
                outcome: crate::Outcome::ReviewRequired,
                ..
            } => {
                return Err(RunError::Validation(
                    "review required; use lane deliver --accept-review ATTEMPT",
                ));
            }
            _ => {
                return Err(RunError::Validation(
                    "delivery needs a succeeded settled writer",
                ));
            }
        };
        let task = self
            .request(record.run_id, call)?
            .tasks
            .into_iter()
            .find(|task| task.task_id == record.task_id)
            .ok_or(RunError::NotFound)?;
        let writer = task
            .writer
            .ok_or(RunError::Validation("delivery needs a writer"))?;
        let request = override_request
            .or(task.deliver.clone())
            .ok_or(RunError::Validation(
                "choose --deliver for a lane without recorded delivery",
            ))?;
        validate(&request)?;
        let head = record
            .head
            .ok_or(RunError::Validation("receipt has no settled head"))?;
        let attempt = self.attempt_dir(record.run_id, id, false)?;
        let key = provenant_core::hex(
            provenant_records::CanonicalV1::file_digest(
                &serde_json::to_vec(&(head, &request))
                    .map_err(|_| RunError::Validation("delivery key"))?,
            )
            .bytes(),
        );
        // New successful receipts never collide with a legacy failed seal.
        let receipt_name = format!("delivery-complete-{key}.json");
        for name in [&receipt_name, &format!("delivery-{key}.json")] {
            if let Some(raw) = crate::artifacts::read_sealed(&attempt, name, crate::RECORD_LIMIT)? {
                let receipt: WriterDeliveryReceipt = crate::db::decode(&raw)?;
                if matches!(receipt.state, WriterDeliveryState::Delivered { .. }) {
                    // Recover a crash between sealing success and publishing
                    // the mutable pickup pointer used by graph observation.
                    publish_pickup(&attempt, &request, &receipt)?;
                    return Ok(receipt);
                }
            }
        }
        if let Some(receipt) = record.delivery.as_ref()
            && task.deliver.as_ref() == Some(&request)
            && receipt.remote == request.remote
            && receipt.branch == request.branch
            && matches!(receipt.state, WriterDeliveryState::Delivered { .. })
            && (!request.open_pr
                || matches!(
                    receipt.state,
                    WriterDeliveryState::Delivered {
                        pull_request: Some(_)
                    }
                ))
        {
            return Ok(receipt.clone());
        }
        if !self.custody_released(&record, call)? {
            return Err(RunError::Validation("writer custody has not settled"));
        }
        let mut launch = crate::prepare_writer(&writer, &self.writer_registry, record.project_id)?;
        let state = launch
            .custody
            .take()
            .ok_or_else(|| RunError::unsafe_path(writer.worktree.clone(), PathCheck::Missing))?
            .0;
        let _writer = state.lock(
            OsStr::new("writer.lock"),
            LockMode::Exclusive,
            call.budget,
            call.cancel,
            call.clock,
        )?;
        match state.metadata_at(OsStr::new("reservation.json"), false) {
            Err(error) if error.is_not_found() => {}
            _ => {
                return Err(RunError::Validation(
                    "worktree has another writer reservation",
                ));
            }
        }
        let host = host.prepare(&writer, &request)?;
        diagnose_legacy_git_policy(&attempt, &format!("Run {} Attempt {id}", record.run_id))?;
        host.clean(&writer)?;
        if host.head(&writer)? != head {
            return Err(RunError::Validation(
                "worktree head differs from the settled receipt",
            ));
        }
        let result = host
            .commit_body(
                &writer,
                head,
                &format!("Run {}\nAttempt {id}", record.run_id),
            )
            .and_then(|body| host.push(&writer, &request, head, &body));
        let state = match result {
            Ok(pull_request) => WriterDeliveryState::Delivered { pull_request },
            Err(RunError::Delivery(stage)) => WriterDeliveryState::Failed { stage },
            Err(_) => WriterDeliveryState::Failed { stage: Stage::Push },
        };
        let receipt = WriterDeliveryReceipt {
            schema_version: 1,
            remote: request.remote.clone(),
            branch: request.branch.clone(),
            head: Some(head),
            review_acceptance,
            state,
        };
        // Failed effects may be explicitly retried; successful effects replay.
        if matches!(receipt.state, WriterDeliveryState::Delivered { .. }) {
            crate::artifacts::seal(
                &attempt,
                &receipt_name,
                &crate::db::bytes(&receipt)?,
                crate::RECORD_LIMIT,
            )?;
            publish_pickup(&attempt, &request, &receipt)?;
        }
        Ok(receipt)
    }
}

fn diagnose_legacy_git_policy(attempt: &Directory, identity: &str) -> Result<()> {
    if crate::objects::legacy_policy(attempt)? {
        crate::objects::legacy_policy_diagnostic(identity);
    }
    Ok(())
}

/// The live holder calls this only after success and confinement checks. A
/// sealed start marker prevents repeating external effects after interruption.
pub(crate) fn deliver_once(
    host: &HostDeliveryContext,
    writer: &WriterRequest,
    request: &WriterDeliveryRequest,
    attempt: &Directory,
    identity: &str,
) -> Result<WriterDeliveryReceipt> {
    *host
        .verification
        .lock()
        .map_err(|_| RunError::Delivery(Stage::Prepare))? =
        crate::objects::VerificationCache::default();
    if let Some(raw) = crate::artifacts::read_sealed(attempt, "delivery.json", crate::RECORD_LIMIT)?
    {
        return crate::db::decode(&raw);
    }
    diagnose_legacy_git_policy(attempt, identity)?;
    if host
        .bound
        .as_ref()
        .is_none_or(|bound| bound.destination != *request)
    {
        return Err(RunError::Validation(
            "delivery destination changed after launch",
        ));
    }
    let started =
        crate::artifacts::read_sealed(attempt, "delivery-started.json", crate::RECORD_LIMIT)?;
    let mut receipt = match &started {
        Some(raw) => crate::db::decode(raw)?,
        None => WriterDeliveryReceipt {
            schema_version: 1,
            remote: request.remote.clone(),
            branch: request.branch.clone(),
            head: host.head(writer).ok(),
            review_acceptance: None,
            state: WriterDeliveryState::Failed {
                stage: Stage::Interrupted,
            },
        },
    };
    if started.is_none() {
        crate::artifacts::seal(
            attempt,
            "delivery-started.json",
            &crate::db::bytes(&receipt)?,
            crate::RECORD_LIMIT,
        )?;
        let unchanged = receipt
            .head
            .zip(host.bound.as_ref())
            .is_some_and(|(head, bound)| head == bound.base_head);
        receipt.state = if unchanged {
            WriterDeliveryState::NoNewCommits
        } else {
            match receipt
                .head
                .ok_or(RunError::Delivery(Stage::Head))
                .and_then(|head| {
                    let body = host.commit_body(writer, head, identity)?;
                    crate::artifacts::seal(
                        attempt,
                        "pull-request-body.md",
                        body.as_bytes(),
                        crate::RESULT_LIMIT,
                    )?;
                    host.push(writer, request, head, &body)
                }) {
                Ok(pull_request) => WriterDeliveryState::Delivered { pull_request },
                Err(RunError::Delivery(stage)) => WriterDeliveryState::Failed { stage },
                Err(_) => WriterDeliveryState::Failed { stage: Stage::Push },
            }
        };
    }
    crate::artifacts::seal(
        attempt,
        "delivery.json",
        &crate::db::bytes(&receipt)?,
        crate::RECORD_LIMIT,
    )?;
    Ok(receipt)
}
