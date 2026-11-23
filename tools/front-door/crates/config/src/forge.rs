//! One resolved Git context per command.
//!
//! A forge operation needs four facts that used to be discovered independently:
//! which remote to talk to, which branch is the default, which branches are
//! protected, and the owner's resolved operation policy. [`ForgeContext`] is
//! the one place that answers them, with the source of every answer attached.
//! The owner resolves it once and passes it down; no consumer rebuilds it.
//!
//! Detection is read-only: it parses the checkout's Git metadata and, when a
//! layer declared nothing, asks the forge through [`ForgeProbe`]. A failure is
//! a refusal that names what was seen, never a silent `origin`/`main` guess.
//!
//! Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command

use crate::ports::PortTable;
use provenant_records::policy::{Policy, Source as PolicySource};
use provenant_records::provenance::{Layer, Source, Sourced};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
};
use thiserror::Error;

/// The project root, as seen by the owner resolving the context.
pub type Project = Path;

/// A refusal from one step of context resolution. It never invents a default:
/// it lists what was seen and the closest spellings, so the operator can add a
/// declaration instead of guessing why a command picked another remote.
#[derive(Debug, Error)]
pub enum ForgeContextError {
    #[error("cannot read the Git metadata at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "no Git remote is declared and none can be inferred; remotes seen: {remotes:?}{}",
        close_hint(.close)
    )]
    NoRemote {
        remotes: Vec<String>,
        close: Vec<String>,
    },
    #[error(
        "the default branch for remote {remote:?} is unknown; branches seen: {branches:?}{}",
        close_hint(.close)
    )]
    NoDefaultBranch {
        remote: String,
        branches: Vec<String>,
        close: Vec<String>,
    },
}

fn close_hint(close: &[String]) -> String {
    if close.is_empty() {
        String::new()
    } else {
        format!("; did you mean {}?", close.join(", "))
    }
}

/// The forge API answers the one question Git metadata cannot: which branch is
/// the default and which branches the forge protects. The real probe is built
/// from a `ForgePort`; tests supply a fake that records the policy it received.
pub trait ForgeProbe {
    fn default_branch(&self, remote: &str) -> Option<String> {
        let _ = remote;
        None
    }
    fn protected_branches(&self) -> Vec<String> {
        Vec::new()
    }
}

/// A context resolved without any forge API access. Declaration and local Git
/// detection still work; only the forge fallback is absent.
#[derive(Clone, Copy, Debug)]
pub struct NoForgeProbe;
impl ForgeProbe for NoForgeProbe {}

const NO_FORGE_PROBE: NoForgeProbe = NoForgeProbe;

/// The owner-supplied inputs to one resolution: the effective port table, the
/// operation policy already resolved for the owner, and, optionally, a declared
/// default branch and a forge probe.
pub struct Config<'a> {
    pub ports: &'a PortTable,
    pub policy: Sourced<Policy>,
    pub default_branch: Option<Sourced<String>>,
    pub forge: &'a dyn ForgeProbe,
}

impl<'a> Config<'a> {
    pub fn new(ports: &'a PortTable, policy: Policy) -> Self {
        // A declared git.default_branch is the first answer ForgeContext asks
        // for, ahead of the remote HEAD symref and the forge API. An empty
        // declaration leaves those detections in place.
        let default_branch = (!ports.git.default_branch.trim().is_empty()).then(|| Sourced {
            value: ports.git.default_branch.trim().to_owned(),
            source: port_source(ports, "git.default_branch", "default_branch"),
        });
        Self {
            ports,
            policy: sourced_policy(policy),
            default_branch,
            forge: &NO_FORGE_PROBE,
        }
    }
}

/// The one Git context, resolved once per command and passed down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForgeContext {
    pub remote: Sourced<String>,
    pub default_branch: Sourced<String>,
    pub protected: Sourced<Vec<String>>,
    pub policy: Sourced<Policy>,
}

impl ForgeContext {
    /// Resolve the context for one command. Detection that finds nothing is a
    /// refusal listing the remotes or branches it saw, with close matches.
    pub fn resolve(config: &Config<'_>, git: &Git) -> Result<Self, ForgeContextError> {
        let remote = Self::remote(config, git)?;
        let default_branch = Self::resolve_default_branch(config, git, &remote.value)?;
        let protected = Self::resolve_protected(config, &default_branch.value);
        Ok(Self {
            remote,
            default_branch,
            protected,
            policy: config.policy.clone(),
        })
    }

    /// Whether a branch is protected. Both bare and fully qualified names are
    /// accepted; the default branch is always protected even when a declaration
    /// forgot to list it.
    pub fn is_protected(&self, branch: &str) -> bool {
        let branch = short_branch(branch);
        branch == short_branch(&self.default_branch.value)
            || self
                .protected
                .value
                .iter()
                .any(|candidate| short_branch(candidate) == branch)
    }

    /// Resolve just the remote from the same inputs the full context uses.
    /// Consumers that must ask the forge before trusting a local default branch
    /// need the remote without a resolution that prefers local Git metadata.
    pub fn remote(config: &Config<'_>, git: &Git) -> Result<Sourced<String>, ForgeContextError> {
        let declared = config.ports.git.remote.trim();
        if !declared.is_empty() {
            return Ok(Sourced {
                value: declared.to_owned(),
                source: port_source(config.ports, "git.remote", "remote"),
            });
        }
        if let Some((branch, remote)) = git.upstream_remote() {
            return Ok(Sourced {
                value: remote,
                source: Source {
                    layer: Layer::Detected,
                    origin: Some(format!("branch.{branch}.remote")),
                },
            });
        }
        let remotes = git.remotes();
        if remotes.len() == 1 {
            return Ok(Sourced {
                value: remotes[0].clone(),
                source: Source {
                    layer: Layer::Detected,
                    origin: Some("the sole Git remote".to_owned()),
                },
            });
        }
        let close = remotes
            .first()
            .map(|first| provenant_core::close_matches(first, remotes.iter().map(String::as_str)))
            .unwrap_or_default()
            .into_iter()
            .map(str::to_owned)
            .collect();
        Err(ForgeContextError::NoRemote { remotes, close })
    }

    /// The remote, or `None` when the checkout has no remote at all. A
    /// remote-less checkout is an absence, never an empty name: Git crashes on
    /// an empty `branch.<name>.remote`, and `refs/remotes//` names nothing.
    /// An ambiguous set of remotes is still a refusal.
    /// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    pub fn remote_if_any(
        config: &Config<'_>,
        git: &Git,
    ) -> Result<Option<Sourced<String>>, ForgeContextError> {
        match Self::remote(config, git) {
            Ok(remote) => Ok(Some(remote)),
            Err(ForgeContextError::NoRemote { remotes, .. }) if remotes.is_empty() => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn resolve_default_branch(
        config: &Config<'_>,
        git: &Git,
        remote: &str,
    ) -> Result<Sourced<String>, ForgeContextError> {
        if let Some(declared) = &config.default_branch {
            return Ok(declared.clone());
        }
        if let Some(branch) = git.remote_head(remote) {
            return Ok(Sourced {
                value: branch,
                source: Source {
                    layer: Layer::Detected,
                    origin: Some(format!("refs/remotes/{remote}/HEAD")),
                },
            });
        }
        if let Some(branch) = config.forge.default_branch(remote) {
            return Ok(Sourced {
                value: branch,
                source: Source {
                    layer: Layer::Detected,
                    origin: Some("forge API".to_owned()),
                },
            });
        }
        let mut branches = git.local_branches();
        branches.sort();
        let close = branches
            .first()
            .map(|first| provenant_core::close_matches(first, branches.iter().map(String::as_str)))
            .unwrap_or_default()
            .into_iter()
            .map(str::to_owned)
            .collect();
        Err(ForgeContextError::NoDefaultBranch {
            remote: remote.to_owned(),
            branches,
            close,
        })
    }

    fn resolve_protected(config: &Config<'_>, default_branch: &str) -> Sourced<Vec<String>> {
        let declared: Vec<String> = config
            .ports
            .git
            .protected
            .iter()
            .map(|branch| short_branch(branch).to_owned())
            .collect();
        if !declared.is_empty() {
            let mut value: Vec<String> = declared;
            if !value.iter().any(|branch| branch == default_branch) {
                value.push(default_branch.to_owned());
            }
            value.sort();
            value.dedup();
            return Sourced {
                value,
                source: port_source(config.ports, "git.protected", "protected"),
            };
        }
        let mut set = BTreeSet::new();
        set.insert(default_branch.to_owned());
        set.extend(
            config
                .forge
                .protected_branches()
                .into_iter()
                .map(|branch| short_branch(&branch).to_owned()),
        );
        Sourced {
            value: set.into_iter().collect(),
            source: Source {
                layer: Layer::Detected,
                origin: Some("default branch plus forge-protected branches".to_owned()),
            },
        }
    }
}

/// Shorten a fully qualified branch name to the name operators declare.
///
/// A local branch's whole remainder is its name (`refs/heads/release/1.0` is
/// `release/1.0`). Only a remote-tracking ref drops the remote's name before
/// the branch (`refs/remotes/origin/main` is `main`). Splitting the already
/// stripped local remainder would truncate nested branch names and leave
/// single-segment ones qualified.
fn short_branch(branch: &str) -> &str {
    if let Some(rest) = branch.strip_prefix("refs/heads/") {
        rest
    } else if let Some(rest) = branch.strip_prefix("refs/remotes/") {
        rest.split_once('/').map_or(rest, |(_, name)| name)
    } else {
        branch
    }
}

/// The layer of a declared port leaf, with an origin that names the file and
/// key so an explanation can show exactly where the value came from.
fn port_source(ports: &PortTable, leaf: &str, key: &str) -> Source {
    // The layer walk records each declaration's exact file (or builtin) and
    // dotted key. Read the winning candidate's own source so a host declaration
    // and a host-project binding declaration remain distinguishable: the old
    // reconstruction named both `ports.json#git.remote`, which hid the file
    // that actually supplied the value.
    if let Some(winner) = ports.sourced_leaf(leaf).last() {
        return winner.source.clone();
    }
    let layer = ports
        .provenance()
        .get(leaf)
        .copied()
        .unwrap_or(ports.git.source);
    Source {
        layer,
        origin: Some(format!("{}#git.{key}", layer.description())),
    }
}

/// Rank a policy's contributing sources to name the layer that set it.
fn policy_layer(policy: &Policy) -> Layer {
    fn rank(source: PolicySource) -> u8 {
        match source {
            PolicySource::CompiledDefault => 0,
            PolicySource::Host => 1,
            PolicySource::Project => 2,
            PolicySource::Dispatch => 3,
        }
    }
    policy
        .rules
        .iter()
        .map(|rule| rule.source)
        .max_by_key(|source| rank(*source))
        .map(|source| match source {
            PolicySource::CompiledDefault => Layer::Default,
            PolicySource::Host => Layer::Host,
            PolicySource::Project => Layer::Project,
            PolicySource::Dispatch => Layer::Route,
        })
        .unwrap_or(Layer::Default)
}

fn sourced_policy(policy: Policy) -> Sourced<Policy> {
    let layer = policy_layer(&policy);
    Sourced {
        value: policy,
        source: Source {
            layer,
            origin: Some("owner operation policy".to_owned()),
        },
    }
}

/// The owner's operation policy for one forge construction.
///
/// The policy is read from the owner's own config and already carries its
/// layer. Resolving a forge context cannot change it — the context returns the
/// policy it was given — so this reads it directly. A checkout with no usable
/// Git metadata therefore does not print a spurious warning on every forge
/// construction. Callers needing the remote, default branch or protected set
/// resolve the context themselves through [`resolve_for`].
pub fn operation_policy(
    state: &Path,
    project: &Path,
    _ports: &PortTable,
) -> std::sync::Arc<Policy> {
    let project_id = crate::ports::project_key_for(project).unwrap_or_default();
    std::sync::Arc::new(crate::policy::read(state, &project_id, &Default::default()))
}

/// Resolve the one forge context from the full port table: host declarations
/// and project declarations together, never just the project file. Every
/// consumer of the remote, default branch or protected set goes through this
/// so a declaration in either layer reaches all of them.
pub fn resolve_for(project: &Path, ports: &PortTable) -> Result<ForgeContext, ForgeContextError> {
    resolve_in(&Git::new(project)?, ports)
}

/// Resolve the one forge context from Git metadata the caller already bound,
/// such as a bound bare repository's own directory ([`Git::bare`]).
pub fn resolve_in(git: &Git, ports: &PortTable) -> Result<ForgeContext, ForgeContextError> {
    let config = Config::new(ports, Policy::default());
    ForgeContext::resolve(&config, git)
}

/// A bounded, read-only view of a checkout's Git metadata. It never runs a
/// process and never changes the tree, so resolving a context is safe anywhere.
#[derive(Clone, Debug)]
pub struct Git {
    git_dir: PathBuf,
    common_dir: PathBuf,
}

const MAX_METADATA_BYTES: u64 = 64 * 1024;

impl Git {
    /// Bind to the checkout containing `path`, found the way Git discovers
    /// it: the nearest ancestor holding `.git`, so a launch from a
    /// subdirectory reads the same metadata as one from the toplevel. A linked
    /// worktree's `.git` file is followed through its `gitdir:` pointer and its
    /// `commondir`, the same rules Git uses.
    pub fn new(path: &Path) -> Result<Self, ForgeContextError> {
        let Some(project) = path.ancestors().find(|dir| dir.join(".git").exists()) else {
            return Err(ForgeContextError::Io {
                path: path.join(".git"),
                source: io::Error::new(io::ErrorKind::NotFound, "no Git metadata"),
            });
        };
        let dot = project.join(".git");
        if dot.is_dir() {
            return Ok(Self {
                git_dir: dot.clone(),
                common_dir: dot,
            });
        }
        if dot.is_file() {
            let text = read_bounded(&dot)?;
            let pointer = text.strip_prefix("gitdir:").map(str::trim).ok_or_else(|| {
                ForgeContextError::Io {
                    path: dot.clone(),
                    source: io::Error::new(io::ErrorKind::InvalidData, "invalid .git pointer"),
                }
            })?;
            let git_dir = resolve_path(project, pointer);
            let common_dir = read_bounded(&git_dir.join("commondir"))
                .map(|text| resolve_path(&git_dir, text.trim()))
                .unwrap_or_else(|_| git_dir.clone());
            return Ok(Self {
                git_dir,
                common_dir,
            });
        }
        Err(ForgeContextError::Io {
            path: dot,
            source: io::Error::new(io::ErrorKind::NotFound, "no Git metadata"),
        })
    }

    /// Bind to a bare repository's own Git directory. A bound bare project has
    /// no checkout: its config, refs and remote HEAD live in that directory.
    pub fn bare(common: &Path) -> Result<Self, ForgeContextError> {
        if !common.join("HEAD").is_file() {
            return Err(ForgeContextError::Io {
                path: common.join("HEAD"),
                source: io::Error::new(io::ErrorKind::NotFound, "no Git metadata"),
            });
        }
        Ok(Self {
            git_dir: common.to_path_buf(),
            common_dir: common.to_path_buf(),
        })
    }

    /// The remotes visible in the checkout: those declared in Git config plus
    /// those with a remote-tracking ref. A GC fixture or a recovered checkout
    /// can keep tracking refs without a config section; both are remotes seen.
    pub fn remotes(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        if let Some(text) = self.config_text() {
            for (name, _) in section_entries(&text, "remote") {
                seen.insert(name);
            }
        }
        for base in [&self.common_dir, &self.git_dir] {
            let root = base.join("refs/remotes");
            let Ok(entries) = std::fs::read_dir(root) else {
                continue;
            };
            for entry in entries.flatten() {
                if entry.path().is_dir()
                    && let Some(name) = entry.file_name().to_str()
                {
                    seen.insert(name.to_owned());
                }
            }
        }
        seen.into_iter().collect()
    }

    /// The current branch and the remote its upstream is configured on.
    pub fn upstream_remote(&self) -> Option<(String, String)> {
        let head = read_bounded(&self.git_dir.join("HEAD")).ok()?;
        let branch = head.strip_prefix("ref: refs/heads/")?.trim().to_owned();
        let text = self.config_text()?;
        section_entries(&text, "branch")
            .into_iter()
            .find(|(name, _)| name == &branch)
            .and_then(|(_, values)| values.get("remote").cloned())
            .filter(|remote| !remote.trim().is_empty())
            .map(|remote| (branch, remote))
    }

    /// The branch the remote's `HEAD` symref names, when the checkout has one.
    /// Both a local branch target and a remote-tracking target are accepted;
    /// fixtures and recovered checkouts use the latter.
    pub fn remote_head(&self, remote: &str) -> Option<String> {
        let relative = format!("refs/remotes/{remote}/HEAD");
        let tracking = format!("refs/remotes/{remote}/");
        for base in [&self.common_dir, &self.git_dir] {
            if let Ok(text) = read_bounded(&base.join(&relative))
                && let Some(target) = text.strip_prefix("ref: ")
            {
                let target = target.trim();
                if let Some(branch) = target.strip_prefix("refs/heads/") {
                    return Some(branch.to_owned());
                }
                if let Some(branch) = target.strip_prefix(tracking.as_str()) {
                    return Some(branch.to_owned());
                }
            }
        }
        None
    }

    /// The local branches present in the checkout, for a refusal that lists
    /// what it saw instead of guessing a default.
    pub fn local_branches(&self) -> Vec<String> {
        let mut branches = BTreeSet::new();
        for base in [&self.common_dir, &self.git_dir] {
            collect_branches(
                &base.join("refs/heads"),
                &base.join("refs/heads"),
                &mut branches,
            );
        }
        branches.into_iter().collect()
    }

    fn config_text(&self) -> Option<String> {
        read_bounded(&self.common_dir.join("config")).ok()
    }
}

fn resolve_path(base: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

fn read_bounded(path: &Path) -> Result<String, ForgeContextError> {
    use io::Read;
    let file = std::fs::File::open(path).map_err(|source| ForgeContextError::Io {
        path: path.to_owned(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| ForgeContextError::Io {
            path: path.to_owned(),
            source,
        })?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(ForgeContextError::Io {
            path: path.to_owned(),
            source: io::Error::new(io::ErrorKind::InvalidData, "Git metadata too large"),
        });
    }
    String::from_utf8(bytes).map_err(|_| ForgeContextError::Io {
        path: path.to_owned(),
        source: io::Error::new(io::ErrorKind::InvalidData, "Git metadata is not UTF-8"),
    })
}

/// Parse one family of `[<kind> "<name>"]` sections into name/key/value triples.
/// Git config values here are simple names and URLs; a bounded line parse is
/// enough and keeps detection independent of an external Git process.
fn section_entries(text: &str, kind: &str) -> Vec<(String, BTreeMap<String, String>)> {
    let header = format!("[{kind} \"");
    let mut out = Vec::new();
    let mut current: Option<(String, BTreeMap<String, String>)> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(header.as_str())
            && let Some((name, _)) = rest.split_once("\"]")
        {
            if let Some(entry) = current.take() {
                out.push(entry);
            }
            current = Some((name.to_owned(), BTreeMap::new()));
        } else if line.starts_with('[') {
            if let Some(entry) = current.take() {
                out.push(entry);
            }
        } else if let Some((key, value)) = line.split_once('=')
            && let Some((_, values)) = current.as_mut()
        {
            values.insert(key.trim().to_owned(), unquote(value.trim()));
        }
    }
    if let Some(entry) = current {
        out.push(entry);
    }
    out
}

fn unquote(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
        .to_owned()
}

fn collect_branches(dir: &Path, root: &Path, out: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_branches(&path, root, out);
        } else if let Ok(relative) = path.strip_prefix(root)
            && let Some(name) = relative.to_str()
        {
            out.insert(name.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_records::policy::Strength;

    struct Fixture {
        root: tempfile::TempDir,
    }

    impl Fixture {
        fn new(config: &str) -> Self {
            let root = tempfile::tempdir().expect("temp dir");
            let git = root.path().join(".git");
            std::fs::create_dir_all(git.join("refs/heads")).expect("refs");
            std::fs::create_dir_all(git.join("refs/remotes")).expect("remotes");
            std::fs::write(git.join("config"), config).expect("config");
            std::fs::write(git.join("HEAD"), "ref: refs/heads/topic\n").expect("HEAD");
            Self { root }
        }
        fn git(&self) -> Git {
            Git::new(self.root.path()).expect("git")
        }
        fn set_remote_head(&self, remote: &str, branch: &str) {
            let path = self
                .root
                .path()
                .join(".git/refs/remotes")
                .join(remote)
                .join("HEAD");
            std::fs::create_dir_all(path.parent().expect("parent")).expect("parent");
            std::fs::write(path, format!("ref: refs/heads/{branch}\n")).expect("remote HEAD");
        }
    }

    struct FakeForge {
        branch: Option<&'static str>,
        protected: Vec<&'static str>,
    }
    impl ForgeProbe for FakeForge {
        fn default_branch(&self, _remote: &str) -> Option<String> {
            self.branch.map(str::to_owned)
        }
        fn protected_branches(&self) -> Vec<String> {
            self.protected
                .iter()
                .map(|name| (*name).to_owned())
                .collect()
        }
    }

    fn context(fixture: &Fixture, ports: &PortTable) -> Result<ForgeContext, ForgeContextError> {
        let config = Config::new(ports, Policy::default());
        ForgeContext::resolve(&config, &fixture.git())
    }

    /// A bound bare repository has no checkout; its own directory supplies the
    /// remote and the remote HEAD, so GC can resolve the protected set.
    /// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    #[test]
    fn a_bare_repository_resolves_from_its_own_directory() {
        let bare = tempfile::tempdir().expect("temp dir");
        let dir = bare.path();
        std::fs::write(dir.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
        std::fs::write(dir.join("config"), "[core]\n\tbare = true\n").expect("config");
        std::fs::create_dir_all(dir.join("refs/remotes/origin")).expect("refs");
        std::fs::write(
            dir.join("refs/remotes/origin/HEAD"),
            "ref: refs/heads/main\n",
        )
        .expect("remote HEAD");
        let git = Git::bare(dir).expect("bare binds");
        let context = resolve_in(&git, &PortTable::builtin()).expect("resolve");
        assert_eq!(context.remote.value, "origin");
        assert_eq!(context.default_branch.value, "main");
        let empty = tempfile::tempdir().expect("temp dir");
        assert!(Git::bare(empty.path()).is_err());
    }

    /// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    #[test]
    fn a_subdirectory_binds_the_checkout_that_contains_it() {
        let fixture = Fixture::new("[remote \"upstream\"]\n");
        let nested = fixture.root.path().join("tools/front-door");
        std::fs::create_dir_all(&nested).expect("nested");
        let from_nested = Git::new(&nested).expect("discovered from a subdirectory");
        assert_eq!(from_nested.remotes(), fixture.git().remotes());
        let outside = tempfile::tempdir().expect("temp dir");
        assert!(Git::new(outside.path()).is_err());
    }

    /// The nearest `.git` wins: a checkout nested inside another binds its own
    /// metadata, from the nested root and from a directory beneath it.
    #[test]
    fn a_nested_repository_binds_the_nearest_checkout() {
        let outer = Fixture::new("[remote \"outer\"]\n");
        let nested = outer.root.path().join("nested");
        let git = nested.join(".git");
        std::fs::create_dir_all(git.join("refs/remotes")).expect("nested metadata");
        std::fs::write(git.join("config"), "[remote \"inner\"]\n").expect("nested config");
        std::fs::write(git.join("HEAD"), "ref: refs/heads/topic\n").expect("nested HEAD");
        assert_eq!(
            Git::new(&nested).expect("nearest checkout").remotes(),
            ["inner"]
        );
        let deeper = nested.join("src/deep");
        std::fs::create_dir_all(&deeper).expect("deep directory");
        assert_eq!(
            Git::new(&deeper)
                .expect("nearest from a subdirectory")
                .remotes(),
            ["inner"]
        );
        assert_eq!(outer.git().remotes(), ["outer"]);
    }

    /// A linked worktree's `.git` is a `gitdir:` pointer. Read from a
    /// subdirectory, it must follow the pointer and its `commondir` to the
    /// shared config and refs.
    #[test]
    fn a_linked_worktree_subdirectory_follows_gitdir_and_commondir() {
        let common = Fixture::new("[remote \"upstream\"]\n");
        common.set_remote_head("upstream", "main");
        let worktree = tempfile::tempdir().expect("worktree");
        let private = worktree.path().join("private-gitdir");
        std::fs::create_dir_all(&private).expect("private gitdir");
        std::fs::write(private.join("HEAD"), "ref: refs/heads/topic\n").expect("HEAD");
        std::fs::write(
            private.join("commondir"),
            format!("{}\n", common.root.path().join(".git").display()),
        )
        .expect("commondir");
        std::fs::write(
            worktree.path().join(".git"),
            format!("gitdir: {}\n", private.display()),
        )
        .expect("gitdir pointer");
        let subdir = worktree.path().join("crates/config/src");
        std::fs::create_dir_all(&subdir).expect("subdir");

        let from_subdir = Git::new(&subdir).expect("worktree from a subdirectory");
        assert_eq!(from_subdir.remotes(), common.git().remotes());
        assert_eq!(from_subdir.remote_head("upstream").as_deref(), Some("main"));
    }

    /// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    #[test]
    fn a_sole_remote_is_detected_with_its_source() {
        let fixture = Fixture::new("[remote \"upstream\"]\n\turl = git@example.com:up.git\n");
        fixture.set_remote_head("upstream", "main");
        let result = context(&fixture, &PortTable::builtin()).expect("resolve");
        assert_eq!(result.remote.value, "upstream");
        assert_eq!(result.remote.source.layer, Layer::Detected);
    }

    /// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    #[test]
    fn two_remotes_follow_the_current_branch_upstream() {
        let fixture = Fixture::new(
            "[remote \"origin\"]\n\turl = one\n[remote \"upstream\"]\n\turl = two\n\
             [branch \"topic\"]\n\tremote = upstream\n",
        );
        fixture.set_remote_head("upstream", "main");
        let result = context(&fixture, &PortTable::builtin()).expect("resolve");
        assert_eq!(result.remote.value, "upstream");
        assert_eq!(
            result.remote.source.origin.as_deref(),
            Some("branch.topic.remote")
        );
    }

    /// A checkout with no remote at all is an absence, never an empty name.
    /// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    #[test]
    fn an_absent_remote_is_none_and_never_an_empty_name() {
        let fixture = Fixture::new("");
        let ports = PortTable::builtin();
        let config = Config::new(&ports, Policy::default());
        assert_eq!(
            ForgeContext::remote_if_any(&config, &fixture.git()).expect("absence"),
            None
        );
    }

    /// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    #[test]
    fn no_remote_is_a_refusal_that_lists_what_was_seen() {
        let fixture = Fixture::new("[remote \"origin\"]\n[remote \"upstream\"]\n");
        let error = context(&fixture, &PortTable::builtin()).expect_err("refusal");
        match error {
            ForgeContextError::NoRemote { remotes, close } => {
                assert_eq!(remotes, ["origin", "upstream"]);
                assert!(!close.is_empty());
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    /// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    #[test]
    fn a_declared_remote_overrides_detection_with_its_source() {
        let fixture = Fixture::new("[remote \"upstream\"]\n");
        let mut ports = PortTable::builtin();
        ports.git.remote = "pinned".to_owned();
        ports.git.source = Layer::Project;
        fixture.set_remote_head("pinned", "main");
        let result = context(&fixture, &ports).expect("resolve");
        assert_eq!(result.remote.value, "pinned");
        assert_eq!(result.remote.source.layer, Layer::Project);
        assert!(
            result
                .remote
                .source
                .origin
                .as_deref()
                .is_some_and(|origin| origin.contains("git.remote"))
        );
    }

    /// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    #[test]
    fn the_remote_head_names_the_default_branch() {
        let fixture = Fixture::new("[remote \"upstream\"]\n");
        fixture.set_remote_head("upstream", "trunk");
        let result = context(&fixture, &PortTable::builtin()).expect("resolve");
        assert_eq!(result.default_branch.value, "trunk");
        assert_eq!(
            result.default_branch.source.origin.as_deref(),
            Some("refs/remotes/upstream/HEAD")
        );
        assert!(result.is_protected("trunk"));
    }

    /// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    #[test]
    fn a_declared_default_branch_wins_ahead_of_head_and_forge() {
        let fixture = Fixture::new("[remote \"upstream\"]\n");
        fixture.set_remote_head("upstream", "trunk");
        let forge = FakeForge {
            branch: Some("develop"),
            protected: vec!["release"],
        };
        let mut ports = PortTable::builtin();
        ports.git.default_branch = "declared".to_owned();
        let mut config = Config::new(&ports, Policy::default());
        config.forge = &forge;
        let result = ForgeContext::resolve(&config, &fixture.git()).expect("resolve");
        assert_eq!(result.default_branch.value, "declared");
        assert!(
            result
                .default_branch
                .source
                .origin
                .as_deref()
                .is_some_and(|origin| origin.contains("git.default_branch")),
            "{:?}",
            result.default_branch.source
        );
        assert!(result.is_protected("declared"));
    }

    // Spec: docs/specs/state.md#every-setting-reports-its-layer
    #[test]
    fn declared_git_ports_report_the_exact_file_that_supplied_them() {
        let temp = tempfile::tempdir().expect("temp dir");
        let host = temp.path().join("host");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&host).expect("host");
        std::fs::create_dir_all(project.join(".git")).expect("git marker");
        let binding = crate::host_project_dir(&host, &project).expect("binding");
        std::fs::create_dir_all(&binding).expect("binding dir");
        std::fs::write(
            host.join("ports.json"),
            r#"{"git":{"remote":"host-remote"}}"#,
        )
        .expect("host ports");
        std::fs::write(
            binding.join("ports.json"),
            r#"{"git":{"remote":"binding-remote"}}"#,
        )
        .expect("binding ports");

        let fixture = Fixture::new("[remote \"upstream\"]\n");
        let ports = PortTable::read_for(Some(&project), &host).expect("ports");
        let config = Config::new(&ports, Policy::default());
        let winning = ForgeContext::remote(&config, &fixture.git()).expect("remote");
        assert_eq!(winning.value, "binding-remote");
        assert_eq!(winning.source.layer, Layer::HostProject);
        let binding_origin = winning.source.origin.as_deref().expect("origin");
        assert_eq!(
            binding_origin,
            format!("{}#git.remote", binding.join("ports.json").display())
        );

        // The same declaration at the host layer names the host file instead.
        std::fs::remove_file(binding.join("ports.json")).expect("remove binding");
        let ports = PortTable::read_for(Some(&project), &host).expect("ports");
        let config = Config::new(&ports, Policy::default());
        let winning = ForgeContext::remote(&config, &fixture.git()).expect("remote");
        assert_eq!(winning.value, "host-remote");
        assert_eq!(winning.source.layer, Layer::Host);
        let host_origin = winning.source.origin.as_deref().expect("origin");
        assert_eq!(
            host_origin,
            format!("{}#git.remote", host.join("ports.json").display())
        );
        assert_ne!(host_origin, binding_origin);
    }

    /// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    #[test]
    fn with_no_head_symref_the_forge_supplies_the_default_and_protection() {
        let fixture = Fixture::new("[remote \"upstream\"]\n");
        let forge = FakeForge {
            branch: Some("develop"),
            protected: vec!["release"],
        };
        let ports = PortTable::builtin();
        let config = Config {
            ports: &ports,
            policy: sourced_policy(Policy::default()),
            default_branch: None,
            forge: &forge,
        };
        let result = ForgeContext::resolve(&config, &fixture.git()).expect("resolve");
        assert_eq!(result.default_branch.value, "develop");
        assert_eq!(
            result.default_branch.source.origin.as_deref(),
            Some("forge API")
        );
        assert_eq!(result.protected.value, ["develop", "release"]);
    }

    /// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    #[test]
    fn declared_protected_branches_override_detection() {
        let fixture = Fixture::new("[remote \"upstream\"]\n");
        // A resolution needs a default branch; the declared protected branch is
        // shortened while the default branch stays protected automatically.
        fixture.set_remote_head("upstream", "main");
        let mut ports = PortTable::builtin();
        ports.git.protected = vec!["refs/heads/released".to_owned()];
        ports.git.source = Layer::Project;
        let result = context(&fixture, &ports).expect("resolve");
        assert_eq!(result.protected.value, ["main", "released"]);
        assert!(result.is_protected("released"));
        assert!(result.is_protected("refs/heads/released"));
    }

    #[test]
    fn short_branch_keeps_nested_local_branches_and_drops_only_remote_names() {
        assert_eq!(short_branch("refs/heads/main"), "main");
        assert_eq!(short_branch("refs/heads/release/1.0"), "release/1.0");
        assert_eq!(short_branch("refs/remotes/origin/main"), "main");
        assert_eq!(short_branch("release/1.0"), "release/1.0");
    }

    #[test]
    fn a_nested_protected_branch_is_shortened_without_truncation() {
        let fixture = Fixture::new("[remote \"upstream\"]\n");
        fixture.set_remote_head("upstream", "main");
        let mut ports = PortTable::builtin();
        ports.git.protected = vec!["refs/heads/release/1.0".to_owned()];
        ports.git.source = Layer::Project;
        let result = context(&fixture, &ports).expect("resolve");
        assert_eq!(result.protected.value, ["main", "release/1.0"]);
        assert!(result.is_protected("release/1.0"));
        assert!(result.is_protected("refs/heads/release/1.0"));
    }

    /// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    #[test]
    fn a_configured_policy_reaches_the_context_with_its_source() {
        let fixture = Fixture::new("[remote \"upstream\"]\n");
        let mut rules = BTreeMap::new();
        rules.insert("PORT_COMMAND_EVIDENCE".to_owned(), Strength::Refuse);
        let mut settings = crate::policy::Settings::default();
        settings
            .projects
            .insert("project".to_owned(), crate::policy::Project { rules });
        let policy = settings.resolve("project", &BTreeMap::new());
        let ports = PortTable::builtin();
        fixture.set_remote_head("upstream", "main");
        let config = Config::new(&ports, policy);
        let result = ForgeContext::resolve(&config, &fixture.git()).expect("resolve");
        assert_eq!(result.policy.source.layer, Layer::Project);
        // The compiled policy table owns the rule order; find the configured
        // rule by code rather than pinning its position in that table.
        let effective = result.policy.value.rule("PORT_COMMAND_EVIDENCE");
        assert_eq!(effective.source, provenant_records::policy::Source::Project);
        assert_eq!(effective.rule.strength, Strength::Refuse);
    }

    #[test]
    fn operation_policy_reads_the_owner_policy_without_resolving_or_warning() {
        let root = tempfile::tempdir().expect("temp dir");
        let ports = PortTable::builtin();
        let (policy, warnings) = provenant_records::capture(|| {
            let policy = operation_policy(root.path(), &root.path().join("no-checkout"), &ports);
            (*policy).clone()
        });
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(policy, Policy::default());
    }

    /// Spec: docs/specs/run-git.md#one-resolved-forge-context-per-command
    #[test]
    fn the_resolved_policy_reaches_a_forge_adapter() {
        let fixture = Fixture::new("[remote \"upstream\"]\n");
        let mut settings = crate::policy::Settings::default();
        settings.projects.insert(
            "project".to_owned(),
            crate::policy::Project {
                rules: BTreeMap::from([("PORT_COMMAND_EVIDENCE".to_owned(), Strength::Refuse)]),
            },
        );
        let ports = PortTable::builtin();
        fixture.set_remote_head("upstream", "main");
        let config = Config::new(&ports, settings.resolve("project", &BTreeMap::new()));
        let context = ForgeContext::resolve(&config, &fixture.git()).expect("resolve");
        let adapter = crate::ports::Adapter::new(
            ports.forge.clone(),
            "forge",
            fixture.root.path(),
            fixture.root.path(),
            std::sync::Arc::new(context.policy.value.clone()),
        )
        .expect("adapter");
        assert_eq!(
            adapter.policy.rule("PORT_COMMAND_EVIDENCE").rule.strength,
            Strength::Refuse
        );
    }
}
