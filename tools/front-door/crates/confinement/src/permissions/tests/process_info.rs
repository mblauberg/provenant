//! Native libproc causal probe and own-sandbox RSS visibility.
#![cfg(target_os = "macos")]

use super::*;

const PROBE: &str = r#"
#include <errno.h>
#include <libproc.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

int main(int argc, char **argv) {
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
fn process_info_includes_host_and_reports_own_rss() -> Result<(), Box<dyn std::error::Error>> {
    let executor = SystemExecutor::new(2)?;
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
    let input = root.join("proc-info.c");
    let helper = root.join("proc-info");
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
        // attributing a different syscall denial to this libproc operation.
        for (rule, expected) in [
            ("(allow process-info*)", "baseline"),
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
            let variant = source.replace("(allow process-info*)", rule);
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
    }
    assert!(
        failures.is_empty(),
        "process-info causal matrix passed:\n{}\nfailed:\n{}",
        passed.join("\n"),
        failures.join("\n")
    );
    Ok(())
}

const HOST_MEMORY_PROBE: &str = r#"
#include <errno.h>
#include <fcntl.h>
#include <mach/mach.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <servers/bootstrap.h>
#include <sys/ptrace.h>
#include <sys/clonefile.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>
#include <util.h>

static const char *keychain_services[] = {
    "com.apple.SecurityServer", "com.apple.securityd", "com.apple.securityd.xpc"
};

int main(int argc, char **argv) {
    if (argc == 5 && strcmp(argv[1], "clone") == 0) {
        errno = 0;
        int cloned = clonefile(argv[2], argv[3], 0);
        int clone_errno = errno;
        int allowed = strcmp(argv[4], "allow") == 0;
        if (allowed && cloned == 0) return 0;
        if (!allowed && cloned < 0 && (clone_errno == EPERM || clone_errno == EACCES)) return 0;
        if (allowed && (clone_errno == ENOTSUP || clone_errno == EOPNOTSUPP || clone_errno == EXDEV)) return 77;
        fprintf(stderr, "clonefile expected_allow=%d result=%d errno=%d\n", allowed, cloned, clone_errno);
        return 79;
    }
    if (argc == 3 && strcmp(argv[1], "pty-probe") == 0) {
        int master, slave;
        char own_name[128];
        if (openpty(&master, &slave, own_name, NULL, NULL)) {
            perror("ambient openpty"); return 80;
        }
        int own = open(own_name, O_RDWR | O_NOCTTY | O_NONBLOCK);
        if (own < 0) { perror("own tty reopen"); return 81; }
        close(own); close(master); close(slave);
        errno = 0;
        int host = open(argv[2], O_RDWR | O_NOCTTY | O_NONBLOCK);
        int open_errno = errno;
        if (host >= 0) { close(host); fputs("host tty opened\n", stderr); return 82; }
        if (open_errno != EPERM && open_errno != EACCES) {
            fprintf(stderr, "host tty refusal errno=%d\n", open_errno); return 83;
        }
        return 0;
    }
    if (argc == 3 && strcmp(argv[1], "pty-launch") == 0) {
        int master, slave;
        char host_name[128];
        if (openpty(&master, &slave, host_name, NULL, NULL)) return 84;
        int baseline = open(host_name, O_RDWR | O_NOCTTY | O_NONBLOCK);
        if (baseline < 0) { perror("host tty baseline"); return 85; }
        close(baseline);
        fcntl(master, F_SETFD, FD_CLOEXEC);
        fcntl(slave, F_SETFD, FD_CLOEXEC);
        pid_t probe = fork();
        if (probe == 0) {
            execl("/usr/bin/sandbox-exec", "sandbox-exec", "-p", argv[2],
                argv[0], "pty-probe", host_name, NULL);
            _exit(86);
        }
        int status = 0;
        if (probe < 0) status = 87 << 8;
        else while (waitpid(probe, &status, 0) < 0 && errno == EINTR) {}
        close(master); close(slave);
        return WIFEXITED(status) ? WEXITSTATUS(status) : 88;
    }
    if (argc == 5 && strcmp(argv[1], "probe") == 0) {
        pid_t host = (pid_t)strtol(argv[2], NULL, 10);
        int baseline = strcmp(argv[3], "baseline") == 0;
        int failed = 0;
        mach_port_t task = MACH_PORT_NULL;
        kern_return_t control = task_for_pid(mach_task_self(), host, &task);
        printf("task_for_pid=%d\n", control);
        if (control == KERN_SUCCESS) {
            mach_port_deallocate(mach_task_self(), task);
            if (!baseline) failed = 1;
        }
        task = MACH_PORT_NULL;
        errno = 0;
        int read_result = (int)syscall(SYS_task_read_for_pid, mach_task_self(), host, &task);
        printf("task_read_for_pid=%d errno=%d\n", read_result, errno);
        if (read_result == 0) {
            mach_port_deallocate(mach_task_self(), task);
            if (!baseline) failed = 1;
        }
        errno = 0;
        int attached = ptrace(PT_ATTACHEXC, host, NULL, 0);
        printf("ptrace=%d errno=%d\n", attached, errno);
        if (attached == 0) {
            // This target is disposable. Its unconfined parent always kills
            // and reaps it, including if a broken policy allowed the attach.
            ptrace(PT_DETACH, host, (caddr_t)1, 0);
            kill(host, SIGCONT);
            if (!baseline) failed = 1;
        }
        unsigned available = (unsigned)strtoul(argv[4], NULL, 10);
        for (unsigned i = 0; i < 3; ++i) {
            mach_port_t service = MACH_PORT_NULL;
            kern_return_t found = bootstrap_look_up(bootstrap_port, keychain_services[i], &service);
            printf("keychain %s baseline_available=%d result=%d\n",
                keychain_services[i], (available & (1u << i)) != 0, found);
            if (found == KERN_SUCCESS) {
                mach_port_deallocate(mach_task_self(), service);
                if (!baseline) failed = 1;
            }
        }
        if (available == 0) puts("SKIP keychain IPC attribution: no baseline registered service");
        return failed;
    }
    if (argc != 3 || strcmp(argv[1], "launch") != 0) return 90;
    int ready[2];
    if (pipe(ready)) return 91;
    pid_t host = fork();
    if (host < 0) return 92;
    if (host == 0) {
        close(ready[0]);
        char byte = 'r';
        if (write(ready[1], &byte, 1) != 1) _exit(93);
        close(ready[1]);
        for (;;) pause();
    }
    close(ready[1]);
    char byte;
    if (read(ready[0], &byte, 1) != 1) {
        kill(host, SIGKILL);
        waitpid(host, NULL, 0);
        return 94;
    }
    close(ready[0]);
    unsigned available = 0;
    for (unsigned i = 0; i < 3; ++i) {
        mach_port_t service = MACH_PORT_NULL;
        if (bootstrap_look_up(bootstrap_port, keychain_services[i], &service) == KERN_SUCCESS) {
            available |= 1u << i;
            mach_port_deallocate(mach_task_self(), service);
        }
    }
    char pid[32], service_mask[32];
    snprintf(pid, sizeof(pid), "%d", host);
    snprintf(service_mask, sizeof(service_mask), "%u", available);
    pid_t probe = fork();
    if (probe == 0) {
        if (strcmp(argv[2], "baseline") == 0)
            execl(argv[0], argv[0], "probe", pid, "baseline", service_mask, NULL);
        else
            execl("/usr/bin/sandbox-exec", "sandbox-exec", "-p", argv[2],
                argv[0], "probe", pid, "denied", service_mask, NULL);
        _exit(95);
    }
    int status = 0;
    if (probe < 0) status = 96 << 8;
    else while (waitpid(probe, &status, 0) < 0 && errno == EINTR) {}
    kill(host, SIGKILL);
    while (waitpid(host, NULL, 0) < 0 && errno == EINTR) {}
    return WIFEXITED(status) ? WEXITSTATUS(status) : 97;
}
"#;

// Large/native: real Mach/ptrace calls against a disposable same-UID host peer.
#[test]
fn ambient_profiles_deny_host_task_memory_ptrace_and_keychain_ipc()
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
        provenant_test_support::skip_sandbox_dependent_test(format!(
            "native host memory boundary requires Seatbelt: {availability:?}; {diagnostic:?}"
        ))?;
        return Ok(());
    }
    let temp = crate::test_support::tempdir()?;
    let root = provenant_os::fs::canonicalize(temp.path())?;
    let helper = compile_native_boundary_helper(&executor, &root)?;
    let run = |policy: &str| {
        let argv = [
            helper.as_os_str().to_owned(),
            "launch".into(),
            policy.into(),
        ];
        let mut options = ExecOptions::new(&argv, &root, Duration::from_secs(8));
        options.env = Some(&env);
        options.cancel = Some(&cancel);
        options.process_group = true;
        options.stdout = StreamPolicy::Capture { limit: 4096 };
        options.stderr = StreamPolicy::Capture { limit: 4096 };
        executor.execute(options)
    };
    // The platform may already refuse unsigned task access. Record that
    // baseline without bypassing SIP or requiring any entitlement change.
    let baseline = run("baseline")?;
    assert!(
        successful_probe(&baseline),
        "host memory baseline: {baseline:?}"
    );
    if String::from_utf8_lossy(&baseline.stdout.bytes).contains("SKIP keychain IPC attribution") {
        provenant_test_support::skip_sandbox_dependent_test(
            "native keychain IPC attribution unavailable: no registered baseline service",
        )?;
    }
    let (mut plan, _) = write_grants_fixture();
    plan.read_only = None;
    plan.private_state = None;
    plan.scratch.path = root.clone();
    plan.write_roots.clear();
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
    let git = worktree_grants_fixture();
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
            &[],
            &[],
            writer.then_some(&git),
            Path::new("/fixture/home"),
        )?)?;
        let result = run(&source)?;
        assert!(
            successful_probe(&result),
            "writer={writer}; host memory boundary: {result:?}; baseline: {baseline:?}"
        );
    }
    Ok(())
}

pub(super) fn compile_native_boundary_helper(
    executor: &SystemExecutor,
    root: &Path,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let input = root.join("host-memory.c");
    let helper = root.join("host-memory");
    std::fs::write(&input, HOST_MEMORY_PROBE)?;
    let compile = [
        OsString::from("/usr/bin/cc"),
        input.as_os_str().to_owned(),
        "-o".into(),
        helper.as_os_str().to_owned(),
    ];
    let compiled = executor.execute(ExecOptions::new(&compile, root, Duration::from_secs(30)))?;
    assert!(
        successful_probe(&compiled),
        "compile host memory helper: {compiled:?}"
    );
    Ok(helper)
}

// Large/native: both terminals are synthetic and owned by this fixture.
#[test]
fn ambient_pty_creation_keeps_host_terminals_private() -> Result<(), Box<dyn std::error::Error>> {
    let executor = SystemExecutor::new(2)?;
    let cancel = CancelToken::new();
    let env = [("LC_ALL".into(), "C".into())];
    let (availability, diagnostic) = probe_sandbox_with_diagnostic(&BackendContext {
        executor: &executor,
        cancel: &cancel,
        env: &env,
    })?;
    if !matches!(availability, Availability::Available { .. }) {
        provenant_test_support::skip_sandbox_dependent_test(format!(
            "native PTY boundary requires Seatbelt: {availability:?}; {diagnostic:?}"
        ))?;
        return Ok(());
    }
    let temp = crate::test_support::tempdir()?;
    let root = provenant_os::fs::canonicalize(temp.path())?;
    let helper = compile_native_boundary_helper(&executor, &root)?;
    let (mut plan, _) = write_grants_fixture();
    plan.read_only = None;
    plan.private_state = None;
    plan.scratch.path = root.clone();
    plan.write_roots.clear();
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
    let git = worktree_grants_fixture();
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
            &[],
            &[],
            writer.then_some(&git),
            Path::new("/fixture/home"),
        )?)?;
        let argv = [
            helper.as_os_str().to_owned(),
            "pty-launch".into(),
            source.into(),
        ];
        let mut options = ExecOptions::new(&argv, &root, Duration::from_secs(8));
        options.env = Some(&env);
        options.cancel = Some(&cancel);
        options.process_group = true;
        options.stdout = StreamPolicy::Capture { limit: 4096 };
        options.stderr = StreamPolicy::Capture { limit: 4096 };
        let result = executor.execute(options)?;
        assert!(
            successful_probe(&result),
            "writer={writer}; native PTY boundary: {result:?}"
        );
    }
    Ok(())
}
