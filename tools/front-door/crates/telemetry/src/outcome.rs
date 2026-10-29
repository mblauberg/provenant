//! Offline post-merge outcome attribution.
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    ffi::OsString,
    path::Path,
    time::Duration,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum OutcomeError {
    #[error("outcome evidence: {0}")]
    Evidence(String),
}
type Result<T> = std::result::Result<T, OutcomeError>;

/// One node's retained evidence. Unknown measurements stay absent.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Evidence {
    pub sample: String,
    pub graph: Option<String>,
    pub pr: Option<u64>,
    pub issue: Option<u64>,
    pub pattern: Option<String>,
    pub lens: Option<String>,
    pub model: Option<String>,
    pub risk_tier: Option<String>,
    pub task_kind: Option<String>,
    pub cost_microdollars: Option<u64>,
    pub confirmed: Option<u64>,
    pub false_positives: Option<u64>,
}

/// The shape produced by `gh run list --branch main --json ...`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CiRun {
    pub head_sha: String,
    pub conclusion: String,
    #[serde(deserialize_with = "timestamp")]
    pub created_at: jiff::Timestamp,
    pub workflow_name: String,
}

fn timestamp<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<jiff::Timestamp, D::Error> {
    String::deserialize(deserializer)?
        .parse()
        .map_err(serde::de::Error::custom)
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Signals {
    pub unmeasured: bool,
    pub reverts: u64,
    pub follow_up_fixes: u64,
    /// None means no CI input was supplied, never measured zero.
    pub ci_failures: Option<u64>,
}
impl Signals {
    pub fn escaped(&self) -> bool {
        self.reverts > 0 || self.follow_up_fixes > 0 || self.ci_failures.is_some_and(|n| n > 0)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Attributed {
    pub evidence: Evidence,
    pub stratum: &'static str,
    pub merge: Option<String>,
    pub mature: bool,
    pub signals: Signals,
}

#[derive(Debug, Serialize)]
pub struct Outcomes {
    pub ci_coverage_from: Option<String>,
    pub skipped_ci_runs: usize,
    pub window_days: u32,
    pub ci: &'static str,
    pub unattributed_rows: usize,
    pub rows: Vec<Attributed>,
}

struct Commit {
    sha: String,
    time: i64,
    parents: Vec<String>,
    subject: String,
    body: String,
}

/// All Git operations use the shared executable resolver, with inherited Git
/// repository/config overrides removed. No network or working-tree writes.
fn git(repo: &Path, args: &[&str]) -> Result<String> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    let executable = provenant_os::git::git_executable()
        .ok_or_else(|| OutcomeError::Evidence("Git unavailable".into()))?;
    let mut env: Vec<_> = std::env::vars_os()
        .filter(|(key, _)| !key.to_string_lossy().starts_with("GIT_"))
        .collect();
    env.extend(
        [
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_OPTIONAL_LOCKS", "0"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v))),
    );
    let mut argv = vec![executable.into_os_string()];
    argv.extend(
        [
            "--no-pager",
            "--literal-pathspecs",
            "-c",
            "core.quotePath=false",
        ]
        .map(OsString::from),
    );
    argv.extend(args.iter().map(OsString::from));
    let mut options = ExecOptions::new(&argv, repo, Duration::from_secs(30));
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture {
        limit: 64 * 1024 * 1024,
    };
    options.stop_on_output_limit = true;
    let output = SystemExecutor::new(1)
        .and_then(|executor| executor.execute(options))
        .map_err(|e| OutcomeError::Evidence(e.to_string()))?;
    if output.termination != Termination::Exit(0)
        || output.timed_out
        || output.drain_truncated
        || output.stdout.truncated
        || output.stdout.read_failed
        || output.cancelled
        || output.overflow_stopped
    {
        return Err(OutcomeError::Evidence(format!(
            "Git failed or evidence truncated: {}",
            String::from_utf8_lossy(&output.stderr.bytes).trim()
        )));
    }
    String::from_utf8(output.stdout.bytes).map_err(|e| OutcomeError::Evidence(e.to_string()))
}

fn number(text: &str) -> Option<u64> {
    text.chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

fn parse_commits(log: &str) -> Result<Vec<Commit>> {
    let mut commits = Vec::new();
    for record in log.split('\x1e').map(str::trim).filter(|s| !s.is_empty()) {
        let fields: Vec<_> = record.split('\0').collect();
        if fields.len() < 6 {
            return Err(OutcomeError::Evidence("invalid Git log record".into()));
        }
        commits.push(Commit {
            sha: fields[0].into(),
            time: fields[1]
                .parse()
                .map_err(|_| OutcomeError::Evidence("invalid Git timestamp".into()))?,
            parents: fields[2].split_whitespace().map(str::to_owned).collect(),
            subject: fields[3].into(),
            body: fields[4].into(),
        });
        if commits.len() > 100_000 {
            return Err(OutcomeError::Evidence(
                "history exceeds 100,000 commits".into(),
            ));
        }
    }
    Ok(commits)
}

/// First-parent history is the explicit main-line ref supplied by the caller.
pub fn attribute(
    repo: &Path,
    main: &str,
    evidence: Vec<Evidence>,
    ci: Option<&[CiRun]>,
    window_days: u32,
    now: i64,
) -> Result<Outcomes> {
    if window_days == 0 || evidence.len() > 100_000 || main.starts_with('-') {
        return Err(OutcomeError::Evidence(
            "invalid window, history ref or evidence limit".into(),
        ));
    }
    // Resolve once, preventing option injection and a moving ref during analysis.
    let head = git(
        repo,
        &["rev-parse", "--verify", &format!("{main}^{{commit}}")],
    )?;
    let mut commits = parse_commits(&git(
        repo,
        &[
            "log",
            "--first-parent",
            "--reverse",
            "--format=%H%x00%ct%x00%P%x00%s%x00%x00%x1e",
            head.trim(),
        ],
    )?)?;
    if evidence
        .iter()
        .any(|row| row.pr.is_none() && row.issue.is_some())
    {
        for commit in commits.iter_mut().filter(|c| c.parents.len() > 1) {
            commit.body = git(repo, &["show", "-s", "--format=%b", &commit.sha])?;
        }
    }
    let window = i64::from(window_days) * 86400;
    let ci_coverage_from = ci.and_then(|runs| runs.iter().map(|r| r.created_at).min());
    let merge_indices: HashMap<_, _> = commits
        .iter()
        .enumerate()
        .filter(|(_, c)| c.parents.len() > 1)
        .map(|(i, c)| (c.sha.as_str(), i))
        .collect();
    let mut skipped_ci_runs = 0;
    let mut rows = Vec::new();
    let mut signal_cache = BTreeMap::<usize, Signals>::new();
    let mut seen_ci = BTreeSet::new();
    let mut ci_by_merge = BTreeMap::<usize, u64>::new();
    if let Some(runs) = ci {
        for run in runs {
            if !seen_ci.insert((&run.head_sha, &run.workflow_name, run.created_at)) {
                continue;
            }
            if !run.head_sha.bytes().all(|b| b.is_ascii_hexdigit()) || run.head_sha.len() != 40 {
                return Err(OutcomeError::Evidence("invalid CI headSha".into()));
            }
            // Reachability, rather than timestamp order, assigns a run to the
            // nearest preceding main-line merge, including ordinary main commits.
            let ancestors = match git(repo, &["rev-list", "--first-parent", &run.head_sha]) {
                Ok(value) => value,
                Err(_) => {
                    skipped_ci_runs += 1;
                    continue;
                }
            };
            if run.conclusion != "failure" {
                continue;
            }
            let nearest = ancestors
                .lines()
                .find_map(|sha| merge_indices.get(sha).copied());
            if let Some(index) = nearest {
                let merge = &commits[index];
                let elapsed = run.created_at.as_second() - merge.time;
                if (0..=window).contains(&elapsed) && run.created_at.as_second() <= now {
                    *ci_by_merge.entry(index).or_default() += 1;
                }
            }
        }
    }
    let descendants = if let Some(first) = commits.first() {
        git(
            repo,
            &[
                "log",
                "--ancestry-path",
                "--no-merges",
                "--reverse",
                "--format=%H%x00%ct%x00%P%x00%s%x00%x00%x1e",
                &format!("{}..{}", first.sha, head.trim()),
            ],
        )
        .and_then(|log| parse_commits(&log))
    } else {
        Ok(Vec::new())
    };
    let mut cache = CandidateCache::default();
    for row in evidence {
        let matches: Vec<_> = commits
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                if c.parents.len() < 2 || c.time > now {
                    return false;
                }
                let pr = c
                    .subject
                    .strip_prefix("Merge pull request #")
                    .and_then(number);
                row.pr.is_some() && row.pr == pr
                    || row.pr.is_none()
                        && row.issue.is_some_and(|issue| {
                            c.body.lines().any(|line| {
                                line.trim().strip_prefix("Closes #").and_then(number) == Some(issue)
                            })
                        })
            })
            .map(|(i, _)| i)
            .collect();
        // Ambiguity is evidence missing, not permission to guess.
        let origin = (row.graph.is_some() && matches.len() == 1).then(|| matches[0]);
        let (merge, mature, signals) = if let Some(index) = origin {
            let commit = &commits[index];
            let signals = if let Some(signals) = signal_cache.get(&index) {
                signals.clone()
            } else {
                let mut signals = Signals {
                    ci_failures: ci_coverage_from
                        .filter(|t| t.as_second() <= commit.time)
                        .map(|_| ci_by_merge.get(&index).copied().unwrap_or(0)),
                    ..Signals::default()
                };
                if measure_merge(
                    repo,
                    commit,
                    &descendants,
                    window,
                    now,
                    &mut signals,
                    &mut cache,
                )
                .is_err()
                {
                    signals.unmeasured = true;
                }
                signal_cache.insert(index, signals.clone());
                signals
            };
            (
                Some(commit.sha.clone()),
                now - commit.time >= window,
                signals,
            )
        } else {
            (
                None,
                false,
                Signals {
                    ci_failures: None,
                    ..Signals::default()
                },
            )
        };
        rows.push(Attributed {
            evidence: row,
            stratum: if signals.unmeasured {
                "unmeasured"
            } else if merge.is_some() {
                "attributed"
            } else {
                "unattributed"
            },
            merge,
            mature,
            signals,
        });
    }
    Ok(Outcomes {
        window_days,
        ci_coverage_from: ci_coverage_from.map(|t| t.to_string()),
        skipped_ci_runs,
        ci: if ci.is_some() {
            "measured"
        } else {
            "not_measured"
        },
        unattributed_rows: rows.iter().filter(|r| r.merge.is_none()).count(),
        rows,
    })
}

const MAX_FILES_PER_MERGE: usize = 200;
const MAX_HUNKS_PER_MERGE: usize = 2_000;

fn fix_subject(subject: &str) -> bool {
    let lower = subject.to_ascii_lowercase();
    ["fix", "bugfix", "hotfix"].iter().any(|word| {
        lower.strip_prefix(word).is_some_and(|rest| {
            rest.chars()
                .next()
                .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '!')
        })
    })
}

#[derive(Default)]
struct CandidateCache {
    fixes: HashMap<String, Option<(BTreeSet<String>, usize, usize)>>,
    bodies: HashMap<String, Option<String>>,
}

fn measure_merge(
    repo: &Path,
    commit: &Commit,
    descendants: &Result<Vec<Commit>>,
    window: i64,
    now: i64,
    signals: &mut Signals,
    cache: &mut CandidateCache,
) -> Result<()> {
    let descendants = descendants
        .as_ref()
        .map_err(|e| OutcomeError::Evidence(e.to_string()))?;
    let (changed, mut files_used, mut hunks_used) =
        changed_lines(repo, &commit.parents[0], &commit.sha, false)?;
    let mut lines = BTreeSet::new();
    for (file, ranges) in changed {
        for (start, count) in ranges {
            lines.extend(blamed_lines(repo, &commit.sha, &file, start, count)?);
        }
    }
    let merged = git(
        repo,
        &[
            "rev-list",
            &format!("{}..{}", commit.parents[0], commit.parents[1]),
        ],
    )?;
    for later in descendants.iter().filter(|c| {
        c.time >= commit.time
            && c.time <= commit.time + window
            && c.time <= now
            && c.parents.len() == 1
    }) {
        if !later.subject.starts_with("Revert ") && !fix_subject(&later.subject) {
            continue;
        }
        // A candidate must descend from this merge, not merely share its time window.
        if git(
            repo,
            &["merge-base", "--is-ancestor", &commit.sha, &later.sha],
        )
        .is_err()
        {
            continue;
        }
        let body = cache
            .bodies
            .entry(later.sha.clone())
            .or_insert_with(|| git(repo, &["show", "-s", "--format=%b", &later.sha]).ok())
            .as_ref()
            .ok_or_else(|| OutcomeError::Evidence("commit body unavailable".into()))?;
        let reverts = body.split("This reverts commit ").skip(1).any(|part| {
            part.starts_with(&commit.sha) || merged.lines().any(|sha| part.starts_with(sha))
        }) || later.subject == format!("Revert \"{}\"", commit.subject);
        if reverts {
            signals.reverts += 1;
            continue;
        }
        if !fix_subject(&later.subject) {
            continue;
        }
        let candidate = cache.fixes.entry(later.sha.clone()).or_insert_with(|| {
            let (changed, files, hunks) =
                changed_lines(repo, &later.parents[0], &later.sha, true).ok()?;
            let mut blamed = BTreeSet::new();
            for (file, ranges) in changed {
                for (start, count) in ranges {
                    blamed.extend(blamed_lines(repo, &later.parents[0], &file, start, count).ok()?);
                }
            }
            Some((blamed, files, hunks))
        });
        let candidate = candidate
            .as_ref()
            .ok_or_else(|| OutcomeError::Evidence("fix measurement unavailable".into()))?;
        files_used += candidate.1;
        hunks_used += candidate.2;
        if files_used > MAX_FILES_PER_MERGE || hunks_used > MAX_HUNKS_PER_MERGE {
            return Err(OutcomeError::Evidence(
                "merge measurement limit exceeded".into(),
            ));
        }
        let overlaps = candidate.0.iter().any(|line| lines.contains(line));
        if overlaps {
            signals.follow_up_fixes += 1;
        }
    }
    Ok(())
}

type Ranges = BTreeMap<String, Vec<(u64, u64)>>;
fn changed_lines(
    repo: &Path,
    before: &str,
    after: &str,
    old: bool,
) -> Result<(Ranges, usize, usize)> {
    let files = git(
        repo,
        &[
            "diff",
            "--no-ext-diff",
            "--no-renames",
            "--name-only",
            "-z",
            before,
            after,
            "--",
        ],
    )?;
    let mut result = BTreeMap::new();
    let mut file_count = 0;
    let mut hunk_count = 0;
    for file in files.split('\0').filter(|s| !s.is_empty()) {
        file_count += 1;
        if file_count > MAX_FILES_PER_MERGE {
            return Err(OutcomeError::Evidence("file limit exceeded".into()));
        }
        let diff = git(
            repo,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "--unified=0",
                before,
                after,
                "--",
                file,
            ],
        )?;
        if diff.lines().any(|line| {
            line.trim_start_matches(['+', '-', ' '])
                .starts_with("Subproject commit ")
                || line == "new file mode 160000"
                || line == "deleted file mode 160000"
        }) {
            continue;
        }
        let mut ranges = Vec::new();
        for line in diff.lines().filter(|line| line.starts_with("@@ ")) {
            hunk_count += 1;
            if hunk_count > MAX_HUNKS_PER_MERGE {
                return Err(OutcomeError::Evidence("hunk limit exceeded".into()));
            }
            let range = line
                .split_whitespace()
                .nth(if old { 1 } else { 2 })
                .unwrap_or("");
            let mut parts = range.get(1..).unwrap_or("").split(',');
            if let Some(start) = parts.next().and_then(|s| s.parse().ok()) {
                let count = parts
                    .next()
                    .unwrap_or("1")
                    .parse()
                    .map_err(|_| OutcomeError::Evidence("invalid diff hunk".into()))?;
                if count > 0 {
                    ranges.push((start, count));
                }
            }
        }
        if !ranges.is_empty() {
            result.insert(file.into(), ranges);
        }
    }
    Ok((result, file_count, hunk_count))
}

fn blamed_lines(
    repo: &Path,
    commit: &str,
    file: &str,
    start: u64,
    count: u64,
) -> Result<BTreeSet<String>> {
    if count > 100_000 {
        return Err(OutcomeError::Evidence(
            "changed-line range exceeds 100,000".into(),
        ));
    }
    let text = git(
        repo,
        &[
            "blame",
            "--line-porcelain",
            "--no-textconv",
            "-L",
            &format!("{start},+{count}"),
            commit,
            "--",
            file,
        ],
    )?;
    let mut result = BTreeSet::new();
    let mut identity = None;
    for line in text.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() >= 3
            && fields[0].len() == 40
            && fields[0].bytes().all(|b| b.is_ascii_hexdigit())
        {
            identity = Some(format!("{}:{}", fields[0], fields[1]));
        } else if line.starts_with("filename ")
            && let Some(identity) = &identity
        {
            result.insert(format!("{}:{identity}", &line[9..]));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::fix_subject;

    #[test]
    fn fix_subject_requires_a_word_boundary() {
        assert!(fix_subject("fix: defect"));
        assert!(fix_subject("Hotfix defect"));
        assert!(!fix_subject("fixture"));
        assert!(!fix_subject("fixup! previous change"));
    }
}
