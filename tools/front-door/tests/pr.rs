mod support;
use std::{
    fs,
    io::{self, Write},
    path::Path,
    process::ExitCode,
};
use support::{Result, TestRig, code};

const HEAD: &str = "1111111111111111111111111111111111111111";
const OTHER: &str = "2222222222222222222222222222222222222222";

fn rig() -> Result<TestRig> {
    let mut rig = TestRig::new()?;
    let bin = rig.root.join("bin");
    fs::create_dir(&bin)?;
    let gh = bin.join("gh");
    // Keep execution in the lane build cache, including in confined writers.
    std::os::unix::fs::symlink(std::env::current_exe()?, &gh)?;
    let (_, path) = rig
        .env
        .iter_mut()
        .find(|(key, _)| key == "PATH")
        .ok_or("PATH")?;
    *path = format!("{}:/usr/bin:/bin", bin.display()).into();
    fs::write(
        rig.project.join("head.json"),
        format!(r#"{{"headRefOid":"{HEAD}"}}"#),
    )?;
    fs::write(
        rig.project.join("checks.json"),
        r#"[{"name":"ci-status","bucket":"pass"},{"name":"optional","bucket":"skipping"}]"#,
    )?;
    // The fake forge has no reviews or target to read, so land on CI alone.
    fs::write(
        rig.state.join("config.toml"),
        "schema_version = 1\n[domains.pr.land]\napproval_tier = 'none'\n",
    )?;
    Ok(rig)
}
fn fixture_git(rig: &TestRig, args: &[&str]) -> Result<String> {
    let git = provenant_os::git::resolve_git_executable(None).ok_or("native Git executable")?;
    let output = rig.execute(&git, args, None)?;
    assert_eq!(
        code(&output),
        0,
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    Ok(String::from_utf8(output.stdout.bytes)?.trim().to_owned())
}

/// A real PR with local and remote refs for the landing repository adapter.
fn live_pr_origin(rig: &TestRig) -> Result<String> {
    fixture_git(rig, &["init", "-q", "--initial-branch=main"])?;
    fixture_git(rig, &["config", "user.name", "Fixture"])?;
    fixture_git(rig, &["config", "user.email", "fixture@example.invalid"])?;
    fixture_git(rig, &["config", "maintenance.auto", "false"])?;
    fixture_git(rig, &["config", "gc.auto", "0"])?;
    fs::write(rig.project.join("base.txt"), "base\n")?;
    fixture_git(rig, &["add", "base.txt"])?;
    fixture_git(rig, &["commit", "-qm", "base"])?;
    let base = fixture_git(rig, &["rev-parse", "HEAD"])?;

    let origin = rig.root.join("origin.git");
    let origin = origin.to_str().ok_or("origin path")?;
    fixture_git(
        rig,
        &["init", "-q", "--bare", "--initial-branch=main", origin],
    )?;
    fixture_git(rig, &["remote", "add", "origin", origin])?;
    fixture_git(rig, &["push", "-q", "--set-upstream", "origin", "main"])?;
    fixture_git(rig, &["checkout", "-q", "-b", "feature"])?;
    fs::write(rig.project.join("feature.txt"), "feature\n")?;
    fixture_git(rig, &["add", "feature.txt"])?;
    fixture_git(rig, &["commit", "-qm", "feature"])?;
    let head = fixture_git(rig, &["rev-parse", "HEAD"])?;
    fixture_git(rig, &["push", "-q", "origin", "feature"])?;

    fs::write(
        rig.project.join("head.json"),
        serde_json::to_vec(&serde_json::json!({"headRefOid": head}))?,
    )?;
    fs::write(rig.project.join("base-sha"), &base)?;
    fs::write(rig.project.join("live-pr"), "")?;
    let refs = fixture_git(rig, &["ls-remote", "--heads", "origin"])?;
    assert!(
        refs.lines()
            .any(|line| line.starts_with(&format!("{base}\trefs/heads/main"))),
        "{refs}"
    );
    assert!(
        refs.lines()
            .any(|line| line.starts_with(&format!("{head}\trefs/heads/feature"))),
        "{refs}"
    );
    Ok(head)
}

fn merge(rig: &TestRig) -> Result<provenant_os::exec::ExecOutcome> {
    rig.command(&["land", "1668", "--head", HEAD, "--json"])
}
fn no_merge(rig: &TestRig) -> Result {
    assert!(!rig.project.join("merge-args").exists());
    let calls = fs::read_to_string(rig.project.join("gh-calls"))?;
    assert!(!calls.contains("pr merge"), "{calls}");
    Ok(())
}

fn pr_watch_reports_the_verified_head_and_reads_ci_through_the_observer() -> Result {
    let rig = self::rig()?;
    let output = rig.command(&["pr", "watch", "1668", "--full"])?;
    assert_eq!(code(&output), 0, "{:?}", output);
    assert_eq!(
        String::from_utf8(output.stdout.bytes)?,
        format!("pass #1668 {HEAD}\n")
    );
    let calls = fs::read_to_string(rig.project.join("gh-calls"))?;
    assert!(
        calls.contains(&format!("commits/{HEAD}/check-runs")),
        "{calls}"
    );
    assert!(!calls.contains("pr checks"), "{calls}");
    no_merge(&rig)
}

fn pr_watch_failure_and_head_changes_report_fail_with_head() -> Result {
    for flag in ["watch-fail", "move-head"] {
        let rig = self::rig()?;
        if flag == "watch-fail" {
            fs::write(rig.project.join("checks.json"), r#"[{"bucket":"fail"}]"#)?;
        } else {
            fs::write(rig.project.join(flag), "")?;
        }
        let output = rig.command(&["pr", "watch", "1668"])?;
        assert_ne!(code(&output), 0, "{flag}");
        let error = String::from_utf8(output.stderr.bytes)?;
        assert!(error.contains(&format!("fail #1668 {HEAD}")), "{error}");
        no_merge(&rig)?;
    }
    Ok(())
}

fn land_refuses_unpassed_missing_and_malformed_checks() -> Result {
    for bucket in ["fail", "cancel", "unknown", "malformed"] {
        let rig = self::rig()?;
        let checks = match bucket {
            "malformed" => "invalid json".into(),
            _ => format!(r#"[{{"name":"ci-status","bucket":"{bucket}"}}]"#),
        };
        fs::write(rig.project.join("checks.json"), checks)?;
        assert_ne!(code(&merge(&rig)?), 0, "{bucket}");
        no_merge(&rig)?;
    }
    Ok(())
}

fn land_refuses_head_changes_before_and_after_checks() -> Result {
    for changed_before in [false, true] {
        let rig = self::rig()?;
        if changed_before {
            fs::write(
                rig.project.join("head.json"),
                format!(r#"{{"headRefOid":"{OTHER}"}}"#),
            )?;
        } else {
            fs::write(rig.project.join("move-head"), "")?;
        }
        assert_ne!(code(&merge(&rig)?), 0);
        no_merge(&rig)?;
        if changed_before {
            assert!(!fs::read_to_string(rig.project.join("gh-calls"))?.contains("pr checks"));
        }
    }
    Ok(())
}

fn merge_calls(rig: &TestRig) -> Result<usize> {
    Ok(fs::read_to_string(rig.project.join("gh-calls"))?
        .lines()
        .filter(|line| line.contains("--method PUT"))
        .count())
}

fn rest_merge_refusal(flag: &str, expected: &str) -> Result {
    let rig = self::rig()?;
    let head = live_pr_origin(&rig)?;
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[domains.pr.land]\napproval_tier='none'\npublish='rest_ordered'\n",
    )?;
    fs::write(rig.project.join(flag), "")?;
    let output = rig.command(&["land", "1668", "--head", &head, "--json"])?;
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(report["error"]["code"], expected, "{report}");
    assert_eq!(report["status"], "refused", "{report}");
    assert_eq!(report["data"]["merged"], false, "{report}");
    assert_eq!(report["data"]["steps"], serde_json::json!([]));
    assert!(
        report["error"]["repair"]
            .as_str()
            .is_some_and(|repair| !repair.is_empty()),
        "{report}"
    );
    assert_eq!(merge_calls(&rig)?, 1);
    assert!(!rig.project.join("merged").exists());
    Ok(())
}

fn land_refuses_a_rest_merge_409_as_head_moved() -> Result {
    rest_merge_refusal("merge-conflict", "HEAD_MOVED")
}

fn land_refuses_a_rest_merge_405_with_a_next_step() -> Result {
    rest_merge_refusal("merge-not-allowed", "MERGE_NOT_ALLOWED")
}

fn land_plain_singleton_uses_batch_driver() -> Result {
    let rig = self::rig()?;
    let git = provenant_os::git::git_executable().ok_or("Git executable")?;
    let remote = rig.root.join("unused-remote.git");
    assert_eq!(
        code(&rig.execute(
            &git,
            &[
                "remote",
                "add",
                "origin",
                remote.to_str().ok_or("remote path")?
            ],
            None
        )?),
        0
    );
    fs::write(
        rig.state.join("config.toml"),
        "schema_version = 1\n[domains.pr.land]\napproval_tier = 'carry'\nbatch_wait_seconds = 0\n",
    )?;
    let output = rig.command(&["land", "1668", "--json"])?;
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_ne!(code(&output), 0, "an unapproved PR must not land");
    assert_eq!(report["data"]["outcomes"]["1668"], "excluded", "{report}");
    assert_eq!(report["error"]["code"], "LAND_REFUSED", "{report}");
    assert!(!rig.project.join("merge-args").exists());
    Ok(())
}

fn land_explains_poll_and_budget_sources() -> Result {
    let rig = self::rig()?;
    let show = |args: &[&str]| -> Result<serde_json::Value> {
        let output = rig.command(args)?;
        assert_eq!(code(&output), 0);
        Ok(serde_json::from_slice(&output.stdout.bytes)?)
    };
    let default = show(&["land", "1668", "--explain", "--json"])?;
    assert_eq!(
        default["data"]["fields"]["poll_seconds"]["value"],
        provenant_cli::domains::LandSettings::default().poll_seconds
    );
    assert_eq!(
        default["data"]["fields"]["poll_seconds"]["source"]["layer"],
        "bundled"
    );
    fs::write(
        rig.state.join("config.toml"),
        "schema_version = 1\n[domains.pr.land]\npoll_seconds = 45\npublish_settle_seconds = 0\ncandidate_ref_prefix = 'custom/'\n",
    )?;
    let config = show(&["land", "1668", "--explain", "--json"])?;
    assert_eq!(config["data"]["fields"]["poll_seconds"]["value"], 45);
    assert_eq!(
        config["data"]["fields"]["poll_seconds"]["source"]["layer"],
        "host"
    );
    assert!(
        config["data"]["warnings"][0]
            .as_str()
            .unwrap_or_default()
            .contains("CI trigger")
    );
    assert!(
        config["data"]["warnings"]
            .as_array()
            .is_some_and(|warnings| warnings.iter().any(|warning| warning
                .as_str()
                .unwrap_or_default()
                .contains("disables the in-flight")))
    );
    let cli = show(&[
        "land",
        "1668",
        "--explain",
        "--poll",
        "7",
        "--wait=90s",
        "--json",
    ])?;
    assert_eq!(cli["data"]["fields"]["poll_seconds"]["value"], 7);
    assert_eq!(
        cli["data"]["fields"]["poll_seconds"]["source"]["layer"],
        "invocation"
    );
    assert_eq!(cli["data"]["ci_patience_seconds"], 90);
    assert_eq!(cli["data"]["ci_patience_source"], "--wait");
    let mut inputs = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let binding = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(fs::canonicalize(rig.project.join(".git"))?)?,
    )?;
    fs::write(
        rig.state.join("config.toml"),
        format!(
            "schema_version=1\n[domains.pr.land.projects.\"{}\"]\npoll_seconds=21\n[domains.pr.land.tiers.crucial]\npoll_seconds=12\n",
            binding.project_id()
        ),
    )?;
    let project = show(&["land", "--explain", "--json"])?;
    assert_eq!(project["data"]["fields"]["poll_seconds"]["value"], 21);
    assert_eq!(
        project["data"]["fields"]["poll_seconds"]["source"]["layer"],
        "project"
    );
    let tier = show(&["land", "--explain", "--tier", "crucial", "--json"])?;
    assert_eq!(tier["data"]["fields"]["poll_seconds"]["value"], 12);
    assert_eq!(
        tier["data"]["fields"]["poll_seconds"]["source"]["layer"],
        "tier"
    );
    assert!(!rig.project.join("gh-calls").exists());
    Ok(())
}

fn pr_external_failures_and_invalid_arguments_fail_closed() -> Result {
    for flag in ["checks-fail", "merge-fail"] {
        let rig = self::rig()?;
        fs::write(rig.project.join(flag), "")?;
        let output = merge(&rig)?;
        assert_ne!(code(&output), 0, "{flag}");
        let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
        assert_eq!(report["data"]["merged"], false);
        if flag == "checks-fail" {
            no_merge(&rig)?;
        }
    }
    let rig = self::rig()?;
    for args in [
        vec!["pr", "merge", "1668"],
        vec!["pr", "merge", "1668", "--head", "abc"],
        vec!["pr", "land", "1668"],
        vec!["pr", "land", "1668", "--head", "abc"],
        vec!["pr", "land", "0", "--head", HEAD],
        vec!["land", "0"],
        vec!["land", "1668", "--head"],
        vec!["pr", "watch", "0"],
        vec!["pr", "watch", "--admin"],
    ] {
        assert_eq!(code(&rig.command(&args)?), 2, "{args:?}");
    }
    assert!(!rig.project.join("gh-calls").exists());
    // A short name is valid syntax; it fails when it names no commit here.
    let output = rig.command(&["land", "1668", "--head", "abc"])?;
    assert_eq!(code(&output), 1, "{output:?}");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout.bytes),
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    assert!(text.contains("cannot resolve --head abc"), "{text}");
    assert!(!rig.project.join("gh-calls").exists());
    Ok(())
}

fn pr_status_prints_one_compact_line_per_pull_request() -> Result {
    let rig = self::rig()?;
    fs::write(
        rig.project.join("status-1668.json"),
        format!(
            r#"{{"state":"OPEN","headRefOid":"{HEAD}","mergeStateStatus":"CLEAN","reviewDecision":"APPROVED","isDraft":false}}"#
        ),
    )?;
    fs::write(
        rig.project.join("status-1669.json"),
        format!(
            r#"{{"state":"OPEN","headRefOid":"{OTHER}","mergeStateStatus":"BLOCKED","reviewDecision":"","isDraft":true}}"#
        ),
    )?;
    fs::write(
        rig.project.join("checks-1669.json"),
        r#"[{"bucket":"pass"},{"bucket":"skipping"},{"bucket":"pending"},{"bucket":"fail"}]"#,
    )?;
    let output = rig.command(&["pr", "status", "1668", "1669"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let text = String::from_utf8(output.stdout.bytes)?;
    assert!(text.starts_with("2 PRs:"), "{text}");
    assert!(
        text.lines().any(|line| line == "next: gh pr checks 1669"),
        "{text}"
    );
    let line = text
        .lines()
        .find(|line| line.starts_with("#1669 "))
        .ok_or("missing PR")?;
    for part in [
        "checks=2/4",
        "fail=1",
        "pending=1",
        "merge=blocked",
        "draft",
    ] {
        assert!(line.contains(part), "{line}");
    }
    let output = rig.command(&["pr", "status", "1668", "1669", "--full"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    // Empty check data is unavailable, rather than a passing result.
    fs::write(rig.project.join("checks.json"), "[]")?;
    let output = rig.command(&["pr", "status", "1668", "--json"])?;
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert!(report["data"][0]["checks_total"].is_null());
    let output = rig.command(&["pr", "status", "1668"])?;
    assert!(String::from_utf8(output.stdout.bytes)?.contains("checks=unavailable"));
    // A failed lookup is distinct and still returns the PR's other facts.
    fs::write(rig.project.join("checks-fail"), "")?;
    let output = rig.command(&["pr", "status", "1668", "--json"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(report["data"][0]["head"], HEAD);
    assert!(report["data"][0]["checks_total"].is_null());
    assert_eq!(report["data"][0]["merge"], "clean");
    let output = rig.command(&["pr", "status", "1668"])?;
    assert!(String::from_utf8(output.stdout.bytes)?.contains("checks=unavailable"));
    no_merge(&rig)
}

fn pr_status_preserves_partial_results() -> Result {
    let rig = self::rig()?;
    fs::write(
        rig.project.join("status-1668.json"),
        format!(r#"{{"state":"OPEN","headRefOid":"{HEAD}"}}"#),
    )?;
    let output = rig.command(&["pr", "status", "1668", "9999", "--json"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(report["data"][0]["head"], HEAD);
    assert!(report["data"][1]["error"].is_string());
    Ok(())
}

fn fake_gh(args: &[String]) -> Result<u8> {
    let call = args.join(" ");
    // Real gh rejects JSON fields it does not have; so does this fake.
    if let Some(at) = args.iter().position(|arg| arg == "--json") {
        let known: &[&str] = match (
            args.first().map(String::as_str),
            args.get(1).map(String::as_str),
        ) {
            (Some("pr"), Some("view" | "list")) => &[
                "baseRefName",
                "baseRefOid",
                "headRefName",
                "headRefOid",
                "headRepository",
                "headRepositoryOwner",
                "isCrossRepository",
                "isDraft",
                "mergeCommit",
                "mergeStateStatus",
                "mergeable",
                "mergedAt",
                "number",
                "reviewDecision",
                "state",
                "title",
                "url",
            ],
            (Some("pr"), Some("checks")) => &[
                "bucket",
                "completedAt",
                "description",
                "event",
                "link",
                "name",
                "startedAt",
                "state",
                "workflow",
            ],
            (Some("repo"), Some("view")) => {
                &["defaultBranchRef", "name", "nameWithOwner", "owner", "url"]
            }
            _ => &[],
        };
        for field in args
            .get(at + 1)
            .map(String::as_str)
            .unwrap_or("")
            .split(',')
        {
            if !known.is_empty() && !known.contains(&field) {
                let _ = writeln!(io::stderr(), "Unknown JSON field: {field:?}");
                return Ok(1);
            }
        }
    }
    writeln!(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("gh-calls")?,
        "{call}"
    )?;
    let normalised = call
        .replace("api --include ", "api ")
        .replace(" -H X-GitHub-Api-Version:2026-03-10", "")
        .replace(
            "api -H Accept:application/vnd.github+json -H X-GitHub-Api-Version:2022-11-28 ",
            "api ",
        );
    match normalised.as_str() {
        "api repos/{owner}/{repo}" => {
            io::stdout().write_all(br#"{"default_branch":"main","html_url":"https://github.com/owner/repo","full_name":"owner/repo","owner":{"login":"owner"}}"#)?;
        }
        "api --paginate --slurp repos/{owner}/{repo}/pulls/1668/reviews?per_page=100" => {
            io::stdout().write_all(b"[[]]")?;
        }
        "api repos/{owner}/{repo}/pulls/1668" => {
            if Path::new("read-fail-after-merge").exists() && Path::new("merge-args").exists() {
                writeln!(io::stderr(), "HTTP 502 Bad Gateway")?;
                return Ok(1);
            }
            let head_value: serde_json::Value = serde_json::from_slice(&fs::read("head.json")?)?;
            let next = if Path::new("head-seq").exists() {
                let seq = fs::read_to_string("head-seq")?;
                let mut lines = seq.lines();
                let next = lines.next().unwrap_or(HEAD).to_owned();
                let rest: Vec<&str> = lines.collect();
                if !rest.is_empty() {
                    fs::write("head-seq", rest.join("\n"))?;
                }
                next
            } else if Path::new("head-read").exists() && Path::new("move-head").exists() {
                OTHER.to_owned()
            } else {
                head_value["headRefOid"].as_str().unwrap_or(HEAD).to_owned()
            };
            fs::write("head-read", "")?;
            let merged = Path::new("merged").exists();
            let mergeable = match head_value["mergeable"].as_str() {
                Some("CONFLICTING") => Some(false),
                Some("UNKNOWN") => None,
                _ => Some(true),
            };
            let base = if Path::new("live-pr").exists() {
                fs::read_to_string("base-sha")?
            } else {
                HEAD.to_owned()
            };
            let body = serde_json::json!({"state":if merged {"closed"} else {"open"}, "head":{"sha":next,"ref":"feature","repo":{"full_name":"owner/repo"}}, "base":{"sha":base,"ref":"main","repo":{"full_name":"owner/repo"}}, "mergeable":mergeable,"mergeable_state":"clean","merged_at":if merged {Some("2020-01-01T00:00:00Z")} else {None},"merge_commit_sha":null,"title":"fixture","user":{"login":"fixture-author"}});
            io::stdout().write_all(&serde_json::to_vec(&body)?)?;
        }
        "pr view 1668 --json headRefOid" | "pr view 1668 --json headRefOid,mergeable" => {
            if Path::new("head-seq").exists() {
                // One head per read, the last one repeating: scripted head movement.
                let seq = fs::read_to_string("head-seq")?;
                let mut lines = seq.lines();
                let next = lines.next().unwrap_or(HEAD).to_owned();
                let rest: Vec<&str> = lines.collect();
                if !rest.is_empty() {
                    fs::write("head-seq", rest.join("\n"))?;
                }
                write!(io::stdout(), "{{\"headRefOid\":\"{next}\"}}")?;
            } else if Path::new("head-read").exists() && Path::new("move-head").exists() {
                write!(io::stdout(), "{{\"headRefOid\":\"{OTHER}\"}}")?;
            } else {
                io::stdout().write_all(&fs::read("head.json")?)?;
            }
            fs::write("head-read", "")?;
        }
        "pr view 1668 --json state,baseRefName,headRefName,isCrossRepository,title,mergeCommit,mergedAt" =>
        {
            let state = if Path::new("merged").exists() {
                "MERGED"
            } else {
                "OPEN"
            };
            write!(
                io::stdout(),
                r#"{{"state":"{state}","baseRefName":"main","headRefName":"feature","isCrossRepository":false}}"#
            )?;
        }
        "pr view 1668 --json baseRefName,url" => {
            io::stdout().write_all(
                br#"{"baseRefName":"main","url":"https://github.com/owner/repo/pull/1668"}"#,
            )?;
        }
        value if value.starts_with("api repos/{owner}/{repo}/git/commits/") => {
            let commit = value.rsplit('/').next().ok_or("commit")?;
            let tree = if Path::new("live-pr").exists() {
                let git = provenant_os::git::resolve_git_executable(None)
                    .ok_or("native Git executable")?;
                let output = provenant_os::fixture::git_command_at(git)
                    .current_dir(".")
                    .args(["rev-parse", &format!("{commit}^{{tree}}")])
                    .output()?;
                if !output.status.success() {
                    return Err("cannot resolve live PR commit tree".into());
                }
                String::from_utf8(output.stdout)?.trim().to_owned()
            } else {
                OTHER.to_owned()
            };
            let body = serde_json::json!({"tree":{"sha":tree}});
            io::stdout().write_all(&serde_json::to_vec(&body)?)?;
        }
        "api repos/owner/repo/branches/main" => {
            if Path::new("rate-read-once").exists() {
                fs::remove_file("rate-read-once")?;
                writeln!(io::stdout(), "HTTP/2 429\r\nretry-after: 0\r\n\r\n{{}}")?;
                writeln!(io::stderr(), "HTTP 429")?;
                return Ok(1);
            }
            if Path::new("ruleset-only").exists() {
                io::stdout().write_all(br#"{"protected":true}"#)?;
            } else {
                io::stdout().write_all(br#"{"protected":false}"#)?;
            }
        }
        "api repos/owner/repo/branches/main/protection" => {
            if Path::new("protection-404").exists() {
                io::stdout().write_all(br#"{"message":"Not Found","status":"404"}"#)?;
            } else {
                io::stdout().write_all(br#"{"message":"Branch not protected","status":"404"}"#)?;
            }
            return Ok(1);
        }
        "api --paginate --slurp repos/owner/repo/rules/branches/main?per_page=100" => {
            if Path::new("rules-fail").exists() {
                return Ok(1);
            }
            if Path::new("required.json").exists() {
                io::stdout().write_all(&fs::read("required.json")?)?;
            } else {
                io::stdout().write_all(b"[[]]")?;
            }
        }
        // The CI observer reads one page per request, with response headers.
        value
            if value.starts_with("api repos/owner/repo/commits/")
                && value.contains("/statuses?per_page=100&page=") =>
        {
            let page: usize = value.rsplit("&page=").next().ok_or("page")?.parse()?;
            let pages: Vec<serde_json::Value> = if Path::new("statuses.json").exists() {
                serde_json::from_slice(&fs::read("statuses.json")?)?
            } else {
                Vec::new()
            };
            io::stdout().write_all(b"HTTP/2.0 200 OK\r\n\r\n")?;
            let body = pages
                .get(page - 1)
                .cloned()
                .unwrap_or_else(|| serde_json::json!([]));
            io::stdout().write_all(&serde_json::to_vec(&body)?)?;
        }
        value
            if value.starts_with("api repos/owner/repo/commits/")
                && value.contains("/check-runs?per_page=100&filter=latest&page=") =>
        {
            let commit = value
                .split("/commits/")
                .nth(1)
                .and_then(|part| part.split('/').next())
                .ok_or("check commit")?;
            if Path::new("live-pr").exists() {
                let live_head: serde_json::Value = serde_json::from_slice(&fs::read("head.json")?)?;
                let base = fs::read_to_string("base-sha")?;
                assert!(Some(commit) == live_head["headRefOid"].as_str() || commit == base, "{value}");
            } else {
                assert!(commit == HEAD || commit == OTHER, "{value}");
            }
            let page: usize = value.rsplit("&page=").next().ok_or("page")?.parse()?;
            if Path::new("checks-fail").exists() {
                io::stdout().write_all(b"HTTP/2.0 404 Not Found\r\n\r\n{}")?;
                return Ok(1);
            }
            if Path::new("flaky").exists() {
                fs::remove_file("flaky")?;
                io::stdout().write_all(b"HTTP/2.0 502 Bad Gateway\r\n\r\n{}")?;
                writeln!(io::stderr(), "gh: HTTP 502: Bad Gateway")?;
                return Ok(1);
            }
            io::stdout().write_all(b"HTTP/2.0 200 OK\r\n\r\n")?;
            let file = if commit == OTHER && Path::new("checks-1669.json").exists() {
                "checks-1669.json"
            } else {
                "checks.json"
            };
            let raw = fs::read(file)?;
            let Some(checks) = serde_json::from_slice::<Vec<serde_json::Value>>(&raw)
                .ok()
                .filter(|checks| checks.iter().all(|check| check.get("check_runs").is_none()))
            else {
                // Literal pages, served one per request.
                let pages: Vec<serde_json::Value> = serde_json::from_slice(&raw)?;
                let body = pages
                    .get(page - 1)
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({"check_runs":[]}));
                io::stdout().write_all(&serde_json::to_vec(&body)?)?;
                return Ok(0);
            };
            let runs: Vec<_> = checks.iter().enumerate().map(|(index, check)| {
                let (status, conclusion) = match check["bucket"].as_str().unwrap_or_default() {
                    "pass" => ("completed", "success"), "skipping" => ("completed", "skipped"),
                    "pending" => ("in_progress", ""), "fail" => ("completed", "failure"),
                    "cancel" => ("completed", "cancelled"), _ => ("unknown", ""),
                };
                let name = check["name"].as_str().map(str::to_owned).unwrap_or_else(|| format!("check-{index}"));
                // A far-future registration is a check that has only just appeared.
                let started = if Path::new("fresh-checks").exists() { "2999-01-01T00:00:00Z" } else { "2020-01-01T00:00:00Z" };
                let mut run = serde_json::json!({"name":name,"head_sha":commit,"status":status,"conclusion":conclusion,"started_at":started});
                if let Some(app) = check["app"].as_u64() { run["app"] = serde_json::json!({"id":app}); }
                run
            }).collect();
            io::stdout().write_all(&serde_json::to_vec(
                &serde_json::json!({"check_runs":runs}),
            )?)?;
            if file == "checks.json" && Path::new("checks-next.json").exists() {
                fs::rename("checks-next.json", "checks.json")?;
            }
        }
        "pr checks 1668 --json bucket" | "pr checks 1668 --json name,bucket" => {
            if Path::new("checks-fail").exists() {
                return Ok(8);
            }
            let checks = fs::read("checks.json")?;
            io::stdout().write_all(&checks)?;
            if Path::new("checks-next.json").exists() {
                fs::rename("checks-next.json", "checks.json")?;
            }
            if String::from_utf8_lossy(&checks).contains("pending") {
                return Ok(8);
            }
        }
        "pr checks 1669 --json name,bucket" => {
            // Like gh: the JSON still prints, and pending or failing checks exit non-zero.
            let checks = fs::read_to_string("checks-1669.json")?;
            write!(io::stdout(), "{checks}")?;
            return Ok(if checks.contains("pending") { 8 } else { 0 });
        }
        value
            if value.starts_with("pr view ")
                && value.ends_with(
                    "--json state,headRefOid,mergeStateStatus,reviewDecision,isDraft,url",
                ) =>
        {
            let path = format!("status-{}.json", args[2]);
            let mut view: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
            view["url"] = format!("https://github.com/owner/repo/pull/{}", args[2]).into();
            io::stdout().write_all(&serde_json::to_vec(&view)?)?;
            let next = format!("status-{}.next.json", args[2]);
            if Path::new(&next).exists() {
                fs::rename(next, path)?;
            }
        }
        value if value.starts_with("api --method PUT repos/{owner}/{repo}/pulls/1668/merge ") => {
            fs::write("merge-args", args.join("\n") + "\n")?;
            for (flag, status) in [("merge-conflict", 409), ("merge-not-allowed", 405)] {
                if Path::new(flag).exists() {
                    writeln!(
                        io::stdout(),
                        "HTTP/2 {status}\r\nX-RateLimit-Remaining: 0\r\n\r\n{{}}"
                    )?;
                    writeln!(io::stderr(), "HTTP {status}")?;
                    return Ok(1);
                }
            }
            if Path::new("rate-once").exists() || Path::new("rate-always").exists() {
                // A secondary limit sends Retry-After; an exhausted primary
                // window sends a zero remaining count and its reset instead.
                let headers = if Path::new("rate-once").exists() {
                    fs::remove_file("rate-once")?;
                    "HTTP/2 429\r\nRetry-After: 0\r\nX-RateLimit-Reset: 4102444800"
                } else {
                    "HTTP/2 403\r\nX-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: 4102444800"
                };
                writeln!(io::stdout(), "{headers}\r\n\r\n{{}}")?;
                writeln!(io::stderr(), "API rate limit exceeded")?;
                return Ok(1);
            }
            if Path::new("merge-fail").exists() {
                return Ok(1);
            }
            // A queued merge has not happened yet; anything else has.
            if !Path::new("merge-queue").exists() {
                fs::write("merged", "")?;
            }
            if Path::new("merge-delay").exists() {
                // GitHub merged, but the response consumes the remaining budget.
                #[allow(clippy::disallowed_methods)] // Bound fixture lifetime if cleanup fails.
                std::thread::park_timeout(std::time::Duration::from_secs(120));
            }
            if Path::new("merge-timeout").exists() {
                // The merge took effect, but the reply was lost.
                writeln!(io::stderr(), "gh: HTTP 502: Bad Gateway")?;
                return Ok(1);
            }
            write!(
                io::stdout(),
                "{{\"merged\":{}}}",
                !Path::new("merge-queue").exists()
            )?;
        }
        _ => return Err(format!("unexpected gh arguments: {call}").into()),
    }
    Ok(0)
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let result = if args
        .first()
        .is_some_and(|arg| matches!(arg.as_str(), "pr" | "api"))
    {
        fake_gh(&args)
    } else {
        type Test = (&'static str, fn() -> Result);
        let tests: &[Test] = &[
            (
                "pr_status_preserves_partial_results",
                pr_status_preserves_partial_results,
            ),
            (
                "pr_watch_reports_the_verified_head_and_reads_ci_through_the_observer",
                pr_watch_reports_the_verified_head_and_reads_ci_through_the_observer,
            ),
            (
                "pr_watch_failure_and_head_changes_report_fail_with_head",
                pr_watch_failure_and_head_changes_report_fail_with_head,
            ),
            (
                "land_refuses_unpassed_missing_and_malformed_checks",
                land_refuses_unpassed_missing_and_malformed_checks,
            ),
            (
                "land_refuses_head_changes_before_and_after_checks",
                land_refuses_head_changes_before_and_after_checks,
            ),
            (
                "land_plain_singleton_uses_batch_driver",
                land_plain_singleton_uses_batch_driver,
            ),
            (
                "land_explains_poll_and_budget_sources",
                land_explains_poll_and_budget_sources,
            ),
            (
                "land_refuses_a_rest_merge_409_as_head_moved",
                land_refuses_a_rest_merge_409_as_head_moved,
            ),
            (
                "land_refuses_a_rest_merge_405_with_a_next_step",
                land_refuses_a_rest_merge_405_with_a_next_step,
            ),
            (
                "pr_external_failures_and_invalid_arguments_fail_closed",
                pr_external_failures_and_invalid_arguments_fail_closed,
            ),
            (
                "pr_status_prints_one_compact_line_per_pull_request",
                pr_status_prints_one_compact_line_per_pull_request,
            ),
        ];
        provenant_supervisor::fixture::run_libtest(
            &args.iter().map(Into::into).collect::<Vec<_>>(),
            &tests.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            |name| {
                let (_, test) = tests
                    .iter()
                    .find(|(candidate, _)| *candidate == name)
                    .ok_or_else(|| io::Error::other("unknown PR test"))?;
                test().map_err(|error| io::Error::other(error.to_string()))
            },
        )
        .map(|()| 0)
        .map_err(Into::into)
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            let _ = writeln!(io::stderr(), "{error}");
            ExitCode::FAILURE
        }
    }
}
