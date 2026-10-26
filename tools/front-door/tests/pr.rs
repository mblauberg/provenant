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
fn merge(rig: &TestRig) -> Result<provenant_supervisor::exec::ExecOutcome> {
    rig.command(&["pr", "merge", "1668", "--head", HEAD, "--json"])
}
fn no_merge(rig: &TestRig) -> Result {
    assert!(!rig.project.join("merge-args").exists());
    let calls = fs::read_to_string(rig.project.join("gh-calls"))?;
    assert!(!calls.contains("pr merge"), "{calls}");
    Ok(())
}

fn pr_watch_reports_the_verified_head_and_waits_via_gh() -> Result {
    let rig = rig()?;
    let output = rig.command(&["pr", "watch", "1668"])?;
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
        let rig = rig()?;
        fs::write(rig.project.join(flag), "")?;
        let output = rig.command(&["pr", "watch", "1668"])?;
        assert_ne!(code(&output), 0, "{flag}");
        let error = String::from_utf8(output.stderr.bytes)?;
        assert!(error.contains(&format!("fail #1668 {HEAD}")), "{error}");
        no_merge(&rig)?;
    }
    Ok(())
}

fn pr_merge_refuses_unpassed_missing_and_malformed_checks() -> Result {
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
        let rig = rig()?;
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

fn pr_merge_refuses_head_changes_before_and_after_checks() -> Result {
    for changed_before in [false, true] {
        let rig = rig()?;
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

fn pr_merge_reports_merged_in_text() -> Result {
    let rig = rig()?;
    let output = rig.command(&["pr", "merge", "1668", "--head", HEAD])?;
    assert_eq!(code(&output), 0, "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout.bytes)?,
        format!("merged #1668 {HEAD}\n")
    );
    Ok(())
}

fn pr_merge_guards_head_and_uses_optional_git_email() -> Result {
    for email in [None, Some("123+fixture@users.noreply.github.com")] {
        let rig = rig()?;
        if let Some(email) = email {
            let git = provenant_supervisor::git::git_executable().ok_or("Git executable")?;
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
        let mut expected = format!("pr\nmerge\n1668\n--merge\n--match-head-commit\n{HEAD}\n");
        if let Some(email) = email {
            expected.push_str(&format!("--author-email\n{email}\n"));
        }
        assert_eq!(
            fs::read_to_string(rig.project.join("merge-args"))?,
            expected
        );
        assert!(!rig.project.join("watched").exists());
    }
    Ok(())
}

fn pr_external_failures_and_invalid_arguments_fail_closed() -> Result {
    for flag in ["checks-fail", "merge-fail"] {
        let rig = rig()?;
        fs::write(rig.project.join(flag), "")?;
        let output = merge(&rig)?;
        assert_ne!(code(&output), 0, "{flag}");
        let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
        assert!(report["data"].get("merged").is_none());
        if flag == "checks-fail" {
            no_merge(&rig)?;
        }
    }
    let rig = rig()?;
    for args in [
        vec!["pr", "merge", "1668"],
        vec!["pr", "merge", "1668", "--head", "abc"],
        vec!["pr", "watch", "0"],
        vec!["pr", "watch", "--admin"],
    ] {
        assert_eq!(code(&rig.command(&args)?), 2, "{args:?}");
    }
    assert!(!rig.project.join("gh-calls").exists());
    Ok(())
}

fn fake_gh(args: &[String]) -> Result<u8> {
    let call = args.join(" ");
    writeln!(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("gh-calls")?,
        "{call}"
    )?;
    match call.as_str() {
        "pr view 1668 --json headRefOid" => {
            if Path::new("head-read").exists() && Path::new("move-head").exists() {
                write!(io::stdout(), "{{\"headRefOid\":\"{OTHER}\"}}")?;
            } else {
                io::stdout().write_all(&fs::read("head.json")?)?;
            }
            fs::write("head-read", "")?;
        }
        "pr checks 1668 --watch" => {
            fs::write("watched", "")?;
            if Path::new("watch-fail").exists() {
                return Ok(1);
            }
        }
        "pr checks 1668 --json bucket" => {
            if Path::new("checks-fail").exists() {
                return Ok(8);
            }
            io::stdout().write_all(&fs::read("checks.json")?)?;
        }
        value
            if value.starts_with(&format!("pr merge 1668 --merge --match-head-commit {HEAD}")) =>
        {
            fs::write("merge-args", args.join("\n") + "\n")?;
            if Path::new("merge-fail").exists() {
                return Ok(1);
            }
        }
        _ => return Err(format!("unexpected gh arguments: {call}").into()),
    }
    Ok(0)
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let result = if args.first().is_some_and(|arg| arg == "pr") {
        fake_gh(&args)
    } else {
        type Test = (&'static str, fn() -> Result);
        let tests: &[Test] = &[
            (
                "pr_watch_reports_the_verified_head_and_waits_via_gh",
                pr_watch_reports_the_verified_head_and_waits_via_gh,
            ),
            (
                "pr_watch_failure_and_head_changes_report_fail_with_head",
                pr_watch_failure_and_head_changes_report_fail_with_head,
            ),
            (
                "pr_merge_refuses_unpassed_missing_and_malformed_checks",
                pr_merge_refuses_unpassed_missing_and_malformed_checks,
            ),
            (
                "pr_merge_refuses_head_changes_before_and_after_checks",
                pr_merge_refuses_head_changes_before_and_after_checks,
            ),
            (
                "pr_merge_reports_merged_in_text",
                pr_merge_reports_merged_in_text,
            ),
            (
                "pr_merge_guards_head_and_uses_optional_git_email",
                pr_merge_guards_head_and_uses_optional_git_email,
            ),
            (
                "pr_external_failures_and_invalid_arguments_fail_closed",
                pr_external_failures_and_invalid_arguments_fail_closed,
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
