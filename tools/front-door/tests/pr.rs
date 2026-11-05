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
        r#"[{"bucket":"pass"},{"bucket":"skipping"}]"#,
    )?;
    Ok(rig)
}
#[path = "support/rest_calls.rs"]
mod rest_calls;
use rest_calls::rest_only;
fn assert_rest_only(rig: &TestRig) -> Result {
    let calls = fs::read_to_string(rig.project.join("gh-calls"))?;
    assert!(!calls.is_empty() && rest_only(&calls), "{calls}");
    Ok(())
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

fn pr_watch_reports_the_verified_head_and_waits_via_gh() -> Result {
    let rig = self::rig()?;
    let output = rig.command(&["pr", "watch", "1668", "--full"])?;
    assert_eq!(code(&output), 0, "{:?}", output);
    assert_eq!(
        String::from_utf8(output.stdout.bytes)?,
        format!("pass #1668 {HEAD}\n")
    );
    assert!(rig.project.join("watched").exists());
    no_merge(&rig)
}

fn pr_watch_failure_and_head_changes_report_fail_with_head() -> Result {
    for flag in ["watch-fail", "move-head"] {
        let rig = self::rig()?;
        fs::write(rig.project.join(flag), "")?;
        let output = rig.command(&["pr", "watch", "1668"])?;
        assert_ne!(code(&output), 0, "{flag}");
        let error = String::from_utf8(output.stderr.bytes)?;
        assert!(error.contains(&format!("fail #1668 {HEAD}")), "{error}");
        no_merge(&rig)?;
    }
    Ok(())
}

fn land_refuses_unpassed_missing_and_malformed_checks() -> Result {
    for bucket in [
        "pending",
        "fail",
        "cancel",
        "unknown",
        "skipping",
        "empty",
        "malformed",
        "pass-pending",
        "pass-fail",
    ] {
        let rig = self::rig()?;
        let checks = match bucket {
            "empty" => "[]".into(),
            "malformed" => "invalid json".into(),
            "pass-pending" => r#"[{"bucket":"pass"},{"bucket":"pending"}]"#.into(),
            "pass-fail" => r#"[{"bucket":"pass"},{"bucket":"fail"}]"#.into(),
            _ => format!(r#"[{{"bucket":"{bucket}"}}]"#),
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

fn land_reports_a_queued_merge_as_pending_with_its_own_exit_code() -> Result {
    let rig = self::rig()?;
    fs::write(rig.project.join("merge-queue"), "")?;
    let output = rig.command(&["land", "1668", "--head", HEAD])?;
    assert_eq!(code(&output), 5, "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout.bytes)?,
        format!("pending #1668 {HEAD} (OPEN)\nnext: provenant pr status 1668 --wait\n")
    );
    let json = rig.command(&["land", "1668", "--head", HEAD, "--json"])?;
    assert_eq!(code(&json), 5);
    let report: serde_json::Value = serde_json::from_slice(&json.stdout.bytes)?;
    assert_eq!(report["status"], "pending", "{report}");
    assert_eq!(report["data"]["result"], "pending");
    assert_eq!(report["data"]["merged"], false);
    assert_rest_only(&rig)?;
    Ok(())
}

fn land_reports_landed_when_the_merge_call_errors_after_taking_effect() -> Result {
    // The merge call times out (a 502 here, not a real 60 s wait) after GitHub merged.
    let rig = self::rig()?;
    fs::write(rig.project.join("merge-timeout"), "")?;
    let output = merge(&rig)?;
    assert_eq!(code(&output), 0, "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(report["data"]["merged"], true, "{report}");
    assert_eq!(report["data"]["result"], "pass");
    assert!(report["data"]["merge_note"].is_string(), "{report}");
    assert_ne!(report["error"]["code"], "MERGE_FAILED");
    // The same error with no effect is still a failure.
    let rig = self::rig()?;
    fs::write(rig.project.join("merge-fail"), "")?;
    let report: serde_json::Value = serde_json::from_slice(&merge(&rig)?.stdout.bytes)?;
    assert_eq!(report["error"]["code"], "MERGE_FAILED", "{report}");
    Ok(())
}

fn land_reports_unknown_when_a_failed_merge_cannot_be_confirmed() -> Result {
    // A merge error that is not a refusal may still have taken effect, so an
    // unconfirmable state must not be reported as a plain merge failure.
    let rig = self::rig()?;
    fs::write(rig.project.join("merge-fail"), "")?;
    fs::write(rig.project.join("read-fail-after-merge"), "")?;
    let output = rig.command(&["land", "1668", "--head", HEAD, "--wait=5s", "--json"])?;
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(report["error"]["code"], "MERGE_STATE_UNKNOWN", "{report}");
    // A refusal proves nothing merged, so it keeps its own code.
    let rig = self::rig()?;
    fs::write(rig.project.join("merge-conflict"), "")?;
    fs::write(rig.project.join("read-fail-after-merge"), "")?;
    let output = rig.command(&["land", "1668", "--head", HEAD, "--wait=5s", "--json"])?;
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(report["error"]["code"], "HEAD_MOVED", "{report}");
    Ok(())
}

fn land_reports_unknown_when_the_merge_exhausts_the_wait_budget() -> Result {
    let rig = self::rig()?;
    fs::write(rig.project.join("merge-delay"), "")?;
    let output = rig.command(&["land", "1668", "--head", HEAD, "--wait=5s", "--json"])?;
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert!(
        rig.project.join("merge-args").exists(),
        "merge was attempted"
    );
    assert!(rig.project.join("merged").exists(), "merge took effect");
    assert_eq!(report["error"]["code"], "MERGE_STATE_UNKNOWN", "{report}");
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("merge may have completed"),
        "{report}"
    );
    Ok(())
}

fn merge_calls(rig: &TestRig) -> Result<usize> {
    Ok(fs::read_to_string(rig.project.join("gh-calls"))?
        .lines()
        .filter(|line| line.contains("--method PUT"))
        .count())
}

fn land_retries_a_rate_limited_merge() -> Result {
    let rig = self::rig()?;
    fs::write(rig.project.join("rate-once"), "")?;
    let output = rig.command(&["land", "1668", "--head", HEAD, "--wait=30s", "--json"])?;
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(code(&output), 0, "{report}");
    assert_eq!(report["data"]["merged"], true, "{report}");
    assert_eq!(merge_calls(&rig)?, 2);
    assert_rest_only(&rig)?;
    Ok(())
}

fn land_reports_reset_and_exact_retry_when_merge_rate_limit_exhausts_budget() -> Result {
    let rig = self::rig()?;
    fs::write(rig.project.join("rate-always"), "")?;
    let output = rig.command(&["land", "1668", "--head", HEAD, "--wait=5s", "--json"])?;
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(report["error"]["code"], "RATE_LIMITED", "{report}");
    let retry = format!(
        "provenant land 1668 --head {HEAD} --wait=5s --state-dir '{}' --json",
        rig.state.display()
    );
    assert_eq!(report["error"]["repair"], retry, "{report}");
    let message = report["error"]["message"].as_str().ok_or("error message")?;
    assert!(message.contains("2100-01-01"), "{report}");
    assert!(message.contains(&format!("next: {retry}")), "{report}");
    assert_eq!(report["data"]["merged"], false, "{report}");
    assert_eq!(report["data"]["steps"], serde_json::json!([]));
    assert!(!rig.project.join("merged").exists());
    assert_rest_only(&rig)?;
    Ok(())
}

fn rest_merge_refusal(flag: &str, expected: &str) -> Result {
    let rig = self::rig()?;
    fs::write(rig.project.join(flag), "")?;
    let output = merge(&rig)?;
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

fn land_refuses_when_the_head_moves_after_checks_were_pending() -> Result {
    // Second wait read sees B; the A-B-A script returns to A afterwards and
    // must not be treated as a head that never moved.
    for script in [
        "1111111111111111111111111111111111111111\n1111111111111111111111111111111111111111\n2222222222222222222222222222222222222222",
        "1111111111111111111111111111111111111111\n1111111111111111111111111111111111111111\n2222222222222222222222222222222222222222\n1111111111111111111111111111111111111111",
    ] {
        let rig = self::rig()?;
        fs::write(rig.project.join("head-seq"), script)?;
        fs::write(
            rig.project.join("checks.json"),
            r#"[{"name":"build","bucket":"pending"}]"#,
        )?;
        let output = rig.command(&["land", "1668", "--head", HEAD, "--wait=20s", "--json"])?;
        let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
        assert_eq!(report["error"]["code"], "HEAD_MOVED", "{report}");
        no_merge(&rig)?;
    }
    Ok(())
}

fn land_refuses_a_failing_legacy_status_and_unreadable_ci_pages() -> Result {
    let run = |name: &str, conclusion: &str| serde_json::json!({"name":name,"head_sha":HEAD,"status":"completed","conclusion":conclusion,"started_at":"2020-01-01T00:00:00Z"});
    // A legacy commit status that fails blocks landing even when every check run passes.
    let rig = self::rig()?;
    fs::write(
        rig.project.join("statuses.json"),
        r#"[[{"context":"external","state":"failure","created_at":"2020-01-01T00:00:00Z"}]]"#,
    )?;
    let report: serde_json::Value = serde_json::from_slice(&merge(&rig)?.stdout.bytes)?;
    assert_eq!(report["error"]["code"], "CI_FAILED", "{report}");
    no_merge(&rig)?;
    // A failure on the second page is seen.
    let rig = self::rig()?;
    fs::write(
        rig.project.join("checks.json"),
        serde_json::to_vec(&serde_json::json!([
            {"total_count":2,"check_runs":[run("a","success")]},
            {"total_count":2,"check_runs":[run("b","failure")]},
        ]))?,
    )?;
    let report: serde_json::Value = serde_json::from_slice(&merge(&rig)?.stdout.bytes)?;
    assert_eq!(report["error"]["code"], "CI_FAILED", "{report}");
    no_merge(&rig)?;
    // A lost page is not a pass.
    let rig = self::rig()?;
    fs::write(
        rig.project.join("checks.json"),
        serde_json::to_vec(&serde_json::json!([
            {"total_count":3,"check_runs":[run("a","success"),run("b","success")]},
        ]))?,
    )?;
    assert_ne!(code(&merge(&rig)?), 0);
    no_merge(&rig)?;
    // Both pages passing lands.
    let rig = self::rig()?;
    fs::write(
        rig.project.join("checks.json"),
        serde_json::to_vec(&serde_json::json!([
            {"total_count":2,"check_runs":[run("a","success")]},
            {"total_count":2,"check_runs":[run("b","success")]},
        ]))?,
    )?;
    assert_eq!(code(&merge(&rig)?), 0);
    assert!(rig.project.join("merge-args").exists());
    Ok(())
}

fn land_fails_closed_when_protection_and_rulesets_are_unreadable() -> Result {
    // A 404 that is not "Branch not protected" is not an absence of policy.
    let rig = self::rig()?;
    fs::write(rig.project.join("ruleset-only"), "")?;
    fs::write(rig.project.join("protection-404"), "")?;
    fs::write(rig.project.join("rules-fail"), "")?;
    let report: serde_json::Value = serde_json::from_slice(&merge(&rig)?.stdout.bytes)?;
    assert_eq!(report["error"]["code"], "CI_POLICY_UNAVAILABLE", "{report}");
    no_merge(&rig)
}

fn land_treats_a_skipped_required_check_as_unavailable() -> Result {
    let rig = self::rig()?;
    fs::write(rig.project.join("ruleset-only"), "")?;
    fs::write(
        rig.project.join("required.json"),
        r#"[[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"build"}]}}]]"#,
    )?;
    fs::write(
        rig.project.join("checks.json"),
        r#"[{"name":"build","bucket":"skipping"},{"name":"lint","bucket":"pass"}]"#,
    )?;
    let report: serde_json::Value = serde_json::from_slice(&merge(&rig)?.stdout.bytes)?;
    assert_eq!(report["error"]["code"], "CI_UNAVAILABLE", "{report}");
    no_merge(&rig)
}

fn land_matches_a_required_check_on_its_integration() -> Result {
    let required = |app: u64| {
        format!(
            r#"[[{{"type":"required_status_checks","parameters":{{"required_status_checks":[{{"context":"build","integration_id":{app}}}]}}}}]]"#
        )
    };
    // The name is reported, but by another app: it does not satisfy the pin.
    let rig = self::rig()?;
    fs::write(rig.project.join("ruleset-only"), "")?;
    fs::write(rig.project.join("required.json"), required(15368))?;
    fs::write(
        rig.project.join("checks.json"),
        r#"[{"name":"build","bucket":"pass","app":999}]"#,
    )?;
    let output = rig.command(&["land", "1668", "--head", HEAD, "--wait=1s", "--json"])?;
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(report["error"]["code"], "CI_TIMEOUT", "{report}");
    no_merge(&rig)?;
    // Reported by the pinned app, it lands.
    let rig = self::rig()?;
    fs::write(rig.project.join("ruleset-only"), "")?;
    fs::write(rig.project.join("required.json"), required(15368))?;
    fs::write(
        rig.project.join("checks.json"),
        r#"[{"name":"build","bucket":"pass","app":15368}]"#,
    )?;
    assert_eq!(code(&merge(&rig)?), 0);
    assert!(rig.project.join("merge-args").exists());
    Ok(())
}

fn land_retries_a_rate_limited_policy_read() -> Result {
    let rig = self::rig()?;
    fs::write(rig.project.join("rate-read-once"), "")?;
    let output = rig.command(&["land", "1668", "--head", HEAD, "--wait=30s", "--json"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let calls = fs::read_to_string(rig.project.join("gh-calls"))?;
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.ends_with("repos/owner/repo/branches/main"))
            .count(),
        2
    );
    assert!(rig.project.join("merge-args").exists());
    Ok(())
}

fn land_retries_transient_gh_reads_inside_the_wait() -> Result {
    let rig = self::rig()?;
    fs::write(rig.project.join("flaky"), "")?;
    let output = rig.command(&["land", "1668", "--head", HEAD, "--wait=30s", "--json"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    assert!(rig.project.join("merge-args").exists());
    assert!(
        !rig.project.join("flaky").exists(),
        "the failure was served"
    );
    Ok(())
}

fn land_waits_for_new_checks_to_stop_registering_when_no_check_is_required() -> Result {
    let rig = self::rig()?;
    fs::write(rig.project.join("fresh-checks"), "")?;
    // A single green check that has only just registered is not enough.
    let output = rig.command(&["land", "1668", "--head", HEAD, "--wait=1s", "--json"])?;
    assert_eq!(code(&output), 124, "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(report["error"]["code"], "CI_TIMEOUT", "{report}");
    no_merge(&rig)?;
    let output = rig.command(&["land", "1668", "--head", HEAD, "--json"])?;
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(report["error"]["code"], "CI_PENDING", "{report}");
    no_merge(&rig)?;
    // Once the newest registration is old enough, it lands.
    fs::remove_file(rig.project.join("fresh-checks"))?;
    assert_eq!(code(&merge(&rig)?), 0);
    assert!(rig.project.join("merge-args").exists());
    Ok(())
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
        "schema_version = 1\n[domains.pr.land]\nbatch_wait_seconds = 0\n",
    )?;
    let output = rig.command(&["land", "1668", "--json"])?;
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_ne!(code(&output), 0, "an unapproved PR must not land");
    assert_eq!(report["data"]["outcomes"]["1668"], "excluded", "{report}");
    assert_eq!(report["error"]["code"], "LAND_REFUSED", "{report}");
    let calls = fs::read_to_string(rig.project.join("gh-calls"))?.replace("api --include ", "api ");
    assert!(calls.contains("api repos/{owner}/{repo}"), "{calls}");
    assert!(
        calls.contains("api repos/{owner}/{repo}/pulls/1668"),
        "{calls}"
    );
    assert!(!rig.project.join("merge-args").exists());
    assert_rest_only(&rig)?;
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
    assert_eq!(default["data"]["fields"]["poll_seconds"]["value"], 30);
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
    assert_eq!(cli["data"]["retry_budget_seconds"], 90);
    assert_eq!(cli["data"]["retry_budget_source"], "--wait");
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

fn land_guards_head_and_uses_rest_merge() -> Result {
    for email in [None, Some("123+fixture@users.noreply.github.com")] {
        let rig = self::rig()?;
        if let Some(email) = email {
            let git = provenant_os::git::git_executable().ok_or("Git executable")?;
            assert_eq!(
                code(&rig.execute(&git, &["config", "user.email", email], None)?),
                0
            );
        }
        let output = merge(&rig)?;
        assert_eq!(code(&output), 0, "{:?}", output);
        let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
        assert_eq!(report["data"]["head"], HEAD);
        assert_eq!(report["data"]["result"], "pass");
        assert_eq!(report["data"]["merged"], true);
        assert_eq!(report["data"]["state"], "MERGED");
        let args = fs::read_to_string(rig.project.join("merge-args"))?;
        assert!(args.contains(&format!("sha={HEAD}")), "{args}");
        assert!(args.contains("merge_method=merge"), "{args}");
        assert!(!args.contains("author-email"), "{args}");
        assert!(!rig.project.join("watched").exists());
        assert_rest_only(&rig)?;
        let calls = fs::read_to_string(rig.project.join("gh-calls"))?;
        assert!(calls.contains("X-GitHub-Api-Version:2026-03-10"), "{calls}");
    }
    assert!(!rest_only("pr list --head branch"));
    assert!(!rest_only("api graphql -f query=x"));
    assert!(!rest_only(
        "api repos/{owner}/{repo}/pulls -f query=x graphql"
    ));
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
        vec!["land", "1668", "--head", "abc"],
        vec!["pr", "watch", "0"],
        vec!["pr", "watch", "--admin"],
    ] {
        assert_eq!(code(&rig.command(&args)?), 2, "{args:?}");
    }
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
    // Counts first, the PR that needs action next, then one typed command.
    assert_eq!(
        String::from_utf8(output.stdout.bytes)?,
        format!(
            "2 PRs: 1 failing, 1 ready (1 need nothing)\n\
             #1669 OPEN {OTHER} checks=2/4 fail=1 pending=1 merge=blocked review=none draft\n\
             next: gh pr checks 1669\n"
        )
    );
    let output = rig.command(&["pr", "status", "1668", "1669", "--full"])?;
    assert_eq!(
        String::from_utf8(output.stdout.bytes)?,
        format!(
            "#1668 OPEN {HEAD} checks=2/2 merge=clean review=approved\n\
             #1669 OPEN {OTHER} checks=2/4 fail=1 pending=1 merge=blocked review=none draft\n\
             next: gh pr checks 1669\n"
        )
    );
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

fn land_wait_is_bounded_and_requires_the_exact_head() -> Result {
    for (bucket, expected, exit) in [("pending", "CI_TIMEOUT", 124), ("fail", "CI_FAILED", 1)] {
        // A conflicting PR never starts CI: name it, with a next step, instead of timing out.
        for (checks, expected) in [
            (r#"[]"#, "CI_CONFLICT"),
            (r#"[{"name":"build","bucket":"pending"}]"#, "CI_CONFLICT"),
        ] {
            let rig = self::rig()?;
            fs::write(rig.project.join("checks.json"), checks)?;
            fs::write(
                rig.project.join("head.json"),
                format!(r#"{{"headRefOid":"{HEAD}","mergeable":"CONFLICTING"}}"#),
            )?;
            let output = rig.command(&["land", "1668", "--head", HEAD, "--wait=1s", "--json"])?;
            let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
            assert_eq!(report["error"]["code"], expected, "{report}");
            assert!(
                report["error"]["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("next:"))
            );
            no_merge(&rig)?;
        }
        // No check ever registered: unscheduled, not a generic timeout.
        let rig = self::rig()?;
        fs::write(rig.project.join("checks.json"), "[]")?;
        let output = rig.command(&["land", "1668", "--head", HEAD, "--wait=1s", "--json"])?;
        let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
        assert_eq!(report["error"]["code"], "CI_NOT_SCHEDULED", "{report}");
        no_merge(&rig)?;
        let rig = self::rig()?;
        fs::write(
            rig.project.join("checks.json"),
            format!(r#"[{{"name":"build","bucket":"{bucket}"}}]"#),
        )?;
        let output = rig.command(&["land", "1668", "--head", HEAD, "--wait=1s", "--json"])?;
        assert_eq!(
            code(&output),
            exit,
            "{}",
            String::from_utf8_lossy(&output.stdout.bytes)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
        assert_eq!(report["error"]["code"], expected, "{report}");
        no_merge(&rig)?;
    }
    let rig = self::rig()?;
    fs::write(rig.project.join("move-head"), "")?;
    let output = rig.command(&["land", "1668", "--head", HEAD, "--wait=10s", "--json"])?;
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(report["error"]["code"], "HEAD_MOVED", "{report}");
    no_merge(&rig)?;
    let rig = self::rig()?;
    fs::write(
        rig.project.join("required.json"),
        r#"[[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"not-registered"}]}}]]"#,
    )?;
    let output = rig.command(&["land", "1668", "--wait=1s", "--json"])?;
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(report["error"]["code"], "CI_TIMEOUT", "{report}");
    no_merge(&rig)
}

fn land_wait_handles_pending_checks_then_passes() -> Result {
    let rig = self::rig()?;
    fs::write(rig.project.join("ruleset-only"), "")?;
    fs::write(
        rig.project.join("required.json"),
        r#"[[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"build"}]}}]]"#,
    )?;
    fs::write(
        rig.project.join("checks.json"),
        r#"[{"name":"build","bucket":"pending"}]"#,
    )?;
    fs::write(
        rig.project.join("checks-next.json"),
        r#"[{"name":"build","bucket":"pass"}]"#,
    )?;
    let output = rig.command(&["land", "1668", "--head", HEAD, "--wait=10s", "--poll", "1"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    assert!(rig.project.join("merge-args").exists());
    assert!(output.stdout.bytes.len() <= 2048);
    assert!(output.stderr.bytes.len() <= 2048);
    Ok(())
}

fn pr_status_waits_for_change_and_preserves_partial_results() -> Result {
    let rig = self::rig()?;
    fs::write(
        rig.project.join("status-1668.json"),
        format!(r#"{{"state":"OPEN","headRefOid":"{HEAD}"}}"#),
    )?;
    fs::write(
        rig.project.join("status-1668.next.json"),
        format!(r#"{{"state":"OPEN","headRefOid":"{OTHER}"}}"#),
    )?;
    let output = rig.command(&["pr", "status", "1668", "--wait=10s", "--json"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(report["data"][0]["head"], OTHER);
    let calls = fs::read_to_string(rig.project.join("gh-calls"))?;
    assert_eq!(calls.lines().count(), 2);
    assert!(!calls.contains("pr checks"), "{calls}");
    let output = rig.command(&["pr", "status", "1668", "9999", "--json"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(report["data"][0]["head"], OTHER);
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
                "statusCheckRollup",
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
    let normalised = call.replace("api --include ", "api ");
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
                if !rest.is_empty() { fs::write("head-seq", rest.join("\n"))?; }
                next
            } else if Path::new("head-read").exists() && Path::new("move-head").exists() {
                OTHER.to_owned()
            } else { head_value["headRefOid"].as_str().unwrap_or(HEAD).to_owned() };
            fs::write("head-read", "")?;
            let merged = Path::new("merged").exists();
            let mergeable = match head_value["mergeable"].as_str() { Some("CONFLICTING") => Some(false), Some("UNKNOWN") => None, _ => Some(true) };
            let body = serde_json::json!({"state":if merged {"closed"} else {"open"}, "head":{"sha":next,"ref":"feature","repo":{"full_name":"owner/repo"}}, "base":{"sha":HEAD,"ref":"main","repo":{"full_name":"owner/repo"}}, "mergeable":mergeable,"mergeable_state":"clean","merged_at":if merged {Some("2020-01-01T00:00:00Z")} else {None},"merge_commit_sha":null,"title":"fixture"});
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
        "pr view 1668 --json state,baseRefName,headRefName,isCrossRepository,title,mergeCommit,mergedAt" => {
            let state = if Path::new("merged").exists() { "MERGED" } else { "OPEN" };
            write!(io::stdout(), r#"{{"state":"{state}","baseRefName":"main","headRefName":"feature","isCrossRepository":false}}"#)?;
        }
        "pr checks 1668 --watch" => {
            fs::write("watched", "")?;
            if Path::new("watch-fail").exists() {
                return Ok(1);
            }
        }
        "pr view 1668 --json baseRefName,url" => {
            io::stdout().write_all(br#"{"baseRefName":"main","url":"https://github.com/owner/repo/pull/1668"}"#)?;
        }
        "api repos/owner/repo/branches/main" => {
            if Path::new("rate-read-once").exists() {
                fs::remove_file("rate-read-once")?;
                writeln!(io::stdout(), "HTTP/2 429\r\nretry-after: 0\r\n\r\n{{}}")?;
                writeln!(io::stderr(), "HTTP 429")?;
                return Ok(1);
            }
            if Path::new("ruleset-only").exists() { io::stdout().write_all(br#"{"protected":true}"#)?; }
            else { io::stdout().write_all(br#"{"protected":false}"#)?; }
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
            if Path::new("rules-fail").exists() { return Ok(1); }
            if Path::new("required.json").exists() { io::stdout().write_all(&fs::read("required.json")?)?; }
            else { io::stdout().write_all(b"[[]]")?; }
        }
        value if value.starts_with("api --paginate --slurp repos/owner/repo/commits/")
            && value.ends_with("/statuses?per_page=100") => {
            assert!(value.contains(HEAD));
            if Path::new("statuses.json").exists() {
                io::stdout().write_all(&fs::read("statuses.json")?)?;
            } else {
                io::stdout().write_all(b"[[]]")?;
            }
        }
        value if value.starts_with("api --paginate --slurp repos/owner/repo/commits/")
            && value.ends_with("/check-runs?per_page=100&filter=latest") => {
            assert!(value.contains(HEAD));
            if Path::new("checks-fail").exists() { return Ok(1); }
            if Path::new("flaky").exists() {
                fs::remove_file("flaky")?;
                writeln!(io::stderr(), "gh: HTTP 502: Bad Gateway")?;
                return Ok(1);
            }
            let raw = fs::read("checks.json")?;
            let Some(checks) = serde_json::from_slice::<Vec<serde_json::Value>>(&raw)
                .ok()
                .filter(|checks| checks.iter().all(|check| check.get("check_runs").is_none()))
            else {
                io::stdout().write_all(&raw)?;
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
                let mut run = serde_json::json!({"name":name,"head_sha":HEAD,"status":status,"conclusion":conclusion,"started_at":started});
                if let Some(app) = check["app"].as_u64() { run["app"] = serde_json::json!({"id":app}); }
                run
            }).collect();
            io::stdout().write_all(&serde_json::to_vec(&serde_json::json!([{"check_runs":runs}]))?)?;
            if Path::new("checks-next.json").exists() { fs::rename("checks-next.json", "checks.json")?; }
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
            if String::from_utf8_lossy(&checks).contains("pending") { return Ok(8); }
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
                    "--json state,headRefOid,mergeStateStatus,reviewDecision,isDraft,statusCheckRollup",
                ) =>
        {
            let path = format!("status-{}.json", args[2]);
            let mut view: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
            if view.get("statusCheckRollup").is_none() {
                let checks_path = if args[2] == "1668" { "checks.json".into() } else { format!("checks-{}.json", args[2]) };
                view["statusCheckRollup"] = if Path::new("checks-fail").exists() { serde_json::Value::Null } else {
                    let checks: Vec<serde_json::Value> = serde_json::from_slice(&fs::read(checks_path)?)?;
                    serde_json::Value::Array(checks.into_iter().map(|check| {
                        let (status, conclusion) = match check["bucket"].as_str().unwrap_or_default() {
                            "pass" => ("COMPLETED", "SUCCESS"), "skipping" => ("COMPLETED", "SKIPPED"),
                            "pending" => ("IN_PROGRESS", ""), _ => ("COMPLETED", "FAILURE")
                        };
                        serde_json::json!({"__typename":"CheckRun","name":"build","status":status,"conclusion":conclusion})
                    }).collect())
                };
            }
            io::stdout().write_all(&serde_json::to_vec(&view)?)?;
            let next = format!("status-{}.next.json", args[2]);
            if Path::new(&next).exists() { fs::rename(next, path)?; }
        }
        value
            if value.starts_with("api --method PUT repos/{owner}/{repo}/pulls/1668/merge ") =>
        {
            fs::write("merge-args", args.join("\n") + "\n")?;
            for (flag, status) in [("merge-conflict", 409), ("merge-not-allowed", 405)] {
                if Path::new(flag).exists() {
                    writeln!(io::stdout(), "HTTP/2 {status}\r\nX-RateLimit-Remaining: 0\r\n\r\n{{}}")?;
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
            write!(io::stdout(), "{{\"merged\":{}}}", !Path::new("merge-queue").exists())?;
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
                "land_wait_is_bounded_and_requires_the_exact_head",
                land_wait_is_bounded_and_requires_the_exact_head,
            ),
            (
                "land_wait_handles_pending_checks_then_passes",
                land_wait_handles_pending_checks_then_passes,
            ),
            (
                "pr_status_waits_for_change_and_preserves_partial_results",
                pr_status_waits_for_change_and_preserves_partial_results,
            ),
            (
                "pr_watch_reports_the_verified_head_and_waits_via_gh",
                pr_watch_reports_the_verified_head_and_waits_via_gh,
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
                "land_reports_a_queued_merge_as_pending_with_its_own_exit_code",
                land_reports_a_queued_merge_as_pending_with_its_own_exit_code,
            ),
            (
                "land_reports_landed_when_the_merge_call_errors_after_taking_effect",
                land_reports_landed_when_the_merge_call_errors_after_taking_effect,
            ),
            (
                "land_refuses_when_the_head_moves_after_checks_were_pending",
                land_refuses_when_the_head_moves_after_checks_were_pending,
            ),
            (
                "land_refuses_a_failing_legacy_status_and_unreadable_ci_pages",
                land_refuses_a_failing_legacy_status_and_unreadable_ci_pages,
            ),
            (
                "land_fails_closed_when_protection_and_rulesets_are_unreadable",
                land_fails_closed_when_protection_and_rulesets_are_unreadable,
            ),
            (
                "land_treats_a_skipped_required_check_as_unavailable",
                land_treats_a_skipped_required_check_as_unavailable,
            ),
            (
                "land_matches_a_required_check_on_its_integration",
                land_matches_a_required_check_on_its_integration,
            ),
            (
                "land_retries_a_rate_limited_policy_read",
                land_retries_a_rate_limited_policy_read,
            ),
            (
                "land_retries_transient_gh_reads_inside_the_wait",
                land_retries_transient_gh_reads_inside_the_wait,
            ),
            (
                "land_waits_for_new_checks_to_stop_registering_when_no_check_is_required",
                land_waits_for_new_checks_to_stop_registering_when_no_check_is_required,
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
                "land_retries_a_rate_limited_merge",
                land_retries_a_rate_limited_merge,
            ),
            (
                "land_reports_reset_and_exact_retry_when_merge_rate_limit_exhausts_budget",
                land_reports_reset_and_exact_retry_when_merge_rate_limit_exhausts_budget,
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
                "land_reports_unknown_when_the_merge_exhausts_the_wait_budget",
                land_reports_unknown_when_the_merge_exhausts_the_wait_budget,
            ),
            (
                "land_reports_unknown_when_a_failed_merge_cannot_be_confirmed",
                land_reports_unknown_when_a_failed_merge_cannot_be_confirmed,
            ),
            (
                "land_guards_head_and_uses_rest_merge",
                land_guards_head_and_uses_rest_merge,
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
