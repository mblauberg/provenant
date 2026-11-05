//! Advisory tracker lint composition. Forge failures do not block work.
use crate::{emit, pr, roots};
use provenant_cli::{
    Globals, Tracker,
    tracker::{self, Config, Finding},
};
use provenant_records::{CommandResult, Outcome};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, Instant},
};

#[derive(Clone, Deserialize)]
struct IssueType {
    name: String,
}
#[derive(Clone, Deserialize)]
struct Issue {
    body: Option<String>,
    state: String,
    #[serde(rename = "issueType")]
    issue_type: Option<IssueType>,
}
#[derive(Serialize)]
struct Report {
    findings: Vec<Finding>,
    skipped: Vec<String>,
}

fn issue(root: &Path, number: u64, timeout: Duration) -> Result<Issue, String> {
    let number = number.to_string();
    let args = [
        "gh",
        "issue",
        "view",
        &number,
        "--json",
        "body,state,issueType",
    ];
    let output = pr::execute_for(root, &args, timeout, false)
        .and_then(pr::successful)
        .map_err(|e| e.to_string())?;
    serde_json::from_slice(&output).map_err(|e| e.to_string())
}

fn config(root: &Path, skipped: &mut Vec<String>) -> Config {
    let path = root.join(".provenant/tracker-lint.toml");
    match std::fs::read_to_string(&path) {
        Ok(text) => match toml_edit::de::from_str(&text) {
            Ok(config) => {
                if let Ok(doc) = text.parse::<toml_edit::DocumentMut>() {
                    for (key, _) in doc.iter() {
                        if !["citation", "closed_owner", "acceptance", "verify"].contains(&key) {
                            skipped.push(format!("unknown tracker lint config key: {key}"));
                        }
                    }
                }
                config
            }
            Err(error) => {
                skipped.push(format!(
                    "invalid tracker lint config; defaults used: {error}"
                ));
                Config::default()
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(error) => {
            skipped.push(format!(
                "tracker lint config unavailable; defaults used: {error}"
            ));
            Config::default()
        }
    }
}

fn inspect(
    root: &Path,
    sources: &[(String, String, String)],
    config: &Config,
    fetch: &mut impl FnMut(u64) -> Result<Issue, String>,
) -> Report {
    let mut findings = Vec::new();
    let mut skipped = Vec::new();
    let mut states = BTreeMap::new();
    if config.closed_owner != tracker::Severity::Off {
        for (_, body, _) in sources {
            for number in tracker::live_references(body) {
                if states.contains_key(&number) {
                    continue;
                }
                match fetch(number) {
                    Ok(issue) => {
                        if issue.state.eq_ignore_ascii_case("closed") {
                            states.insert(number, true);
                        } else if issue.state.eq_ignore_ascii_case("open") {
                            states.insert(number, false);
                        }
                    }
                    Err(error) => {
                        skipped.push(format!("closed-issue check skipped for #{number}: {error}"))
                    }
                }
            }
        }
    }
    for (label, body, kind) in sources {
        for mut finding in tracker::lint(root, body, kind, config, &states) {
            finding.message = format!("{label}: {}", finding.message);
            findings.push(finding);
        }
    }
    Report { findings, skipped }
}

fn collect(
    root: &Path,
    numbers: &[u64],
    file: Option<&Path>,
    kind: &str,
    fetch: &mut impl FnMut(u64) -> Result<Issue, String>,
) -> Report {
    let mut skipped = Vec::new();
    let cfg = config(root, &mut skipped);
    let mut sources = Vec::new();
    let mut cache = BTreeMap::new();
    let mut cached_fetch = |number| cache.entry(number).or_insert_with(|| fetch(number)).clone();
    if let Some(path) = file {
        match std::fs::read_to_string(path) {
            Ok(body) => sources.push((path.display().to_string(), body, kind.to_owned())),
            Err(error) => skipped.push(format!("local file skipped: {error}")),
        }
    }
    for &number in numbers {
        match cached_fetch(number) {
            Ok(issue) => sources.push((
                format!("#{number}"),
                issue.body.unwrap_or_default(),
                issue.issue_type.map_or_else(|| kind.to_owned(), |t| t.name),
            )),
            Err(_) => skipped.push(format!("issue #{number} skipped: forge unavailable")),
        }
    }
    if sources.is_empty() && numbers.is_empty() && file.is_none() {
        skipped.push("no issues selected; pass issue numbers or --file".into());
    }
    let mut report = inspect(root, &sources, &cfg, &mut cached_fetch);
    skipped.append(&mut report.skipped);
    report.skipped = skipped;
    report
}

fn human(report: &Report) -> String {
    let mut text = String::new();
    for f in &report.findings {
        text.push_str(&format!("{:?}: {}: {}\n", f.severity, f.check, f.message));
    }
    for s in &report.skipped {
        text.push_str(&format!("skipped: {s}\n"));
    }
    text
}

pub fn run(command: Tracker, globals: &Globals) -> u8 {
    let Tracker::Lint {
        numbers,
        file,
        kind,
    } = command;
    let root = match roots::selected_project(globals.project.as_deref(), false) {
        Ok(root) => root,
        Err(error) => {
            return crate::fail(
                "tracker lint",
                Outcome::Invalid,
                "ROOT",
                &error.to_string(),
                globals,
            );
        }
    };
    let root = project_root(&root);
    let report = collect(&root, &numbers, file.as_deref(), &kind, &mut |n| {
        issue(&root, n, Duration::from_secs(15))
    });
    let human = human(&report);
    emit(
        &CommandResult::new("tracker lint", Outcome::Ok, report),
        globals,
        &human,
        true,
    )
}

fn project_root(directory: &Path) -> std::path::PathBuf {
    provenant_os::git::repository_path(directory, provenant_os::git::RepositoryPath::Toplevel)
        .unwrap_or_else(|_| directory.to_path_buf())
}

#[allow(
    clippy::disallowed_methods,
    reason = "real-time deadline bounds forge advice"
)]
fn advice_fetch(
    fetches: &mut usize,
    deadline: Instant,
    fetch: impl FnOnce(Duration) -> Result<Issue, String>,
) -> Result<Issue, String> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if *fetches >= 5 || remaining.is_zero() {
        return Err("advice fetch limit reached".into());
    }
    *fetches += 1;
    fetch(remaining)
}

/// Report advice immediately before work starts, regardless of lint availability.
#[allow(
    clippy::disallowed_methods,
    reason = "entry advice has a real-time budget"
)]
pub(crate) fn advise_then<T>(
    root: &Path,
    spec: &str,
    path: Option<&Path>,
    out: &mut impl std::io::Write,
    proceed: impl FnOnce() -> T,
) -> T {
    let root = project_root(root);
    let spec = spec.trim();
    if spec.is_empty() {
        return proceed();
    }
    let number = spec
        .trim_start_matches('#')
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0);
    let numbers: Vec<_> = number.into_iter().collect();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut fetches = 0;
    let report = collect(
        &root,
        &numbers,
        if number.is_some() { None } else { path },
        "task",
        &mut |n| {
            advice_fetch(&mut fetches, deadline, |remaining| {
                issue(&root, n, remaining)
            })
        },
    );
    let _ = out.write_all(human(&report).as_bytes());
    proceed()
}

#[cfg(test)]
mod tests {
    #[allow(
        unused_imports,
        reason = "the worktree driver includes this module without libtest"
    )]
    use super::*;
    #[allow(
        unused_imports,
        reason = "the worktree driver includes this module without libtest"
    )]
    use std::error::Error;

    #[test]
    fn tracker_local_file_works_when_forge_is_unavailable() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("issue.md");
        std::fs::write(
            &path,
            "Owner: #17\nAcceptance: works\nVerify: test\nSee src/missing.rs#Gone",
        )?;
        let report = collect(dir.path(), &[], Some(&path), "task", &mut |_| {
            Err("offline".into())
        });
        assert!(report.findings.iter().any(|f| f.check == "citation"));
        assert!(report.skipped.iter().any(|s| s.contains("#17")));
        Ok(())
    }

    #[test]
    fn tracker_repository_config_changes_report() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir(dir.path().join(".provenant"))?;
        std::fs::write(
            dir.path().join(".provenant/tracker-lint.toml"),
            "acceptance = 'off'\nverify = 'info'\n",
        )?;
        let path = dir.path().join("issue.md");
        std::fs::write(&path, "Export records")?;
        let report = collect(dir.path(), &[], Some(&path), "task", &mut |_| {
            Err("offline".into())
        });
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].check, "verify");
        assert_eq!(report.findings[0].severity, tracker::Severity::Info);
        Ok(())
    }

    #[test]
    fn tracker_null_type_uses_kind_and_reuses_fetch() {
        let mut calls = 0;
        let report = collect(Path::new("."), &[12], None, "task", &mut |_| {
            calls += 1;
            serde_json::from_str(r#"{"body":"Owner: #12","state":"mystery","issueType":null}"#)
                .map_err(|error| error.to_string())
        });
        assert_eq!(calls, 1);
        assert!(report.findings.iter().any(|f| f.check == "acceptance"));
        assert!(report.findings.iter().any(|f| f.check == "verify"));
        assert!(!report.findings.iter().any(|f| f.check == "closed_owner"));
    }

    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "test exercises the real-time deadline"
    )]
    fn tracker_advice_bounds_fetches_and_deadline() {
        let mut count = 0;
        let deadline = Instant::now() + Duration::from_secs(3);
        for _ in 0..5 {
            assert!(advice_fetch(&mut count, deadline, |_| Err("offline".into())).is_err());
        }
        assert!(advice_fetch(&mut count, deadline, |_| panic!("over limit")).is_err());
        let mut count = 0;
        assert!(advice_fetch(&mut count, Instant::now(), |_| panic!("expired")).is_err());
        assert_eq!(count, 0);
    }

    #[test]
    fn tracker_unknown_config_key_preserves_settings() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir(dir.path().join(".provenant"))?;
        std::fs::write(
            dir.path().join(".provenant/tracker-lint.toml"),
            "acceptance = 'off'\nextra = 'value'\n",
        )?;
        let mut warnings = Vec::new();
        let config = config(dir.path(), &mut warnings);
        assert_eq!(config.acceptance, tracker::Severity::Off);
        assert!(warnings.iter().any(|s| s.contains("extra")));
        Ok(())
    }

    #[test]
    fn tracker_advice_shows_warnings_and_returns_to_caller() -> Result<(), Box<dyn Error>> {
        let dir = tempfile::tempdir()?;
        std::fs::write(
            dir.path().join("issue.md"),
            "Acceptance: works\nVerify: test\nSee gone.rs#Gone",
        )?;
        let mut output = Vec::new();
        let mut proceeded = false;
        let result = advise_then(
            dir.path(),
            "issue.md",
            Some(&dir.path().join("issue.md")),
            &mut output,
            || {
                proceeded = true;
                42
            },
        );
        assert!(String::from_utf8(output)?.contains("gone.rs#Gone"));
        assert!(proceeded);
        assert_eq!(result, 42);
        Ok(())
    }
}
