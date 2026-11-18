//! Native process metadata stays ambient while process arguments stay private.
#![cfg(target_os = "macos")]

use super::*;

pub(super) const PROCESS_SYSCTL_DENIAL: &str =
    "(deny sysctl-read (sysctl-name-prefix \"kern.procargs\"))\n";

const PROBE: &str = r#"
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/sysctl.h>
#include <unistd.h>

static int query(const char *label, int *mib, unsigned count, int denied) {
    unsigned char bytes[8192] = {0};
    size_t length = sizeof(bytes);
    errno = 0;
    int result = sysctl(mib, count, bytes, &length, NULL, 0);
    int error = errno;
    // Never print bytes: process arguments can include private host data.
    if (denied ? (result != -1 || (error != EPERM && error != ESRCH)) :
        (result != 0 || length == 0)) {
        fprintf(stderr, "%s: result=%d errno=%d bytes=%zu expected=%s\n",
            label, result, error, length, denied ? "denied" : "available");
        return 1;
    }
    return 0;
}

int main(int argc, char **argv) {
    if (argc == 3 && strcmp(argv[1], "peer") == 0) {
        FILE *ready = fopen(argv[2], "w");
        if (!ready) return 90;
        fprintf(ready, "%d\n", getpid());
        if (fclose(ready)) return 91;
        for (;;) pause();
    }
    if (argc != 4 || strcmp(argv[1], "probe") != 0) return 92;
    pid_t host = (pid_t)strtol(argv[2], NULL, 10);
    pid_t peer = (pid_t)strtol(argv[3], NULL, 10);
    pid_t self = getpid();
    pid_t parent = getppid();
    int self_pid[] = {CTL_KERN, KERN_PROC, KERN_PROC_PID, self};
    int host_pid_info[] = {CTL_KERN, KERN_PROC, KERN_PROC_PID, host};
    int self_args[] = {CTL_KERN, KERN_PROCARGS2, self};
    int parent_args[] = {CTL_KERN, KERN_PROCARGS2, parent};
    int host_args[] = {CTL_KERN, KERN_PROCARGS2, host};
    int peer_args[] = {CTL_KERN, KERN_PROCARGS2, peer};
    int cpus[] = {CTL_HW, HW_NCPU};
    int failed = 0;
    failed |= query("self process metadata", self_pid, 4, 0);
    failed |= query("host process metadata", host_pid_info, 4, 0);
    failed |= query("own arguments", self_args, 3, 0);
    failed |= query("sandbox launcher arguments", parent_args, 3, 1);
    failed |= query("host arguments", host_args, 3, 1);
    failed |= query("other sandbox arguments", peer_args, 3, 1);
    failed |= query("hardware metadata", cpus, 2, 0);
    return failed;
}
"#;

#[test]
// Spec: docs/specs/supervisor.md#network-ipc-and-process-information
fn process_arguments_are_private_across_sandbox_boundaries()
-> Result<(), Box<dyn std::error::Error>> {
    let executor = SystemExecutor::new(3)?;
    let cancel = CancelToken::new();
    let env = [("LC_ALL".into(), "C".into())];
    let (availability, diagnostic) = probe_sandbox_with_diagnostic(&BackendContext {
        executor: &executor,
        cancel: &cancel,
        env: &env,
    })?;
    if !matches!(availability, Availability::Available { .. }) {
        provenant_test_support::skip_sandbox_dependent_test(format!(
            "native sandbox unavailable: {availability:?}; {diagnostic:?}"
        ))?;
        return Ok(());
    }

    let temp = crate::test_support::tempdir()?;
    let root = provenant_os::fs::canonicalize(temp.path())?;
    let input = root.join("sysctl-process.c");
    let helper = root.join("sysctl-process");
    std::fs::write(&input, PROBE)?;
    let compile = [
        OsString::from("/usr/bin/cc"),
        input.as_os_str().to_owned(),
        "-o".into(),
        helper.as_os_str().to_owned(),
    ];
    let compiled = executor.execute(ExecOptions::new(&compile, &root, Duration::from_secs(30)))?;
    assert!(
        successful_probe(&compiled),
        "compile sysctl process probe: {}",
        String::from_utf8_lossy(&compiled.stderr.bytes)
    );

    let (mut plan, _) = write_grants_fixture();
    plan.read_only = None;
    plan.private_state = None;
    plan.scratch.path = root.clone();
    plan.write_roots.clear();
    let git = worktree_grants_fixture();
    let profile = ProviderProfile {
        spec: ProviderProfileSpec {
            process_executables: vec![PathGrant {
                path: helper.clone(),
                scope: PathScope::Exact,
            }],
            ..ProviderProfileSpec::default()
        },
        observations: BTreeMap::new(),
    };
    let mut failures = Vec::new();
    for writer in [false, true] {
        plan.mode = if writer {
            AccessMode::WorktreeWrite(Box::new(WorktreeWriteAuthority {
                git_grants_digest: [7; 32],
                publishing: false,
                common_git_dir: "/fixture/git".into(),
                cargo_cache: PathGrant {
                    path: root.join("cache"),
                    scope: PathScope::Subtree,
                },
            }))
        } else {
            AccessMode::ReadOnly
        };
        let source = String::from_utf8(render_profile(
            &plan,
            &profile,
            &[PathGrant {
                path: root.clone(),
                scope: PathScope::Subtree,
            }],
            &[],
            writer.then_some(&git),
            Path::new("/fixture/home"),
        )?)?;
        assert!(source.contains(PROCESS_SYSCTL_DENIAL));
        assert!(source.contains("(deny process-info-pidinfo)"));
        assert!(source.contains("(allow process-info-pidinfo (target same-sandbox))"));

        let ready = root.join(format!("peer-{writer}.pid"));
        let peer_cancel = CancelToken::new();
        let peer_argv = [
            OsString::from(SANDBOX_EXEC),
            "-p".into(),
            source.clone().into(),
            helper.as_os_str().to_owned(),
            "peer".into(),
            ready.as_os_str().to_owned(),
        ];
        std::thread::scope(|scope| -> Result<(), Box<dyn std::error::Error>> {
            let peer_worker = scope.spawn(|| {
                let mut options = ExecOptions::new(&peer_argv, &root, Duration::from_secs(20));
                options.env = Some(&env);
                options.cancel = Some(&peer_cancel);
                options.process_group = true;
                options.cleanup_process_group = true;
                executor.execute(options)
            });
            let peer_pid = provenant_test_support::wait_for(
                "sysctl peer PID",
                Duration::from_secs(5),
                || std::fs::read_to_string(&ready).ok(),
                Option::is_some,
            )?
            .ok_or("missing sysctl peer PID")?;
            let host_pid = std::process::id().to_string();
            let probe_argv = [
                OsString::from(SANDBOX_EXEC),
                "-p".into(),
                source.clone().into(),
                helper.as_os_str().to_owned(),
                "probe".into(),
                host_pid.into(),
                peer_pid.trim().into(),
            ];
            let mut options = ExecOptions::new(&probe_argv, Path::new("/"), Duration::from_secs(5));
            options.env = Some(&env);
            options.cancel = Some(&cancel);
            options.process_group = true;
            options.stdout = StreamPolicy::Capture { limit: 1024 };
            options.stderr = StreamPolicy::Capture { limit: 4096 };
            let result = executor.execute(options)?;
            peer_cancel.cancel();
            let _ = peer_worker.join().expect("sysctl peer executor");
            if !successful_probe(&result) {
                failures.push(format!(
                    "writer={writer}: sysctl boundary: {:?}; {}; {}",
                    result.termination,
                    String::from_utf8_lossy(&result.stdout.bytes),
                    String::from_utf8_lossy(&result.stderr.bytes)
                ));
            }
            Ok(())
        })?;
    }
    assert!(
        failures.is_empty(),
        "sysctl argument boundary failed:\n{}",
        failures.join("\n")
    );
    Ok(())
}
