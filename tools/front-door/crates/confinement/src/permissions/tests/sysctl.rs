//! Native process-list and argument sysctls, without exposing their contents.
#![cfg(target_os = "macos")]

use super::*;

pub(super) const PROCESS_SYSCTL_DENIAL: &str = "(deny sysctl-read (sysctl-name-prefix \"kern.procargs\") (sysctl-name-prefix \"kern.proc.\"))\n";

const PROBE: &str = r#"
#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/sysctl.h>
#include <unistd.h>

static int query(const char *label, int *mib, unsigned count,
                 size_t capacity, int denied) {
    void *bytes = calloc(1, capacity);
    if (!bytes) return 1;
    size_t length = capacity;
    errno = 0;
    int result = sysctl(mib, count, bytes, &length, NULL, 0);
    int error = errno;
    // Never print bytes: process arguments can include private host data.
    free(bytes);
    if (denied ? (result != -1 || error != EPERM) :
        (result != 0 || length == 0)) {
        fprintf(stderr, "%s: result=%d errno=%d bytes=%zu expected=%s\n",
            label, result, error, length, denied ? "EPERM" : "nonempty success");
        return 1;
    }
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 3) return 90;
    int private = strcmp(argv[1], "private") == 0;
    pid_t host = (pid_t)strtol(argv[2], NULL, 10);
    int self_pid[] = {CTL_KERN, KERN_PROC, KERN_PROC_PID, getpid()};
    int self_args[] = {CTL_KERN, KERN_PROCARGS2, getpid()};
    int host_pid[] = {CTL_KERN, KERN_PROC, KERN_PROC_PID, host};
    int host_args[] = {CTL_KERN, KERN_PROCARGS2, host};
    int list[] = {CTL_KERN, KERN_PROC, KERN_PROC_ALL, 0};
    int cpus[] = {CTL_HW, HW_NCPU};
    int failed = 0;
    failed |= query("self KERN_PROC_PID", self_pid, 4, 8192, 0);
    failed |= query("self KERN_PROCARGS2", self_args, 3, ARG_MAX, 0);
    failed |= query("host KERN_PROC_PID", host_pid, 4, 8192, private);
    failed |= query("host KERN_PROCARGS2", host_args, 3, ARG_MAX, private);
    failed |= query("host KERN_PROC_ALL", list, 4, 8 * 1024 * 1024, private);
    failed |= query("HW_NCPU", cpus, 2, sizeof(int), 0);
    return failed;
}
"#;

#[test]
fn sysctl_blocks_host_process_queries_but_preserves_self() -> Result<(), Box<dyn std::error::Error>>
{
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
    let host_pid = std::process::id().to_string();
    let mut failures = Vec::new();
    for writer in [false, true] {
        plan.mode = if writer {
            AccessMode::WorktreeWrite(Box::new(WorktreeWriteAuthority {
                git_grants_digest: [7; 32],
                publishing: false,
                common_git_dir: Some("/fixture/git".into()),
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
        for (variant, source) in [
            ("baseline", source.replace(PROCESS_SYSCTL_DENIAL, "")),
            ("private", source),
        ] {
            let argv = [
                OsString::from(SANDBOX_EXEC),
                "-p".into(),
                source.into(),
                helper.as_os_str().to_owned(),
                variant.into(),
                host_pid.clone().into(),
            ];
            let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::from_secs(5));
            options.env = Some(&env);
            options.cancel = Some(&cancel);
            options.process_group = true;
            options.stdout = StreamPolicy::Capture { limit: 1024 };
            options.stderr = StreamPolicy::Capture { limit: 4096 };
            let result = executor.execute(options)?;
            if !successful_probe(&result) {
                failures.push(format!(
                    "writer={writer} {variant}: {:?}; {}",
                    result.termination,
                    String::from_utf8_lossy(&result.stderr.bytes)
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "sysctl process boundary failed:\n{}",
        failures.join("\n")
    );
    Ok(())
}
