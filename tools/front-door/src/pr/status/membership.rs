//! Integration-batch membership is a state report inferred from bounded Git
//! history: a member is in a batch when its head commit is carried by the
//! batch head. The rules are pure so a small history fixture can pin them;
//! the Git and forge reads at the bottom only supply facts and never refuse.

use crate::pr::git::GIT_TIMEOUT;
use crate::pr::{execute_for, forge_execute, successful};
use crate::pr::{git, observer, status};
use provenant_cli::domains::LandSettings;
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

/// A batch container: an open pull request or a local landing branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Container {
    pub(crate) number: Option<u64>,
    pub(crate) label: String,
    pub(crate) head: String,
    pub(crate) ci: String,
}

impl Container {
    fn key(&self) -> String {
        self.number
            .map_or_else(|| self.label.clone(), |n| format!("#{n}"))
    }
}

/// How a member stands relative to one batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Standing {
    HeadMerged,
    HeadMoved,
}

/// Classify one member against one container. Without a base, only the exact
/// containment of the current head is knowable: shared history cannot prove a
/// member moved, so no moved claim is made.
pub(crate) fn standing(
    history: &History,
    base: Option<&str>,
    member: &str,
    container: &str,
) -> Option<Standing> {
    if !history.contains(container) {
        return None;
    }
    if history.is_ancestor(member, container) {
        return Some(Standing::HeadMerged);
    }
    let base = base?;
    let prior = history.reachable(member);
    let carried = history.reachable(container);
    let older = history.reachable(base);
    (prior.intersection(&carried))
        .any(|oid| !older.contains(oid))
        .then_some(Standing::HeadMoved)
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
/// label respectively.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Report {
    pub(crate) links: BTreeMap<u64, Link>,
    pub(crate) details: BTreeMap<String, BatchDetail>,
}

/// Choose the batch that most recently carried each member and build its
/// per-file attribution. An exact containment outranks a moved head; an open
/// pull request outranks a landing branch.
pub(crate) fn report(
    history: &History,
    base: Option<&str>,
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
            let Some(standing) = standing(history, base, head, &container.head) else {
                continue;
            };
            let rank = (
                if standing == Standing::HeadMerged {
                    2
                } else {
                    1
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
        if let Some((_, _, _, index, standing)) = best {
            let container = &containers[index];
            container_of.insert(*number, index);
            result.links.insert(
                *number,
                Link {
                    number: container.number,
                    label: container.key(),
                    head: container.head.clone(),
                    ci: String::new(),
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
        let files = base.map_or_else(Vec::new, |base| {
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

/// Open pull requests are candidate batches even when this account did not
/// author them. A missing or unreadable list simply leaves no candidates.
fn open_containers(state: &Path, project: &Path) -> Vec<Container> {
    #[derive(Deserialize)]
    struct Listed {
        number: u64,
        #[serde(rename = "headRefOid")]
        head: String,
    }
    let bytes = forge_execute(
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
            "number,headRefOid",
        ],
        &Request::within(Duration::from_secs(60)),
        false,
        None,
    )
    .ok()
    .and_then(|out| successful(out).ok())
    .and_then(|bytes| serde_json::from_slice::<Vec<Listed>>(&bytes).ok());
    bytes
        .into_iter()
        .flatten()
        .filter(|pull| GitOid::parse(&pull.head).is_ok())
        .map(|pull| Container {
            number: Some(pull.number),
            label: format!("#{}", pull.number),
            head: pull.head,
            ci: String::new(),
        })
        .collect()
}

/// Local landing candidates are integration branches too. Only refs under the
/// bundled `candidate_ref_prefix` are considered; a project that overrides it
/// keeps its own CI and is not claimed here.
fn candidate_containers(project: &Path) -> Vec<Container> {
    let prefix = LandSettings::default().candidate_ref_prefix;
    if !git::plain_ref(&prefix) {
        return Vec::new();
    }
    let pattern = format!("refs/heads/{prefix}");
    let bytes = execute_for(
        project,
        &[
            "for-each-ref",
            "--format=%(refname:short) %(objectname)",
            &pattern,
        ],
        GIT_TIMEOUT,
        false,
    )
    .ok()
    .map(|out| out.stdout.bytes);
    let Some(text) = bytes.and_then(|bytes| String::from_utf8(bytes).ok()) else {
        return Vec::new();
    };
    text.lines()
        .take(50)
        .filter_map(|line| {
            let (label, head) = line.split_once(' ')?;
            GitOid::parse(head).ok()?;
            Some(Container {
                number: None,
                label: label.to_owned(),
                head: head.to_owned(),
                ci: String::new(),
            })
        })
        .collect()
}

/// The merge base bounds history to work done for the batch. Octopus covers a
/// batch of more than two heads; without a base no attribution is claimed.
fn merge_base(project: &Path, heads: &[String]) -> Option<String> {
    if heads.len() < 2 {
        return None;
    }
    let mut argv = vec!["merge-base".to_owned(), "--octopus".to_owned()];
    argv.extend(heads.iter().cloned());
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    let bytes = execute_for(project, &args, GIT_TIMEOUT, false)
        .ok()
        .map(|out| out.stdout.bytes)?;
    let base = String::from_utf8(bytes).ok()?.trim().to_owned();
    GitOid::parse(&base).ok()?;
    Some(base)
}

/// Read commits for all heads after the base in one bounded Git call.
fn read_history(project: &Path, heads: &[String], base: Option<&str>) -> Option<History> {
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
    if let Some(base) = base {
        argv.push("--not".to_owned());
        argv.push(base.to_owned());
    }
    argv.push("--".to_owned());
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    let bytes = execute_for(project, &args, GIT_TIMEOUT, false)
        .ok()
        .map(|out| out.stdout.bytes)?;
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
    let members: Vec<(u64, String)> = lines
        .iter()
        .filter(|line| line.error.is_none() && line.state == "OPEN")
        .map(|line| (line.number, line.head.clone()))
        .collect();
    let mut containers = open_containers(state, project);
    containers.extend(candidate_containers(project));
    if members.is_empty() || containers.is_empty() {
        return;
    }
    let mut heads: Vec<String> = containers
        .iter()
        .map(|container| container.head.clone())
        .chain(members.iter().map(|(_, head)| head.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let base = merge_base(project, &heads);
    heads.sort();
    let Some(history) = read_history(project, &heads, base.as_deref()) else {
        return;
    };
    let mut found = report(&history, base.as_deref(), &members, &containers);
    let labels: BTreeSet<String> = found
        .links
        .values()
        .map(|link| link.label.clone())
        .collect();
    if !labels.is_empty() {
        let Some(repository) = status::batch::repository(state, project).ok() else {
            return;
        };
        for container in &mut containers {
            if labels.contains(&container.key()) {
                container.ci = container_ci(state, project, &repository, &container.head);
            }
        }
        for link in found.links.values_mut() {
            if let Some(container) = containers
                .iter()
                .find(|container| container.key() == link.label)
            {
                link.ci = container.ci.clone();
            }
        }
    }
    for line in lines.iter_mut() {
        if let Some(link) = found.links.get(&line.number).cloned() {
            line.suppress_own_ci();
            line.batch = Some(link);
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
        }];
        (history, members, containers)
    }

    // Spec: docs/specs/cli.md#integration-batch-membership
    #[test]
    fn a_member_head_contained_in_an_open_batch_reads_as_superseded() {
        let (history, members, containers) = fixture();
        let found = report(&history, Some("b"), &members, &containers);
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
        let found = report(&history, Some("b"), &members, &containers);
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
        let found = report(&history, Some("b"), &members, &containers);
        assert!(!found.links.contains_key(&9), "a PR is not its own batch");
        assert_eq!(found.links.len(), 2);
        let mut unrelated = containers;
        unrelated.push(Container {
            number: Some(10),
            label: "#10".into(),
            head: "b".into(),
            ci: String::new(),
        });
        let found = report(&history, Some("b"), &members, &unrelated);
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
        }];
        let found = report(&history, Some("b"), &members, &containers);
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
    fn membership_without_a_base_never_claims_a_moved_head() {
        let (history, mut members, _) = fixture();
        members[0].1 = "m7-new".into();
        assert_eq!(standing(&history, None, "m7-new", "batch"), None);
        assert_eq!(
            standing(&history, Some("b"), "m7-new", "batch"),
            Some(Standing::HeadMoved)
        );
        assert_eq!(
            standing(&history, None, "m7", "batch"),
            Some(Standing::HeadMerged)
        );
    }
}
