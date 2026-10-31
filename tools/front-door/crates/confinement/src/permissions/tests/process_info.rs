//! Native libproc causal probe and own-process ps/RSS visibility.
#![cfg(target_os = "macos")]

use super::*;

const PROBE: &str = r#"
#include <errno.h>
#include <dlfcn.h>
#include <libproc.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;
extern int sandbox_check(pid_t, const char *, int, ...);

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "exec-ps") == 0) {
        const int *no_report = dlsym(RTLD_DEFAULT, "SANDBOX_CHECK_NO_REPORT");
        int flags = no_report ? *no_report : 0;
        const char *operations[] = {"forbidden-exec-sugid", "not-a-real-operation-2452"};
        for (unsigned i = 0; i < 2; ++i) {
            errno = 0;
            int result = sandbox_check(getpid(), operations[i], flags);
            int error = errno;
            fprintf(stderr, "sandbox_check %s: result=%d errno=%d\n",
                operations[i], result, error);
        }
        char *args[] = {"/bin/ps", "-A", "-o", "pid=,ppid=,pgid=,rss=", NULL};
        execve(args[0], args, environ);
        int error = errno;
        fprintf(stderr, "execve /bin/ps failed: errno=%d\n", error);
        return 1;
    }
    if (argc != 3) return 90;
    int baseline = strcmp(argv[1], "baseline") == 0;
    int allowed = strcmp(argv[1], "denied") != 0;
    pid_t host = (pid_t)strtol(argv[2], NULL, 10);
    int ready[2];
    if (pipe(ready) != 0) return 91;
    pid_t child = fork();
    if (child < 0) return 92;
    if (child == 0) {
        close(ready[0]);
        volatile char pages[16384];
        for (unsigned i = 0; i < sizeof(pages); ++i) pages[i] = 1;
        char byte = pages[0];
        if (write(ready[1], &byte, 1) != 1) _exit(93);
        for (;;) pause();
    }
    close(ready[1]);
    char byte;
    if (read(ready[0], &byte, 1) != 1) return 94;
    close(ready[0]);
    int failed = 0;
    struct proc_taskallinfo own;
    memset(&own, 0, sizeof(own));
    errno = 0;
    int count = proc_pidinfo(child, PROC_PIDTASKALLINFO, 0, &own, sizeof(own));
    int query_errno = errno;
    if (allowed) {
        if (count != sizeof(own) || own.ptinfo.pti_resident_size == 0 ||
            own.pbsd.pbi_pid != (unsigned)child ||
            own.pbsd.pbi_ppid != (unsigned)getpid() ||
            own.pbsd.pbi_pgid != (unsigned)getpgrp()) {
            fprintf(stderr, "own proc_pidinfo(PROC_PIDTASKALLINFO): count=%d errno=%d rss=%llu\n",
                count, query_errno, own.ptinfo.pti_resident_size);
            failed = 1;
        }
    } else if (count != 0 || query_errno != EPERM) {
        fprintf(stderr, "control proc_pidinfo(PROC_PIDTASKALLINFO): count=%d errno=%d\n",
            count, query_errno);
        failed = 1;
    } else {
        puts("proc_pidinfo(PROC_PIDTASKALLINFO) denied with EPERM");
    }
    struct proc_taskinfo outside;
    errno = 0;
    count = proc_pidinfo(host, PROC_PIDTASKINFO, 0, &outside, sizeof(outside));
    query_errno = errno;
    if (baseline ? (count != sizeof(outside) || outside.pti_resident_size == 0) :
        (count != 0 || (query_errno != EPERM && query_errno != ESRCH))) {
        fprintf(stderr, "outside proc_pidinfo(PROC_PIDTASKINFO): count=%d errno=%d\n",
            count, query_errno);
        failed = 1;
    }
    kill(child, SIGTERM);
    while (waitpid(child, NULL, 0) < 0 && errno == EINTR) {}
    return failed;
}
"#;

#[test]
fn process_info_is_same_sandbox_and_ps_reports_own_rss() -> Result<(), Box<dyn std::error::Error>> {
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
    let input = root.join("proc-info.c");
    let helper = root.join("proc-info");
    std::fs::write(&input, PROBE)?;
    let compile = [
        OsString::from("/usr/bin/cc"),
        input.as_os_str().to_owned(),
        "-lsandbox".into(),
        "-o".into(),
        helper.as_os_str().to_owned(),
    ];
    let compiled = executor.execute(ExecOptions::new(&compile, &root, Duration::from_secs(30)))?;
    assert!(
        successful_probe(&compiled),
        "compile libproc probe: {}",
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
            allow_process_fork: true,
            tool_exec_roots: vec![PathGrant {
                path: "/bin".into(),
                scope: PathScope::Subtree,
            }],
            ..ProviderProfileSpec::default()
        },
        observations: BTreeMap::new(),
    };
    let host_pid = std::process::id().to_string();
    let mut failures = Vec::new();
    let mut passed = Vec::new();
    let run = |source: &str, args: &[OsString]| {
        let mut argv = [SANDBOX_EXEC, "-p", source].map(OsString::from).to_vec();
        argv.extend_from_slice(args);
        let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::from_secs(8));
        options.env = Some(&env);
        options.cancel = Some(&cancel);
        options.process_group = true;
        options.stdout = StreamPolicy::Capture { limit: 256 * 1024 };
        options.stderr = StreamPolicy::Capture { limit: 4096 };
        executor.execute(options)
    };
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
        // Single-operation A/B identifies process-info-pidinfo, rather than
        // attributing any ps exec or Mach task-port error to this syscall.
        for (rule, expected) in [
            ("", "baseline"),
            ("(deny process-info*)", "denied"),
            (
                "(deny process-info*)\n(allow process-info-pidinfo (target same-sandbox))",
                "allowed",
            ),
            (
                "(deny process-info*)\n(allow process-info* (target same-sandbox))",
                "allowed",
            ),
        ] {
            let variant = source.replace(
                "(deny process-info*)\n(allow process-info* (target same-sandbox))",
                rule,
            );
            let args = [
                helper.as_os_str().to_owned(),
                expected.into(),
                host_pid.clone().into(),
            ];
            let result = run(&variant, &args)?;
            if !successful_probe(&result) {
                failures.push(format!(
                    "writer={writer}: {rule:?} libproc: {:?}; {}; {}",
                    result.termination,
                    String::from_utf8_lossy(&result.stdout.bytes),
                    String::from_utf8_lossy(&result.stderr.bytes)
                ));
            } else {
                passed.push(format!(
                    "writer={writer}: {rule:?} libproc {expected}: PASS"
                ));
            }
        }
        let script = r#"
set -eu
/bin/sleep 5 &
child=$!
trap 'kill -TERM "$child" 2>/dev/null || :; wait "$child" || :' EXIT
printf 'own=%s child=%s\n' "$$" "$child"
/bin/ps -A -o pid=,ppid=,pgid=,rss=
"#;
        let result = run(&source, &["/bin/sh".into(), "-c".into(), script.into()])?;
        if !successful_probe(&result) {
            let diagnostic = run(&source, &[helper.as_os_str().to_owned(), "exec-ps".into()])?;
            failures.push(format!(
                "writer={writer}: stock ps: {:?}; {}; {}",
                result.termination,
                String::from_utf8_lossy(&result.stderr.bytes),
                String::from_utf8_lossy(&diagnostic.stderr.bytes)
            ));
            continue;
        }
        let text = std::str::from_utf8(&result.stdout.bytes)?;
        let mut lines = text.lines();
        let header = lines.next().ok_or("missing own process IDs")?;
        let own = header
            .split_whitespace()
            .map(|field| {
                field
                    .split_once('=')
                    .ok_or("invalid PID header")
                    .and_then(|(_, value)| value.parse::<u32>().map_err(|_| "invalid PID"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(own.len(), 2);
        let rows = lines
            .filter_map(|line| {
                let fields = line
                    .split_whitespace()
                    .map(str::parse::<u64>)
                    .collect::<Result<Vec<_>, _>>()
                    .ok()?;
                (fields.len() == 4).then_some(fields)
            })
            .collect::<Vec<_>>();
        for pid in own {
            if !rows
                .iter()
                .any(|row| row[0] == u64::from(pid) && row[3] > 0)
            {
                failures.push(format!(
                    "writer={writer}: ps missing own nonzero RSS for {pid}"
                ));
            }
        }
        if !rows
            .iter()
            .filter(|row| row[0] == u64::from(std::process::id()))
            .all(|row| row[3] == 0)
        {
            failures.push("ps exposed outside host RSS".into());
        }
    }
    assert!(
        failures.is_empty(),
        "process-info causal matrix passed:\n{}\nfailed:\n{}",
        passed.join("\n"),
        failures.join("\n")
    );
    Ok(())
}
