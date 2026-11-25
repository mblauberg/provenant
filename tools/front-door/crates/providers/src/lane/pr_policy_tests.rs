use super::{GhShim, gh_wrapper, quote};
use std::ffi::OsString;

#[test]
fn restrictive_gh_wrappers_parse_in_system_shell() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_config::PrPolicy;
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    use std::time::Duration;

    let temp = tempfile::tempdir()?;
    let executor = SystemExecutor::new(1)?;
    let policy = provenant_config::GithubPolicy::default();
    for pr_policy in [PrPolicy::Never, PrPolicy::Draft] {
        let script = gh_wrapper(
            "/usr/bin/gh",
            None,
            "'/tmp/gh.count'",
            &GhShim {
                policy: &policy,
                budget: 0,
                budget_source: "default",
                label_ci_warning: false,
                pr_policy,
                pr_source: "default",
            },
        );
        let argv = ["/bin/sh".into(), "-n".into(), "-c".into(), script.into()];
        let output =
            executor.execute(ExecOptions::new(&argv, temp.path(), Duration::from_secs(5)))?;
        assert_eq!(
            output.termination,
            Termination::Exit(0),
            "{pr_policy:?}: {}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
    }
    Ok(())
}

#[test]
fn the_gh_wrapper_enforces_the_lane_pull_request_policy() -> Result<(), Box<dyn std::error::Error>>
{
    use provenant_config::PrPolicy;
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
    let temp = tempfile::tempdir()?;
    let real_gh = temp.path().join("real-gh");
    fs::write(&real_gh, "#!/bin/sh\nprintf '<%s>\\n' \"$@\"\n")?;
    fs::set_permissions(&real_gh, fs::Permissions::from_mode(0o700))?;
    let policy = provenant_config::GithubPolicy::default();
    let executor = SystemExecutor::new(1)?;
    let run = |name: &str,
               value: PrPolicy,
               source: &str,
               args: &[&str]|
     -> Result<_, Box<dyn std::error::Error>> {
        let shim = temp.path().join(name);
        let counter = temp.path().join(format!("{name}.count"));
        fs::write(
            &shim,
            gh_wrapper(
                real_gh.to_str().unwrap(),
                None,
                &quote(counter.to_str().unwrap()),
                &GhShim {
                    policy: &policy,
                    budget: 0,
                    budget_source: "default",
                    label_ci_warning: false,
                    pr_policy: value,
                    pr_source: source,
                },
            ),
        )?;
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o700))?;
        let argv: Vec<_> = std::iter::once(shim.as_os_str().to_owned())
            .chain(args.iter().map(OsString::from))
            .collect();
        Ok(executor.execute(ExecOptions::new(&argv, temp.path(), Duration::from_secs(5)))?)
    };

    // never: every PR mutation and forge write is refused, reads proceed.
    for args in [
        vec!["pr", "create", "--title", "x"],
        vec!["pr", "edit", "42", "--title", "x"],
        vec!["pr", "close", "42"],
        vec![
            "api",
            "-X",
            "POST",
            "repos/o/r/pulls/1/comments",
            "-f",
            "body=x",
        ],
    ] {
        let output = run("never", PrPolicy::Never, "host", &args)?;
        assert_ne!(output.termination, Termination::Exit(0), "{args:?}");
        assert!(output.stdout.bytes.is_empty(), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr.bytes);
        assert!(stderr.contains("lanes.pr=never"), "{stderr}");
        assert!(stderr.contains("source: host"), "{stderr}");
    }
    let read = run("never", PrPolicy::Never, "host", &["pr", "view", "42"])?;
    assert_eq!(read.termination, Termination::Exit(0));
    assert_eq!(read.stdout.bytes, b"<pr>\n<view>\n<42>\n");
    let api_read = run(
        "never",
        PrPolicy::Never,
        "host",
        &["api", "repos/o/r/pulls/1"],
    )?;
    assert_eq!(api_read.termination, Termination::Exit(0));

    // Explicit read methods override the write implied by fields, in either order.
    for policy in [PrPolicy::Never, PrPolicy::Draft] {
        for (args, allowed) in [
            (
                vec!["api", "-X", "GET", "repos/o/r/pulls", "-f", "state=open"],
                true,
            ),
            (
                vec!["api", "repos/o/r/pulls", "-f", "state=open", "--method=GET"],
                true,
            ),
            (
                vec!["api", "-F", "page=1", "-XHEAD", "repos/o/r/pulls"],
                true,
            ),
            (vec!["api", "repos/o/r/pulls", "-f", "state=open"], false),
            (
                vec!["api", "-X", "POST", "repos/o/r/pulls", "-f", "state=open"],
                false,
            ),
        ] {
            let output = run("api-method", policy, "host", &args)?;
            assert_eq!(
                output.termination == Termination::Exit(0),
                allowed,
                "{policy:?} {args:?}"
            );
            assert_eq!(
                !output.stdout.bytes.is_empty(),
                allowed,
                "{policy:?} {args:?}"
            );
        }
    }

    // Selectors at command boundaries must not change write classification.
    for policy in [PrPolicy::Never, PrPolicy::Draft] {
        for (args, allowed) in [
            (
                vec!["-R", "o/r", "api", "-X", "POST", "repos/o/r/issues"],
                false,
            ),
            (
                vec!["--repo=o/r", "api", "repos/o/r/issues", "-f", "title=x"],
                false,
            ),
            (
                vec![
                    "--hostname",
                    "example.com",
                    "--repo",
                    "o/r",
                    "api",
                    "--input",
                    "body.json",
                ],
                false,
            ),
            (vec!["-Ro/r", "pr", "--repo=o/r", "ready", "42"], false),
            (
                vec!["pr", "--hostname=example.com", "-R", "o/r", "merge", "42"],
                false,
            ),
            (vec!["--repo", "o/r", "api", "repos/o/r/issues"], true),
            (
                vec![
                    "--hostname=example.com",
                    "api",
                    "-f",
                    "q=x",
                    "-X",
                    "GET",
                    "search/issues",
                ],
                true,
            ),
            (vec!["-Ro/r", "pr", "--repo", "o/r", "view", "42"], true),
            (vec!["api", "--template", "-f", "repos/o/r/issues"], true),
        ] {
            let output = run("normalised", policy, "host", &args)?;
            assert_eq!(
                output.termination == Termination::Exit(0),
                allowed,
                "{policy:?} {args:?}"
            );
            if allowed {
                let expected = args
                    .iter()
                    .map(|arg| format!("<{arg}>\n"))
                    .collect::<String>();
                assert_eq!(
                    output.stdout.bytes,
                    expected.as_bytes(),
                    "{policy:?} {args:?}"
                );
            } else {
                assert!(output.stdout.bytes.is_empty(), "{policy:?} {args:?}");
            }
        }
    }

    // All non-maintenance PR writes are refused by both restrictive policies.
    for policy in [PrPolicy::Never, PrPolicy::Draft] {
        for command in [
            "lock",
            "unlock",
            "update-branch",
            "revert",
            "ready",
            "reopen",
            "merge",
            "future-write",
        ] {
            let args = ["pr", command, "42"];
            let output = run("restricted", policy, "host", &args)?;
            assert_ne!(
                output.termination,
                Termination::Exit(0),
                "{policy:?} {args:?}"
            );
            assert!(output.stdout.bytes.is_empty(), "{policy:?} {args:?}");
        }
    }
    for policy in [PrPolicy::Never, PrPolicy::Draft] {
        let output = run(
            "repo-flag",
            policy,
            "host",
            &["pr", "-R", "o/r", "lock", "42"],
        )?;
        assert_ne!(output.termination, Termination::Exit(0));
        assert!(output.stdout.bytes.is_empty());
    }
    for command in ["checkout", "checks", "diff", "list", "status", "view"] {
        for policy in [PrPolicy::Never, PrPolicy::Draft] {
            let output = run("read", policy, "host", &["pr", command])?;
            assert_eq!(
                output.termination,
                Termination::Exit(0),
                "{policy:?} {command}"
            );
        }
    }
    for command in ["edit", "close", "comment", "review"] {
        let output = run(
            "maintenance",
            PrPolicy::Draft,
            "host",
            &["pr", command, "42"],
        )?;
        assert_eq!(output.termination, Termination::Exit(0), "{command}");
    }

    // Repository flags before the command cannot bypass either policy.
    for prefix in [
        vec!["--repo", "o/r"],
        vec!["--repo=o/r"],
        vec!["-R", "o/r"],
        vec!["-Ro/r"],
    ] {
        let mut args = prefix;
        args.extend(["pr", "create", "--title", "x"]);
        let refused = run("prefix-never", PrPolicy::Never, "host", &args)?;
        assert_ne!(refused.termination, Termination::Exit(0), "{args:?}");
        assert!(refused.stdout.bytes.is_empty(), "{args:?}");
        let draft = run("prefix-draft", PrPolicy::Draft, "host", &args)?;
        assert_eq!(draft.termination, Termination::Exit(0), "{args:?}");
        let mut expected = args
            .iter()
            .map(|arg| format!("<{arg}>\n"))
            .collect::<String>();
        expected.push_str("<--draft>\n");
        assert_eq!(draft.stdout.bytes, expected.as_bytes(), "{args:?}");
    }

    // draft: creation gains --draft, promotion and forge writes are refused.
    let draft = run(
        "draft",
        PrPolicy::Draft,
        "launch",
        &["pr", "create", "--title", "x"],
    )?;
    assert_eq!(draft.termination, Termination::Exit(0));
    assert_eq!(
        draft.stdout.bytes,
        b"<pr>\n<create>\n<--title>\n<x>\n<--draft>\n"
    );
    let already = run(
        "draft",
        PrPolicy::Draft,
        "launch",
        &["pr", "create", "--draft"],
    )?;
    assert_eq!(already.termination, Termination::Exit(0));
    assert_eq!(already.stdout.bytes, b"<pr>\n<create>\n<--draft>\n");
    let promote = run("draft", PrPolicy::Draft, "launch", &["pr", "ready", "42"])?;
    assert_ne!(promote.termination, Termination::Exit(0));

    // allowed: PR writes and forge writes forward unchanged.
    let allowed = run(
        "allowed",
        PrPolicy::Allowed,
        "default",
        &["pr", "create", "--title", "x"],
    )?;
    assert_eq!(allowed.termination, Termination::Exit(0));
    assert_eq!(allowed.stdout.bytes, b"<pr>\n<create>\n<--title>\n<x>\n");
    let api_write = run(
        "allowed",
        PrPolicy::Allowed,
        "default",
        &["api", "-X", "POST", "repos/o/r/issues"],
    )?;
    assert_eq!(api_write.termination, Termination::Exit(0));
    Ok(())
}
