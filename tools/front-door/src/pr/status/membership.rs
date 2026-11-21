//! Integration-batch membership is a state report inferred from bounded Git
//! history: a member is in a batch when its head commit is carried by the
//! batch head. The rules are pure so a small history fixture can pin them;
//! the Git and forge reads at the bottom only supply facts and never refuse.

use crate::pr::git::GIT_TIMEOUT;
use crate::pr::landing::model::PendingAttempt;
use crate::pr::{execute_for, forge_execute, successful};
use crate::pr::{git, observer, status};
use provenant_cli::domains::{LandOverride, LandSettings};
use provenant_config::ports::Request;
use provenant_core::GitOid;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Duration,
};

/// One commit in the bounded history a projector supplies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Commit {
    pub(crate) oid: String,
    pub(crate) parents: Vec<String>,
    pub(crate) files: Vec<String>,
}

/// Bounded, read-only Git history for the heads being compared. `order` is the
/// projector's newest-first order (`git log`), which decides who last touched
/// a file; ancestry still comes from the parent graph.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct History {
    commits: BTreeMap<String, Commit>,
    order: Vec<String>,
}

impl History {
    pub(crate) fn new(commits: impl IntoIterator<Item = Commit>) -> Self {
        let mut order = Vec::new();
        let commits = commits
            .into_iter()
            .map(|commit| {
                order.push(commit.oid.clone());
                (commit.oid.clone(), commit)
            })
            .collect();
        Self { commits, order }
    }

    pub(crate) fn contains(&self, oid: &str) -> bool {
        self.commits.contains_key(oid)
    }

    /// Every object reachable from `head`, following parents. The head itself
    /// counts even when its object was not supplied.
    fn reachable(&self, head: &str) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut pending = vec![head.to_owned()];
        while let Some(oid) = pending.pop() {
            if !seen.insert(oid.clone()) {
                continue;
            }
            if let Some(commit) = self.commits.get(&oid) {
                for parent in &commit.parents {
                    pending.push(parent.clone());
                }
            }
        }
        seen
    }

    /// Reachable commits in the projector's newest-first order.
    fn walk(&self, head: &str) -> Vec<&Commit> {
        let reachable = self.reachable(head);
        self.order
            .iter()
            .filter_map(|oid| {
                reachable
                    .contains(oid)
                    .then(|| self.commits.get(oid))
                    .flatten()
            })
            .collect()
    }

    pub(crate) fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
        ancestor == descendant || self.reachable(descendant).contains(ancestor)
    }
}

/// A batch container: an open pull request or a remote landing candidate ref.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Container {
    pub(crate) number: Option<u64>,
    pub(crate) label: String,
    pub(crate) head: String,
    pub(crate) ci: String,
    /// The batch's target base, resolved to a commit when available. Shared
    /// history at or below this commit is pre-batch, not a carried member.
    pub(crate) base: Option<String>,
}

impl Container {
    fn key(&self) -> String {
        self.number
            .map_or_else(|| self.label.clone(), |n| format!("#{n}"))
    }
}

/// How a member stands relative to one batch. `Uncertain` is shared history
/// that is not proof of a batch claim: it is reported without suppressing CI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Standing {
    /// The member's current head is carried by the batch.
    HeadMerged,
    /// A former head the batch carried has been advanced past.
    HeadMoved,
    /// Shared commits newer than the base, but no carried head is proven.
    Uncertain,
}

/// Classify one member against one container using only evidence that the
/// batch carries the member's head: either the current head, or a merge parent
/// on the member's branch that the batch merged in. Shared ancestry alone is
/// reported as `Uncertain`, never as a batch claim. Without a base, a moved
/// head cannot be distinguished from a shared ancestor, so no moved claim is
/// made.
pub(crate) fn standing(
    history: &History,
    base: Option<&str>,
    member: &str,
    container: &str,
) -> Option<Standing> {
    if !history.contains(container) || !history.contains(member) {
        return None;
    }
    if history.is_ancestor(member, container) {
        return Some(Standing::HeadMerged);
    }
    if base.is_some_and(|base| carried_former(history, base, member, container).is_some()) {
        return Some(Standing::HeadMoved);
    }
    let older = base.map_or_else(BTreeSet::new, |base| history.reachable(base));
    let shared = history
        .reachable(member)
        .intersection(&history.reachable(container))
        .any(|oid| !older.contains(oid));
    shared.then_some(Standing::Uncertain)
}

/// The member head a batch actually merged in, when the member has since
/// advanced. A merge commit in the batch's history with a parent on the
/// member's branch that none of its other parents already carries is that
/// evidence; an arbitrary shared ancestor is not. The base keeps pre-batch
/// history from being mistaken for a carried member head.
fn carried_former(history: &History, base: &str, member: &str, container: &str) -> Option<String> {
    let older = history.reachable(base);
    for commit in history.walk(container) {
        if commit.parents.len() < 2 || history.is_ancestor(&commit.oid, member) {
            continue;
        }
        for (index, parent) in commit.parents.iter().enumerate() {
            if older.contains(parent) || !history.is_ancestor(parent, member) {
                continue;
            }
            let redundant = commit
                .parents
                .iter()
                .enumerate()
                .any(|(other, sibling)| index != other && history.is_ancestor(parent, sibling));
            if !redundant {
                return Some(parent.clone());
            }
        }
    }
    None
}

/// The member a file is attributed to in `--json` and human output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileOwner {
    pub(crate) path: String,
    pub(crate) member: u64,
}

/// Attribute each file changed after `base` to the newest batched commit that
/// touched it. Commits reachable from more than one member are shared history,
/// not a member's work, and are not attributed.
pub(crate) fn attribute_files(
    history: &History,
    base: &str,
    container: &str,
    members: &[(u64, String)],
) -> Vec<FileOwner> {
    let mut reach: BTreeMap<&str, BTreeSet<u64>> = BTreeMap::new();
    for (number, head) in members {
        for commit in history.walk(head) {
            reach.entry(&commit.oid).or_default().insert(*number);
        }
    }
    let older = history.reachable(base);
    let mut owners: BTreeMap<String, u64> = BTreeMap::new();
    for commit in history.walk(container) {
        if older.contains(&commit.oid) {
            continue;
        }
        let Some(members) = reach.get(commit.oid.as_str()) else {
            continue;
        };
        let unique: Vec<u64> = members.iter().copied().collect();
        let [member] = unique.as_slice() else {
            continue;
        };
        for path in &commit.files {
            owners.entry(path.clone()).or_insert(*member);
        }
    }
    owners
        .into_iter()
        .map(|(path, member)| FileOwner { path, member })
        .collect()
}

/// The link a member row carries to its batch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Link {
    pub(crate) number: Option<u64>,
    pub(crate) label: String,
    pub(crate) head: String,
    /// The batch's CI summary, read from the shared observer.
    pub(crate) ci: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) head_moved: bool,
}

impl Link {
    /// The suffix a member row adds to its status line.
    pub(crate) fn suffix(&self) -> String {
        let label = bounded(&self.label);
        let ci = bounded(&self.ci);
        if self.head_moved {
            format!(" in batch {label} head moved since batching; own CI superseded; batch ci={ci}")
        } else {
            format!(" in batch {label} (head merged); own CI superseded; batch ci={ci}")
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MemberRow {
    pub(crate) number: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) head_moved: bool,
}

/// What a batch row reports about the members it carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BatchDetail {
    pub(crate) members: Vec<MemberRow>,
    pub(crate) files: Vec<FileOwner>,
}

impl BatchDetail {
    /// The suffix a batch row adds to its status line.
    pub(crate) fn suffix(&self) -> String {
        let members: Vec<String> = self
            .members
            .iter()
            .map(|member| {
                format!(
                    "{}:{}",
                    member.number,
                    if member.head_moved { "moved" } else { "merged" }
                )
            })
            .collect();
        let files: Vec<String> = self
            .files
            .iter()
            .map(|file| format!("{}:{}", bounded(&file.path), file.member))
            .collect();
        format!(
            " batch_members={} batch_files={}",
            members.join(","),
            files.join(",")
        )
    }
}

/// Provider-controlled text is bounded before it reaches an output line.
fn bounded(value: &str) -> String {
    provenant_cli::bounded_text(
        &provenant_records::diagnostic::safe_text(value),
        status::CHECK_NAME_BYTES,
    )
}

/// Links for members and details for batches, keyed by member number and batch
/// label respectively. `uncertain` names shared ancestry that proves no batch
/// claim, so the member's own CI stays visible.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Report {
    pub(crate) links: BTreeMap<u64, Link>,
    pub(crate) details: BTreeMap<String, BatchDetail>,
    pub(crate) uncertain: BTreeMap<u64, String>,
}

/// Choose the batch that most recently carried each member and build its
/// per-file attribution. An exact containment outranks a moved head, which
/// outranks merely uncertain shared ancestry; an open pull request outranks a
/// landing candidate. Each container is compared against its own target base.
pub(crate) fn report(
    history: &History,
    members: &[(u64, String)],
    containers: &[Container],
) -> Report {
    let mut result = Report::default();
    let mut container_of: BTreeMap<u64, usize> = BTreeMap::new();
    for (number, head) in members {
        let mut best: Option<(u8, u8, u64, usize, Standing)> = None;
        for (index, container) in containers.iter().enumerate() {
            if container.number == Some(*number) {
                continue;
            }
            let Some(standing) =
                standing(history, container.base.as_deref(), head, &container.head)
            else {
                continue;
            };
            let rank = (
                match standing {
                    Standing::HeadMerged => 2,
                    Standing::HeadMoved => 1,
                    Standing::Uncertain => 0,
                },
                u8::from(container.number.is_some()),
                container.number.unwrap_or_default(),
                index,
            );
            if best
                .as_ref()
                .is_none_or(|prior| rank > (prior.0, prior.1, prior.2, prior.3))
            {
                best = Some((rank.0, rank.1, rank.2, rank.3, standing));
            }
        }
        if let Some((_, _, _, index, Standing::Uncertain)) = best {
            let label = bounded(&containers[index].key());
            result.uncertain.insert(
                *number,
                format!(
                    " batch={label}? shared history only; membership uncertain; own CI retained"
                ),
            );
            continue;
        }
        if let Some((_, _, _, index, standing)) = best {
            let container = &containers[index];
            container_of.insert(*number, index);
            result.links.insert(
                *number,
                Link {
                    number: container.number,
                    label: container.key(),
                    head: container.head.clone(),
                    ci: container.ci.clone(),
                    head_moved: standing == Standing::HeadMoved,
                },
            );
        }
    }
    for (index, container) in containers.iter().enumerate() {
        let carried: Vec<(u64, String)> = members
            .iter()
            .filter(|(number, _)| container_of.get(number) == Some(&index))
            .cloned()
            .collect();
        if carried.is_empty() {
            continue;
        }
        let rows = carried
            .iter()
            .map(|(number, _)| MemberRow {
                number: *number,
                head_moved: result.links.get(number).is_some_and(|link| link.head_moved),
            })
            .collect();
        let files = container.base.as_deref().map_or_else(Vec::new, |base| {
            attribute_files(history, base, &container.head, &carried)
        });
        result.details.insert(
            container.key(),
            BatchDetail {
                members: rows,
                files,
            },
        );
    }
    result
}

/// One open pull request from the forge list: a candidate member or batch.
struct Pull {
    number: u64,
    head: String,
    base: Option<String>,
}

/// Open pull requests are candidate batches and candidate members even when
/// this account did not author them. A missing or unreadable list leaves no
/// candidates; one bad head never blocks the others.
fn open_pulls(state: &Path, project: &Path) -> Vec<Pull> {
    #[derive(Deserialize)]
    struct Listed {
        number: u64,
        #[serde(rename = "headRefOid")]
        head: String,
        #[serde(rename = "baseRefName", default)]
        base: Option<String>,
    }
    let listed = forge_execute(
        state,
        project,
        &[
            "gh",
            "pr",
            "list",
            "--state",
            "open",
            "--limit",
            "100",
            "--json",
            "number,headRefOid,baseRefName",
        ],
        &Request::within(Duration::from_secs(60)),
        false,
        None,
    )
    .ok()
    .and_then(|out| successful(out).ok())
    .and_then(|bytes| serde_json::from_slice::<Vec<Listed>>(&bytes).ok());
    listed
        .into_iter()
        .flatten()
        .filter(|pull| GitOid::parse(&pull.head).is_ok())
        .map(|pull| Pull {
            number: pull.number,
            head: pull.head,
            base: pull.base.filter(|name| git::plain_ref(name)),
        })
        .collect()
}

/// Resolve one head on its own so an unfetched object marks only its own
/// evidence incomplete instead of failing a history read for every head.
fn resolve_commit(project: &Path, name: &str) -> Option<String> {
    if !git::plain_ref(name) {
        return None;
    }
    let spec = format!("{name}^{{commit}}");
    let bytes = execute_for(
        project,
        &["rev-parse", "--verify", "--end-of-options", &spec],
        GIT_TIMEOUT,
        false,
    )
    .ok()
    .and_then(|out| successful(out).ok())?;
    let oid = String::from_utf8(bytes).ok()?;
    let oid = oid.trim();
    GitOid::parse(oid).ok()?;
    Some(oid.to_owned())
}

/// Resolve a target branch name to the commit history is bounded by: a
/// tracking ref when present, otherwise a local ref of that name.
fn resolve_base(project: &Path, name: &str) -> Option<String> {
    if !git::plain_ref(name) {
        return None;
    }
    let tracking = format!("refs/remotes/{}/{name}", candidate_remote(project));
    resolve_commit(project, &tracking).or_else(|| resolve_commit(project, name))
}

/// The remote a landing candidate is published to, defaulting as landing does.
fn candidate_remote(project: &Path) -> String {
    let context = provenant_os::git_config::ConfigContext {
        operation: "read candidate branch remote",
        file: project.join(".git/config"),
        key: Some("branch tracking key"),
    };
    provenant_os::git_config::read(
        context,
        provenant_os::git_config::Read::Get("branch..remote"),
        |command| {
            let git = provenant_os::git::git_executable()
                .ok_or_else(|| "Git executable is unavailable".to_owned())?;
            let mut argv = vec![
                git.to_str()
                    .ok_or_else(|| "Git executable path is not UTF-8".to_owned())?
                    .to_owned(),
            ];
            argv.extend(
                command
                    .iter()
                    .map(|arg| {
                        arg.to_str()
                            .map(str::to_owned)
                            .ok_or_else(|| "Git config argument is not UTF-8".to_owned())
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()?,
            );
            execute_for(
                project,
                &argv.iter().map(String::as_str).collect::<Vec<_>>(),
                GIT_TIMEOUT,
                false,
            )
            .map_err(|error| error.to_string())
        },
    )
    .ok()
    .and_then(|out| successful(out).ok())
    .and_then(|bytes| String::from_utf8(bytes).ok())
    .map(|name| name.trim().to_owned())
    .filter(|name| git::plain_ref(name) && name != ".")
    .unwrap_or_else(|| "origin".to_owned())
}

/// The member a candidate ref was built for: `<prefix><number>-...`.
fn ref_member(name: &str, prefix: &str) -> Option<u64> {
    name.strip_prefix(prefix)?.split('-').next()?.parse().ok()
}

/// The effective candidate prefix: the project's landing declaration when it
/// overrides the bundled default.
fn candidate_prefix(project: &Path) -> String {
    if let Ok(Some(over)) = provenant_config::read_land::<LandOverride>(project)
        && let Some(prefix) = over.candidate_ref_prefix
        && git::plain_ref(&prefix)
    {
        return prefix;
    }
    LandSettings::default().candidate_ref_prefix
}

/// An in-flight landing publishes its candidate ref, commit and target base in
/// a pending record before it pushes. Those records are the authoritative
/// source for active candidates and their target base.
fn recorded_containers(state: &Path, project: &Path, repository: Option<&str>) -> Vec<Container> {
    let Ok(entries) = std::fs::read_dir(state.join("landing").join("pending")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let bytes = std::fs::read(entry.path()).ok()?;
            let attempt = serde_json::from_slice::<PendingAttempt>(&bytes).ok()?;
            if attempt.settled.is_some()
                || repository.is_some_and(|name| name != attempt.repository)
            {
                return None;
            }
            Some(Container {
                number: None,
                label: attempt.candidate.ref_name,
                head: attempt.candidate.commit,
                ci: String::new(),
                base: resolve_base(project, &attempt.candidate.base),
            })
        })
        .collect()
}

/// Landing pushes its candidate commit straight to a remote
/// `refs/heads/<prefix><number>-...` ref; it never creates that local branch.
/// Discover active candidates from the remote refs landing publishes, using
/// the effective prefix. The first member PR supplies the batch's target base
/// when no pending record carries it.
fn candidate_containers(
    project: &Path,
    state: &Path,
    repository: Option<&str>,
    pulls: &[Pull],
) -> Vec<Container> {
    let mut found = recorded_containers(state, project, repository);
    let prefix = candidate_prefix(project);
    if !git::plain_ref(&prefix) {
        return found;
    }
    let remote = candidate_remote(project);
    let pattern = format!("refs/heads/{prefix}*");
    let bytes = execute_for(
        project,
        &["ls-remote", "--heads", &remote, &pattern],
        GIT_TIMEOUT,
        false,
    )
    .ok()
    .and_then(|out| successful(out).ok());
    let Some(text) = bytes.and_then(|bytes| String::from_utf8(bytes).ok()) else {
        return found;
    };
    for line in text.lines().take(50) {
        let Some((head, full)) = line.split_once('\t') else {
            continue;
        };
        let Some(label) = full.strip_prefix("refs/heads/") else {
            continue;
        };
        if GitOid::parse(head).is_err() || found.iter().any(|c| c.head == head) {
            continue;
        }
        let base = ref_member(label, &prefix)
            .and_then(|number| pulls.iter().find(|pull| pull.number == number))
            .and_then(|pull| pull.base.as_deref())
            .and_then(|name| resolve_base(project, name));
        found.push(Container {
            label: label.to_owned(),
            head: head.to_owned(),
            number: None,
            ci: String::new(),
            base,
        });
    }
    found
}

/// Read commits for the resolved heads after each container's target base in
/// one bounded Git call. Every head was resolved on its own first, so a single
/// missing object cannot fail the whole read.
fn read_history(project: &Path, heads: &[String], bases: &[String]) -> Option<History> {
    if heads.is_empty() {
        return None;
    }
    let mut argv = vec![
        "log".to_owned(),
        "--format=COMMIT %H %P".to_owned(),
        "--name-only".to_owned(),
        "--no-renames".to_owned(),
    ];
    argv.extend(heads.iter().cloned());
    if !bases.is_empty() {
        argv.push("--not".to_owned());
        argv.extend(bases.iter().cloned());
    }
    argv.push("--".to_owned());
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    let bytes = execute_for(project, &args, GIT_TIMEOUT, false)
        .ok()
        .and_then(|out| successful(out).ok())?;
    let text = String::from_utf8(bytes).ok()?;
    let mut commits = Vec::new();
    let mut current: Option<(String, Vec<String>, Vec<String>)> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("COMMIT ") {
            if let Some((oid, parents, files)) = current.take() {
                commits.push(Commit {
                    oid,
                    parents,
                    files,
                });
            }
            let mut ids = rest.split_whitespace();
            let oid = ids.next()?.to_owned();
            current = Some((oid, ids.map(str::to_owned).collect(), Vec::new()));
        } else if !line.is_empty()
            && let Some((_, _, files)) = current.as_mut()
        {
            files.push(line.to_owned());
        }
    }
    if let Some((oid, parents, files)) = current {
        commits.push(Commit {
            oid,
            parents,
            files,
        });
    }
    Some(History::new(commits))
}

/// A container's CI summary, read by head from the shared observer. A read that
/// cannot be answered is labelled unavailable rather than assumed green.
fn container_ci(state: &Path, project: &Path, repository: &str, head: &str) -> String {
    let request = Request::within(Duration::from_secs(60));
    let Ok(mut watcher) = observer::Observer::open(state, project, repository, &request) else {
        return "checks unavailable".to_owned();
    };
    let Ok(checks) = watcher.checks(head, Duration::ZERO, &request) else {
        return "checks unavailable".to_owned();
    };
    let checks = checks
        .into_iter()
        .map(|check| status::Check {
            name: check.name,
            bucket: check.bucket,
        })
        .collect();
    status::summarise_ci(Some(checks), None).summary
}

/// Annotate status rows with the batch state they can prove. Every read is
/// best-effort: unavailable Git history or forge evidence leaves plain rows,
/// so this report never refuses or changes a landing decision.
pub(crate) fn annotate(project: &Path, state: &Path, lines: &mut [status::Line]) {
    let displayed: BTreeMap<u64, String> = lines
        .iter()
        .filter(|line| line.error.is_none() && line.state == "OPEN")
        .map(|line| (line.number, line.head.clone()))
        .collect();
    if displayed.is_empty() {
        return;
    }
    let pulls = open_pulls(state, project);
    let repository = status::batch::repository(state, project).ok();

    // Candidate members come from the authoritative open-PR list, independent
    // of the displayed rows: `pr status M` for a batch must still list its
    // members. The displayed head wins for a row that is shown.
    let mut candidates: BTreeMap<u64, String> = pulls
        .iter()
        .map(|pull| (pull.number, pull.head.clone()))
        .collect();
    for (number, head) in &displayed {
        candidates.entry(*number).or_insert_with(|| head.clone());
    }
    let members: Vec<(u64, String)> = candidates.into_iter().collect();

    let mut containers: Vec<Container> = pulls
        .iter()
        .map(|pull| Container {
            number: Some(pull.number),
            label: format!("#{}", pull.number),
            head: pull.head.clone(),
            ci: String::new(),
            base: pull
                .base
                .as_deref()
                .and_then(|name| resolve_base(project, name)),
        })
        .collect();
    containers.extend(candidate_containers(
        project,
        state,
        repository.as_deref(),
        &pulls,
    ));
    if containers.is_empty() {
        return;
    }

    // Resolve heads one at a time: one unfetched head marks only its own
    // evidence incomplete and leaves the other heads usable for detection.
    let mut heads = BTreeSet::new();
    let mut resolvable = BTreeSet::new();
    for container in &containers {
        if let Some(oid) = resolve_commit(project, &container.head) {
            heads.insert(oid);
            resolvable.insert(container.head.clone());
        }
    }
    for (_, head) in &members {
        if let Some(oid) = resolve_commit(project, head) {
            heads.insert(oid);
            resolvable.insert(head.clone());
        }
    }
    let bases: Vec<String> = containers
        .iter()
        .filter_map(|container| container.base.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let Some(history) = read_history(project, &heads.into_iter().collect::<Vec<_>>(), &bases)
    else {
        return;
    };
    let evaluable: Vec<(u64, String)> = members
        .into_iter()
        .filter(|(_, head)| resolvable.contains(head))
        .collect();
    let mut found = report(&history, &evaluable, &containers);
    // An unresolved head on a displayed member is incomplete evidence: report
    // it as uncertain, never as a batch claim.
    for (number, head) in &displayed {
        if !resolvable.contains(head) && !found.uncertain.contains_key(number) {
            found.uncertain.insert(
                *number,
                " batch evidence incomplete: head not available locally".to_owned(),
            );
        }
    }
    let labels: BTreeSet<String> = found
        .links
        .values()
        .map(|link| link.label.clone())
        .collect();
    if !labels.is_empty()
        && let Some(repository) = repository
    {
        for container in &mut containers {
            if labels.contains(&container.key()) {
                container.ci = container_ci(state, project, &repository, &container.head);
            }
        }
        for link in found.links.values_mut() {
            if let Some(container) = containers.iter().find(|c| c.key() == link.label) {
                link.ci = container.ci.clone();
            }
        }
    }
    for line in lines.iter_mut() {
        if displayed.contains_key(&line.number) {
            if let Some(link) = found.links.get(&line.number).cloned() {
                line.suppress_own_ci();
                line.batch = Some(link);
                line.batch_uncertain = None;
            } else if let Some(note) = found.uncertain.get(&line.number) {
                line.batch_uncertain = Some(note.clone());
            }
        }
        if let Some(detail) = found.details.get(&format!("#{}", line.number)).cloned() {
            line.batch_detail = Some(detail);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)] // The worktree driver includes this module without libtest.
    fn commit(oid: &str, parents: &[&str], files: &[&str]) -> Commit {
        Commit {
            oid: oid.into(),
            parents: parents.iter().map(|value| (*value).into()).collect(),
            files: files.iter().map(|value| (*value).into()).collect(),
        }
    }

    /// Base B; members 7 and 8 branch from it; batch PR 9 merges both heads.
    #[allow(dead_code)] // The worktree driver includes this module without libtest.
    fn fixture() -> (History, Vec<(u64, String)>, Vec<Container>) {
        let history = History::new([
            commit("batch", &["m7", "m8"], &["integration.txt"]),
            commit("m8", &["b"], &["src/b.rs", "shared.rs"]),
            commit("m7", &["b"], &["src/a.rs", "shared.rs"]),
            commit("b", &[], &["base.txt"]),
        ]);
        let members = vec![(7, "m7".into()), (8, "m8".into())];
        let containers = vec![Container {
            number: Some(9),
            label: "#9".into(),
            head: "batch".into(),
            ci: String::new(),
            base: Some("b".into()),
        }];
        (history, members, containers)
    }

    // Spec: docs/specs/cli.md#integration-batch-membership
    #[test]
    fn a_member_head_contained_in_an_open_batch_reads_as_superseded() {
        let (history, members, containers) = fixture();
        let found = report(&history, &members, &containers);
        for number in [7, 8] {
            let link = &found.links[&number];
            assert_eq!(link.label, "#9");
            assert_eq!(link.number, Some(9));
            assert!(!link.head_moved);
            assert!(
                link.suffix()
                    .contains("in batch #9 (head merged); own CI superseded")
            );
        }
        let detail = &found.details["#9"];
        assert_eq!(
            detail
                .members
                .iter()
                .map(|member| member.number)
                .collect::<Vec<_>>(),
            [7, 8]
        );
        assert!(
            detail
                .files
                .iter()
                .all(|file| file.member == 7 || file.member == 8)
        );
    }

    // Spec: docs/specs/cli.md#integration-batch-membership
    #[test]
    fn a_member_that_moved_since_batching_is_flagged() {
        let (_, mut members, containers) = fixture();
        // Member 7 pushed another commit after its head was merged into 9.
        let history = History::new([
            commit("batch", &["m7", "m8"], &["integration.txt"]),
            commit("m7-new", &["m7"], &["src/new.rs"]),
            commit("m8", &["b"], &["src/b.rs"]),
            commit("m7", &["b"], &["src/a.rs"]),
            commit("b", &[], &["base.txt"]),
        ]);
        members[0].1 = "m7-new".into();
        let found = report(&history, &members, &containers);
        let link = &found.links[&7];
        assert!(link.head_moved, "{link:?}");
        assert!(link.suffix().contains("head moved since batching"));
        assert!(!found.links[&8].head_moved);
        assert!(history.is_ancestor("m7", "batch"));
        assert!(!history.is_ancestor("m7-new", "batch"));
        // The batched commit is still attributed to its member.
        assert!(
            found.details["#9"]
                .files
                .iter()
                .any(|file| file.path == "src/a.rs" && file.member == 7)
        );
        assert!(
            !found.details["#9"]
                .files
                .iter()
                .any(|file| file.path == "src/new.rs")
        );
    }

    // Spec: docs/specs/cli.md#integration-batch-membership
    #[test]
    fn per_file_attribution_matches_git_history_in_a_fixture() {
        let history = History::new([
            // Newest first, as `git log` supplies it: member 8 touched the
            // contested file after member 7's two earlier commits.
            commit("batch", &["m7", "m8"], &["integration.txt"]),
            commit("m8", &["b"], &["src/b.rs", "contested.rs"]),
            commit("m7", &["m7-old"], &["src/a.rs"]),
            commit("m7-old", &["b"], &["src/a.rs", "contested.rs"]),
            commit("b", &[], &["base.txt", "shared.rs"]),
        ]);
        let files = attribute_files(
            &history,
            "b",
            "batch",
            &[(7, "m7".into()), (8, "m8".into())],
        );
        let owner = |path: &str| {
            files
                .iter()
                .find(|file| file.path == path)
                .map(|file| file.member)
        };
        assert_eq!(owner("src/a.rs"), Some(7));
        assert_eq!(owner("src/b.rs"), Some(8));
        assert_eq!(owner("contested.rs"), Some(8), "newest commit wins");
        assert_eq!(
            owner("base.txt"),
            None,
            "pre-batch history is not batched work"
        );
        // A commit reachable from two members is shared history, not a claim.
        let shared = History::new([
            commit("batch", &["m7", "m8"], &[]),
            commit("m8", &["both"], &[]),
            commit("m7", &["both"], &[]),
            commit("both", &["b"], &["shared.rs"]),
            commit("b", &[], &[]),
        ]);
        assert!(
            attribute_files(&shared, "b", "batch", &[(7, "m7".into()), (8, "m8".into())])
                .is_empty()
        );
    }

    #[test]
    fn an_unrelated_member_and_a_batch_itself_are_not_linked() {
        let (history, mut members, containers) = fixture();
        members.push((9, "batch".into()));
        let found = report(&history, &members, &containers);
        assert!(!found.links.contains_key(&9), "a PR is not its own batch");
        assert_eq!(found.links.len(), 2);
        let mut unrelated = containers;
        unrelated.push(Container {
            number: Some(10),
            label: "#10".into(),
            head: "b".into(),
            ci: String::new(),
            base: Some("b".into()),
        });
        let found = report(&history, &members, &unrelated);
        assert_eq!(
            found.links[&7].label, "#9",
            "the first containing batch wins"
        );
    }

    #[test]
    fn branch_containers_report_without_a_pull_request_number() {
        let (history, members, _) = fixture();
        let containers = [Container {
            number: None,
            label: "land/integration".into(),
            head: "batch".into(),
            ci: "checks=2/2 fail=0 pending=0".into(),
            base: Some("b".into()),
        }];
        let found = report(&history, &members, &containers);
        assert_eq!(found.links[&7].label, "land/integration");
        assert_eq!(found.links[&7].number, None);
        assert!(
            found.links[&7]
                .suffix()
                .contains("batch ci=checks=2/2 fail=0 pending=0")
        );
        assert!(found.details.contains_key("land/integration"));
    }

    #[test]
    fn shared_history_without_a_base_is_uncertain_and_never_claims_membership() {
        let (_, mut members, containers) = fixture();
        // A moved head is a new commit on top of the batched one; its shared
        // history with the batch proves no membership on its own.
        let history = History::new([
            commit("batch", &["m7", "m8"], &["integration.txt"]),
            commit("m7-new", &["m7"], &["src/new.rs"]),
            commit("m8", &["b"], &["src/b.rs", "shared.rs"]),
            commit("m7", &["b"], &["src/a.rs", "shared.rs"]),
            commit("b", &[], &["base.txt"]),
        ]);
        members[0].1 = "m7-new".into();
        assert_eq!(
            standing(&history, None, "m7-new", "batch"),
            Some(Standing::Uncertain)
        );
        assert_eq!(
            standing(&history, Some("b"), "m7-new", "batch"),
            Some(Standing::HeadMoved)
        );
        assert_eq!(
            standing(&history, None, "m7", "batch"),
            Some(Standing::HeadMerged)
        );
        let no_base = [Container {
            base: None,
            ..containers[0].clone()
        }];
        let found = report(&history, &members, &no_base);
        assert!(
            !found.links.contains_key(&7),
            "shared history is not a claim"
        );
        assert!(
            found
                .uncertain
                .get(&7)
                .is_some_and(|note| note.contains("#9"))
        );
    }

    #[test]
    fn the_batch_target_base_keeps_a_moved_members_former_head_visible() {
        // Member head A was merged into the batch, then advanced to A2. The
        // octopus merge base of A2 and the batch head is A, which would hide A
        // as pre-batch history; the batch's target base B must be used instead.
        let history = History::new([
            commit("batch", &["b", "a"], &["integration.txt"]),
            commit("a2", &["a"], &["src/new.rs"]),
            commit("a", &["b"], &["src/a.rs"]),
            commit("b", &[], &["base.txt"]),
        ]);
        assert_ne!(
            standing(&history, Some("a"), "a2", "batch"),
            Some(Standing::HeadMoved),
            "the member's former head cannot be its own base"
        );
        assert_eq!(
            standing(&history, Some("b"), "a2", "batch"),
            Some(Standing::HeadMoved)
        );
        let containers = [Container {
            number: Some(9),
            label: "#9".into(),
            head: "batch".into(),
            ci: String::new(),
            base: Some("b".into()),
        }];
        let found = report(&history, &[(7, "a2".into())], &containers);
        assert!(found.links[&7].head_moved, "{:?}", found.links);
        assert!(!found.uncertain.contains_key(&7));
    }

    #[test]
    fn siblings_sharing_a_feature_ancestor_are_not_a_batch() {
        let history = History::new([
            commit("s8", &["shared"], &["src/b.rs"]),
            commit("s7", &["shared"], &["src/a.rs"]),
            commit("shared", &["b"], &["feature.rs"]),
            commit("b", &[], &["base.txt"]),
        ]);
        let containers = [Container {
            number: Some(8),
            label: "#8".into(),
            head: "s8".into(),
            ci: String::new(),
            base: Some("b".into()),
        }];
        assert_eq!(
            standing(&history, Some("b"), "s7", "s8"),
            Some(Standing::Uncertain),
            "a shared feature ancestor is not evidence of batching"
        );
        let found = report(&history, &[(7, "s7".into())], &containers);
        assert!(!found.links.contains_key(&7));
        assert!(found.details.is_empty());
        assert!(
            found
                .uncertain
                .get(&7)
                .is_some_and(|note| note.contains("#8"))
        );
    }

    #[test]
    fn a_batch_row_lists_members_that_are_not_displayed_rows() {
        let (history, _, containers) = fixture();
        // Only #9 is displayed; members 7 and 8 come from candidate discovery,
        // never from the rows `pr status 9` shows.
        let candidates = [(7, "m7".into()), (8, "m8".into()), (9, "batch".into())];
        let found = report(&history, &candidates, &containers);
        assert_eq!(
            found.details["#9"]
                .members
                .iter()
                .map(|member| member.number)
                .collect::<Vec<_>>(),
            [7, 8]
        );
        assert!(
            !found.links.contains_key(&9),
            "a batch is not its own member"
        );
    }

    #[test]
    fn landing_candidate_refs_yield_the_member_number() {
        let name = "land/7-abc123-1f-2-3";
        assert_eq!(ref_member(name, "land/"), Some(7));
        assert_eq!(ref_member("queue-42-abc", "queue-"), Some(42));
        assert_eq!(ref_member("land/revert/7", "land/"), None);
    }
}
