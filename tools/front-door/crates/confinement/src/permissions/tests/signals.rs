//! Native comparison of intra-lane signalling and the host-process boundary.
#![cfg(target_os = "macos")]

use super::*;

#[test]
fn same_sandbox_signals_reach_parents_but_cannot_reach_host_processes()
-> Result<(), Box<dyn std::error::Error>> {
    let executor = SystemExecutor::new(2)?;
    let cancel = CancelToken::new();
    let env = [("LC_ALL".into(), "C".into())];
    let (availability, diagnostic) = probe_sandbox_with_diagnostic(&BackendContext {
        executor: &executor,
        cancel: &cancel,
        env: &env,
    })?;
    if !matches!(availability, Availability::Available { .. }) {
        assert_ne!(
            std::env::var("PROVENANT_CONFINEMENT_REQUIRED").as_deref(),
            Ok("1"),
            "native sandbox unavailable: {availability:?}; {diagnostic:?}"
        );
        return Ok(());
    }
    let temp = crate::test_support::tempdir()?;
    let scratch = provenant_os::fs::canonicalize(temp.path())?;
    let (mut plan, _) = write_grants_fixture();
    plan.read_only = None;
    plan.private_state = None;
    plan.scratch.path = scratch;
    plan.write_roots.clear();
    let git = worktree_grants_fixture();
    let profile = ProviderProfile {
        spec: ProviderProfileSpec {
            allow_process_fork: true,
            tool_exec_roots: vec![PathGrant {
                path: "/bin".into(),
                scope: PathScope::Subtree,
            }],
            ..ProviderProfileSpec::default()
        },
        observations: BTreeMap::new(),
    };
    // This test process is alive and outside every sandbox it launches.
    // SIGCONT is harmless if a regression accidentally permits the signal.
    let host_pid = std::process::id().to_string();
    let run = |source: &str, script: &str, argument: &str| {
        let argv = [
            SANDBOX_EXEC,
            "-p",
            source,
            "/bin/sh",
            "-c",
            script,
            "signal-probe",
            argument,
        ]
        .map(OsString::from);
        let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::from_secs(3));
        options.env = Some(&env);
        options.cancel = Some(&cancel);
        options.stdout = StreamPolicy::Capture { limit: 1024 };
        options.stderr = StreamPolicy::Capture { limit: 1024 };
        executor.execute(options)
    };
    let child_to_parent = r#"
set -eu
notified=no
trap 'notified=yes' USR1
/bin/sh -c 'kill -USR1 "$1"' child "$$"
test "$notified" = yes
"#;
    for writer in [false, true] {
        if writer {
            plan.mode = AccessMode::WorktreeWrite(Box::new(WorktreeWriteAuthority {
                git_grants_digest: [7; 32],
                publishing: false,
                common_git_dir: Some("/fixture/git".into()),
                cargo_cache: PathGrant {
                    path: "/fixture/cache".into(),
                    scope: PathScope::Subtree,
                },
            }));
        } else {
            plan.mode = AccessMode::ReadOnly;
        }
        let source = String::from_utf8(render_profile(
            &plan,
            &profile,
            &[],
            &[],
            writer.then_some(&git),
            Path::new("/fixture/home"),
        )?)?;
        let previous = source.replacen(
            "(allow signal (target same-sandbox))",
            if writer {
                "(allow signal (target children))"
            } else {
                ""
            },
            1,
        );
        assert_ne!(previous, source);
        let denied_parent = run(&previous, child_to_parent, "")?;
        assert!(
            denied_probe(&denied_parent),
            "writer={writer}: old policy must deny child-to-parent signals: {:?}; {}",
            denied_parent.termination,
            String::from_utf8_lossy(&denied_parent.stderr.bytes)
        );
        let allowed_parent = run(&source, child_to_parent, "")?;
        assert!(
            successful_probe(&allowed_parent),
            "writer={writer}: same-sandbox parent signal: {:?}; {}",
            allowed_parent.termination,
            String::from_utf8_lossy(&allowed_parent.stderr.bytes)
        );
        let denied_host = run(&source, "kill -CONT \"$1\"", &host_pid)?;
        assert!(
            denied_probe(&denied_host),
            "writer={writer}: signal to outside test process must be denied: {:?}; {}",
            denied_host.termination,
            String::from_utf8_lossy(&denied_host.stderr.bytes)
        );
    }
    Ok(())
}
