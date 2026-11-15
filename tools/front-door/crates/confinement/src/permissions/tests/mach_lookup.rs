//! Exercise the reserved custody marker against the actual rendered profile.
#![cfg(target_os = "macos")]
use super::*;

const LOOKUP_PROBE: &str = r#"
#include <sys/types.h>
#include <unistd.h>
// Private libSystem ABI, matching supervisor/process_os/census.rs.
extern int sandbox_check(pid_t, const char *, int, ...);
enum { SANDBOX_FILTER_GLOBAL_NAME = 2 };
int main(int argc, char **argv) {
    if (argc != 4) return 90;
    if (sandbox_check(getpid(), "mach-lookup", SANDBOX_FILTER_GLOBAL_NAME, argv[1]) != 0) return 91;
    if (sandbox_check(getpid(), "mach-lookup", SANDBOX_FILTER_GLOBAL_NAME, argv[2]) != 1) return 92;
    if (sandbox_check(getpid(), "mach-lookup", SANDBOX_FILTER_GLOBAL_NAME, "dev.provenant.attempt.control") != 1) return 93;
    // Unreserved services retain ambient access.
    if (sandbox_check(getpid(), "mach-lookup", SANDBOX_FILTER_GLOBAL_NAME, argv[3]) != 0) return 94;
    return 0;
}
"#;

#[test]
fn native_attempt_mach_lookups_preserve_custody_membership()
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
            "native sandbox unavailable: {availability:?}; {diagnostic:?}"
        ))?;
        return Ok(());
    }
    let temp = crate::test_support::tempdir()?;
    let root = provenant_os::fs::canonicalize(temp.path())?;
    let input = root.join("mach-lookup.c");
    let helper = root.join("mach-lookup");
    std::fs::write(&input, LOOKUP_PROBE)?;
    let compile = [
        OsString::from("/usr/bin/cc"),
        input.into_os_string(),
        "-o".into(),
        helper.clone().into_os_string(),
    ];
    let compiled = executor.execute(ExecOptions::new(&compile, &root, Duration::from_secs(30)))?;
    assert!(
        successful_probe(&compiled),
        "{}",
        String::from_utf8_lossy(&compiled.stderr.bytes)
    );
    let attempt = provenant_core::AttemptId::from_bytes([7; 16]);
    let foreign = provenant_core::AttemptId::from_bytes([8; 16]);
    let (mut plan, mut profile) = write_grants_fixture();
    plan.network = NetworkAccess::Open;
    profile.spec.process_executables.push(PathGrant {
        path: helper.clone(),
        scope: PathScope::Exact,
    });
    for writer in [false, true] {
        if writer {
            plan.mode = AccessMode::WorktreeWrite(Box::new(WorktreeWriteAuthority {
                git_grants_digest: [0; 32],
                publishing: false,
                common_git_dir: "/fixture/repo/.git".into(),
                cargo_cache: PathGrant {
                    path: "/fixture/cache".into(),
                    scope: PathScope::Subtree,
                },
            }));
        }
        let grants = GrantSet::build(
            &plan,
            &profile,
            Path::new("/fixture/home"),
            writer.then(worktree_grants_fixture),
        )?;
        let launch = ConfinedLaunch {
            enforcement: Enforcement::MacOsSandboxExec,
            profile: render(&grants)?.into_bytes(),
            profile_digest: [0; 32],
            native_controls: Vec::new(),
        };
        let command = launch.command(
            &[
                helper.clone().into_os_string(),
                attempt.sandbox_lookup_name().into(),
                foreign.sandbox_lookup_name().into(),
                "dev.synthetic.ambient-service".into(),
            ],
            &attempt,
            &provenant_os::fs::Directory::open_anchor(&root)?,
        )?;
        let result = executor.execute(command.options(&root, Duration::from_secs(3))?)?;
        assert!(successful_probe(&result), "writer={writer}: {result:?}");
    }
    Ok(())
}
