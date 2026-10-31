//! Host sampling and confined reading under the unmodified production profiles.
#![cfg(target_os = "macos")]
// Fixture readiness and cancellation are explicit native real-time boundaries.
#![allow(clippy::disallowed_methods)]

use super::*;
use provenant_core::AttemptId;
use provenant_supervisor::{
    census::{ProcessObservation, SystemProcessObserver},
    process_tree,
};

const ROLE: &str = "PROVENANT_TREE_FIXTURE_ROLE";
const ROOT: &str = "PROVENANT_TREE_FIXTURE_ROOT";
const HOST: &str = "PROVENANT_TREE_FIXTURE_HOST";
const PEER: &str = "PROVENANT_TREE_FIXTURE_PEER";
const HELPER: &str = "PROVENANT_TREE_FIXTURE_HELPER";
const READER: &str = "permissions::tests::process_tree::process_tree_reader_fixture";

const TREE_HELPER: &str = r#"
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>
static volatile sig_atomic_t stopped;
static void stop(int signal) { (void)signal; stopped = 1; }
int main(int argc, char **argv) {
    if (argc != 2) return 90;
    pid_t live = fork();
    if (live < 0) return 91;
    if (live == 0) { for (;;) pause(); }
    pid_t zombie = fork();
    if (zombie < 0) return 92;
    if (zombie == 0) _exit(0);
    siginfo_t exited;
    if (waitid(P_PID, zombie, &exited, WEXITED | WNOWAIT)) return 93;
    signal(SIGTERM, stop); signal(SIGINT, stop);
    FILE *ready = fopen(argv[1], "w");
    if (!ready) return 94;
    fprintf(ready, "%d %d %d\n", getpid(), live, zombie);
    if (fclose(ready)) return 95;
    while (!stopped) usleep(10000);
    kill(live, SIGTERM);
    while (waitpid(live, NULL, 0) < 0 && errno == EINTR) {}
    while (waitpid(zombie, NULL, 0) < 0 && errno == EINTR) {}
    return 0;
}
"#;

struct Stop<'a>(&'a CancelToken);
impl Drop for Stop<'_> {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn ready(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.is_empty()
        {
            return Ok(text);
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("fixture did not become ready: {}", path.display()).into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn process_tree_reader_fixture() -> Result<(), Box<dyn std::error::Error>> {
    let Some(role) = std::env::var_os(ROLE) else {
        return Ok(());
    };
    let root = PathBuf::from(std::env::var_os(ROOT).ok_or("missing fixture root")?);
    if role == "peer" {
        std::fs::write(root.join("peer.pid"), std::process::id().to_string())?;
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        return Err("peer was not cancelled after the reader finished".into());
    }
    assert_eq!(role, "reader");
    let host = std::env::var(HOST)?.parse::<u32>()?;
    let peer = std::env::var(PEER)?.parse::<u32>()?;
    let helper = std::env::var_os(HELPER).ok_or("tree helper")?;
    let executor = SystemExecutor::new(1)?;
    let cancel = CancelToken::new();
    std::thread::scope(|scope| -> Result<(), Box<dyn std::error::Error>> {
        let worker = scope.spawn(|| {
            let argv = [helper.clone(), root.join("children").into_os_string()];
            let mut options = ExecOptions::new(&argv, &root, Duration::from_secs(20));
            options.cancel = Some(&cancel);
            options.process_group = true;
            executor.execute(options)
        });
        let _stop = Stop(&cancel);
        let children = ready(&root.join("children"))?
            .split_whitespace()
            .map(str::parse::<u32>)
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(children.len(), 3);
        let own = std::process::id();
        std::fs::write(root.join("lane.pid"), own.to_string())?;
        let path = PathBuf::from(std::env::var_os(process_tree::ENV).ok_or("snapshot path")?);
        let _ = ready(&path)?;
        let sample = process_tree::read(&path, own)?;
        assert_eq!(sample.schema_version, 1);
        assert_eq!(sample.root_pid, own);
        let clock = provenant_os::clock::SystemClock::new();
        assert!(sample.sampled_at <= provenant_core::Clock::wall_now(&clock)?);
        for (pid, parent) in [
            (own, None),
            (children[0], Some(own)),
            (children[1], Some(children[0])),
        ] {
            let process = sample
                .processes
                .iter()
                .find(|process| process.pid == pid)
                .ok_or("sample omitted a ready tree member")?;
            assert!(process.rss_bytes > 0, "PID {pid} RSS must be nonzero");
            assert_eq!(process.pgid, if pid == own { own } else { children[0] });
            if let Some(parent) = parent {
                assert_eq!(process.ppid, parent);
            }
        }
        assert!(
            !sample
                .processes
                .iter()
                .any(|process| process.pid == host || process.pid == peer)
        );
        // Darwin can refuse proc_pidinfo and sandbox membership for an
        // unreaped zombie. It has no resident task, so omission is safe;
        // kernels that expose it must report zero RSS.
        let zombie = sample
            .processes
            .iter()
            .find(|process| process.pid == children[2]);
        if let Some(zombie) = zombie {
            assert_eq!(zombie.ppid, children[0]);
            assert_eq!(zombie.rss_bytes, 0);
        }
        assert!(
            process_tree::read(&path, children[2]).is_err(),
            "zombie root is unavailable"
        );
        let subtree = process_tree::read(&path, children[0])?;
        assert_eq!(subtree.root_pid, children[0]);
        assert_eq!(
            subtree
                .processes
                .iter()
                .map(|process| process.pid)
                .collect::<std::collections::BTreeSet<_>>(),
            [
                Some(children[0]),
                Some(children[1]),
                zombie.map(|process| process.pid)
            ]
            .into_iter()
            .flatten()
            .collect::<std::collections::BTreeSet<_>>()
        );
        assert!(
            process_tree::read(&path, host).is_err(),
            "outside host must not be selectable"
        );
        assert!(
            process_tree::read(&path, peer).is_err(),
            "identical-profile peer must not be selectable"
        );
        assert!(
            std::fs::write(&path, b"{}").is_err(),
            "snapshot must remain read-only"
        );
        let attempt = path.parent().ok_or("snapshot parent")?;
        for forbidden in [
            attempt.join("reservation.json"),
            attempt
                .parent()
                .ok_or("attempt parent")?
                .join("other-attempt")
                .join(process_tree::FILE),
        ] {
            let error = std::fs::read(&forbidden)
                .expect_err("exact snapshot grant must not expose siblings or other attempts");
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied,
                "{}: {error}",
                forbidden.display()
            );
        }
        cancel.cancel();
        let _ = worker.join().expect("tree child executor");
        Ok(())
    })
}

#[test]
fn lane_process_tree_sample_is_private_under_production_profiles()
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
        assert_ne!(
            std::env::var("PROVENANT_CONFINEMENT_REQUIRED").as_deref(),
            Ok("1"),
            "native sandbox unavailable: {availability:?}; {diagnostic:?}"
        );
        return Ok(());
    }
    let executable = provenant_os::fs::canonicalize(&std::env::current_exe()?)?;
    for writer in [false, true] {
        let temp = crate::test_support::tempdir()?;
        let base = provenant_os::fs::canonicalize(temp.path())?;
        let attempt_path = base.join("attempt");
        std::fs::create_dir(&attempt_path)?;
        let directory = provenant_os::fs::Directory::open_anchor(&attempt_path)?;
        let snapshot_path = attempt_path.join(process_tree::FILE);
        std::fs::write(
            attempt_path.join("reservation.json"),
            b"private reservation",
        )?;
        let other = base.join("other-attempt");
        std::fs::create_dir(&other)?;
        std::fs::write(other.join(process_tree::FILE), b"foreign snapshot")?;
        let root = base.join("scratch");
        std::fs::create_dir(&root)?;
        let input = root.join("tree-helper.c");
        let helper = root.join("tree-helper");
        std::fs::write(&input, TREE_HELPER)?;
        let compile = [
            OsString::from("/usr/bin/cc"),
            input.into_os_string(),
            "-o".into(),
            helper.as_os_str().to_owned(),
        ];
        let compiled =
            executor.execute(ExecOptions::new(&compile, &root, Duration::from_secs(30)))?;
        assert!(
            successful_probe(&compiled),
            "tree helper: {}",
            String::from_utf8_lossy(&compiled.stderr.bytes)
        );
        let (mut plan, _) = write_grants_fixture();
        plan.read_only = None;
        plan.private_state = None;
        plan.scratch.path = root.clone();
        plan.write_roots.clear();
        let git = worktree_grants_fixture();
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
        let profile = ProviderProfile {
            spec: ProviderProfileSpec {
                process_executables: vec![
                    PathGrant {
                        path: executable.clone(),
                        scope: PathScope::Exact,
                    },
                    PathGrant {
                        path: helper.clone(),
                        scope: PathScope::Exact,
                    },
                ],
                allow_process_fork: true,
                tool_exec_roots: vec![PathGrant {
                    path: "/bin".into(),
                    scope: PathScope::Subtree,
                }],
                ..ProviderProfileSpec::default()
            },
            observations: BTreeMap::new(),
        };
        let attempt = AttemptId::from_bytes([u8::from(writer) + 50; 16]);
        let source = String::from_utf8(render_profile(
            &plan,
            &profile,
            &[
                PathGrant {
                    path: snapshot_path.clone(),
                    scope: PathScope::Exact,
                },
                PathGrant {
                    path: executable.clone(),
                    scope: PathScope::Exact,
                },
            ],
            &[],
            writer.then_some(&git),
            Path::new("/fixture/home"),
        )?)?;
        let attempt_rules = source
            .lines()
            .filter(|line| line.contains(attempt_path.to_str().expect("attempt path")))
            .collect::<Vec<_>>();
        assert_eq!(
            attempt_rules,
            [format!(
                "(allow file-read* (literal \"{}\"))",
                snapshot_path.display()
            )]
        );
        // ConfinedLaunch::command appends the same non-serving attempt marker.
        // Both independent launches deliberately receive byte-identical profiles.
        let source = format!(
            "{source}(allow mach-lookup (global-name \"{}\"))\n",
            attempt.sandbox_lookup_name()
        );
        let argv = [
            OsString::from(SANDBOX_EXEC),
            "-p".into(),
            source.into(),
            executable.as_os_str().to_owned(),
            "--exact".into(),
            READER.into(),
            "--nocapture".into(),
        ];
        let peer_stop = CancelToken::new();
        let reader_stop = CancelToken::new();
        std::thread::scope(|scope| -> Result<(), Box<dyn std::error::Error>> {
            let peer = scope.spawn(|| {
                let env = [
                    ("LC_ALL".into(), "C".into()),
                    (ROLE.into(), "peer".into()),
                    (ROOT.into(), root.as_os_str().to_owned()),
                ];
                let mut options = ExecOptions::new(&argv, &root, Duration::from_secs(20));
                options.env = Some(&env);
                options.cancel = Some(&peer_stop);
                options.process_group = true;
                executor.execute(options)
            });
            let _stop_peer = Stop(&peer_stop);
            let peer_pid = ready(&root.join("peer.pid"))?.parse::<u32>()?;
            let reader_argv = &argv;
            let reader_root = &root;
            let reader_executor = &executor;
            let reader_cancel = &reader_stop;
            let reader = scope.spawn(move || {
                let env = [
                    ("LC_ALL".into(), "C".into()),
                    (ROLE.into(), "reader".into()),
                    (ROOT.into(), reader_root.as_os_str().to_owned()),
                    (HOST.into(), std::process::id().to_string().into()),
                    (PEER.into(), peer_pid.to_string().into()),
                    (HELPER.into(), helper.as_os_str().to_owned()),
                    (
                        process_tree::ENV.into(),
                        snapshot_path.as_os_str().to_owned(),
                    ),
                ];
                let mut options =
                    ExecOptions::new(reader_argv, reader_root, Duration::from_secs(15));
                options.env = Some(&env);
                options.cancel = Some(reader_cancel);
                options.process_group = true;
                options.stdout = StreamPolicy::Capture { limit: 8192 };
                options.stderr = StreamPolicy::Capture { limit: 8192 };
                reader_executor.execute(options)
            });
            let _stop_reader = Stop(&reader_stop);
            let lane_pid = ready(&root.join("lane.pid"))?.parse::<i64>()?;
            let identity = match SystemProcessObserver.observe(lane_pid) {
                ProcessObservation::Present(fact) => fact.identity,
                observation => {
                    return Err(format!("ready lane identity unavailable: {observation:?}").into());
                }
            };
            let snapshot = process_tree::sample(&identity, attempt)?;
            process_tree::publish(&directory, Some(&snapshot))?;
            let result = reader.join().expect("confined reader executor")?;
            assert!(
                successful_probe(&result),
                "writer={writer}: confined process-tree reader: {:?}; {}; {}",
                result.termination,
                String::from_utf8_lossy(&result.stdout.bytes),
                String::from_utf8_lossy(&result.stderr.bytes),
            );
            peer_stop.cancel();
            let _ = peer.join().expect("peer executor");
            Ok(())
        })?;
    }
    Ok(())
}
