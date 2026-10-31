//! PostgreSQL and foreign-object checks for explicit database IPC authority.
#![cfg(target_os = "macos")]

use super::*;

fn native_available(executor: &SystemExecutor) -> Result<bool, Box<dyn std::error::Error>> {
    let cancel = CancelToken::new();
    let (availability, diagnostic) = probe_sandbox_with_diagnostic(&BackendContext {
        executor,
        cancel: &cancel,
        env: &[],
    })?;
    if matches!(availability, Availability::Available { .. }) {
        return Ok(true);
    }
    assert_ne!(
        std::env::var("PROVENANT_CONFINEMENT_REQUIRED").as_deref(),
        Ok("1"),
        "native sandbox unavailable: {availability:?}; {diagnostic:?}"
    );
    Ok(false)
}

fn postgres_bin() -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    let bin = std::env::var_os("PROVENANT_TEST_POSTGRES_BIN")
        .map(PathBuf::from)
        .or_else(|| {
            [
                "/opt/homebrew/opt/postgresql@18/bin",
                "/usr/local/opt/postgresql@18/bin",
            ]
            .into_iter()
            .map(PathBuf::from)
            .find(|bin| bin.join("initdb").is_file())
        });
    if bin.is_none() {
        assert_ne!(
            std::env::var("PROVENANT_POSTGRES_REQUIRED").as_deref(),
            Ok("1"),
            "PostgreSQL 18 unavailable; set PROVENANT_TEST_POSTGRES_BIN"
        );
    }
    bin.map(|path| provenant_os::fs::canonicalize(&path).map_err(Into::into))
        .transpose()
}

fn database_profile(
    root: &Path,
    bin: &Path,
    writer: bool,
    database: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let scratch = root.join("scratch");
    std::fs::create_dir_all(&scratch)?;
    let (mut plan, _) = write_grants_fixture();
    plan.read_only = None;
    plan.private_state = None;
    plan.scratch.path = scratch.clone();
    plan.write_roots.clear();
    plan.network = NetworkAccess::Local;
    plan.mode = if writer {
        AccessMode::WorktreeWrite(Box::new(WorktreeWriteAuthority {
            git_grants_digest: [7; 32],
            publishing: false,
            common_git_dir: Some(root.join("git")),
            cargo_cache: PathGrant {
                path: scratch.join("cache"),
                scope: PathScope::Subtree,
            },
        }))
    } else {
        AccessMode::ReadOnly
    };
    let git = WorktreeWriteGrants::from_paths(
        &root.join("worktree"),
        &root.join("git/private"),
        &root.join("git"),
        "fixture",
    );
    let profile = ProviderProfile {
        spec: ProviderProfileSpec {
            allow_process_fork: true,
            allow_database_ipc: database,
            tool_exec_roots: [
                PathBuf::from("/bin"),
                PathBuf::from("/usr/bin"),
                bin.to_owned(),
            ]
            .map(|path| PathGrant {
                path,
                scope: PathScope::Subtree,
            })
            .to_vec(),
            ..ProviderProfileSpec::default()
        },
        observations: BTreeMap::new(),
    };
    let reads = ["/opt/homebrew", "/usr/local"]
        .map(|path| PathGrant {
            path: path.into(),
            scope: PathScope::Subtree,
        })
        .into_iter()
        .chain(std::iter::once(PathGrant {
            path: bin.parent().ok_or("PostgreSQL prefix")?.to_owned(),
            scope: PathScope::Subtree,
        }))
        .collect::<Vec<_>>();
    Ok(String::from_utf8(render_profile(
        &plan,
        &profile,
        &reads,
        &[],
        writer.then_some(&git),
        &root.join("home"),
    )?)?)
}

fn postgres_case(database: bool) -> Result<(), Box<dyn std::error::Error>> {
    let executor = SystemExecutor::new(2)?;
    if !native_available(&executor)? {
        return Ok(());
    }
    let Some(bin) = postgres_bin()? else {
        return Ok(());
    };
    for writer in [false, true] {
        let temp = crate::test_support::tempdir()?;
        let root = provenant_os::fs::canonicalize(temp.path())?;
        let source = database_profile(&root, &bin, writer, database)?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port().to_string();
        drop(listener);
        let initdb = r#"
set -eu
"$PGBIN/initdb" -D "$TMPDIR/data" --no-locale --encoding=UTF8 -A trust -N -n -c dynamic_shared_memory_type=mmap
"#;
        let lifecycle = r#"
set -eu
"$PGBIN/initdb" -D "$TMPDIR/data" --no-locale --encoding=UTF8 -A trust -N -n -c dynamic_shared_memory_type=mmap > "$TMPDIR/initdb.log" 2>&1 || { cat "$TMPDIR/initdb.log" >&2; exit 1; }
"$PGBIN/postgres" -D "$TMPDIR/data" -h 127.0.0.1 -k '' -p "$PGPORT" -c shared_buffers=8MB -c max_connections=10 -c dynamic_shared_memory_type=mmap > "$TMPDIR/postgres.log" 2>&1 &
pg=$!
trap 'kill -TERM "$pg"; wait "$pg"' EXIT
trap 'exit 1' HUP INT TERM
i=0
until "$PGBIN/psql" -X -h 127.0.0.1 -p "$PGPORT" -d postgres -tAc 'SELECT 42' > "$TMPDIR/query" 2>/dev/null; do
    i=$((i + 1))
    if [ "$i" -ge 50 ]; then cat "$TMPDIR/postgres.log" >&2; exit 1; fi
    sleep 0.1
done
test "$(cat "$TMPDIR/query")" = 42
test "$("$PGBIN/psql" -X -h 127.0.0.1 -p "$PGPORT" -d postgres -tAc 'SHOW dynamic_shared_memory_type')" = mmap
if touch "$OUTSIDE" 2> "$TMPDIR/denial"; then exit 1; fi
grep -q 'Operation not permitted' "$TMPDIR/denial"
"#;
        let outside = root.join("outside-write-roots");
        let env = [
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("LC_ALL".into(), "C".into()),
            ("TMPDIR".into(), root.join("scratch").into_os_string()),
            ("PGBIN".into(), bin.clone().into_os_string()),
            ("PGPORT".into(), port.into()),
            ("PGCONNECT_TIMEOUT".into(), "1".into()),
            (
                "PGOPTIONS".into(),
                "-c default_transaction_read_only=on -c statement_timeout=2000".into(),
            ),
            ("OUTSIDE".into(), outside.clone().into_os_string()),
        ];
        let script = if database { lifecycle } else { initdb };
        let argv = [SANDBOX_EXEC, "-p", &source, "/bin/sh", "-c", script].map(OsString::from);
        let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::from_secs(30));
        options.env = Some(&env);
        options.terminate_grace = Duration::from_secs(5);
        options.stdout = StreamPolicy::Capture { limit: 8192 };
        options.stderr = StreamPolicy::Capture { limit: 8192 };
        let outcome = executor.execute(options)?;
        let log = std::fs::read_to_string(root.join("scratch/postgres.log")).unwrap_or_default();
        if database {
            assert!(
                successful_probe(&outcome),
                "writer={writer}: database-on: {:?}; {}; {}; {log}",
                outcome.termination,
                String::from_utf8_lossy(&outcome.stdout.bytes),
                String::from_utf8_lossy(&outcome.stderr.bytes)
            );
        } else {
            assert!(
                denied_probe(&outcome)
                    && String::from_utf8_lossy(&outcome.stderr.bytes).contains("shared memory"),
                "writer={writer}: initdb must fail on SysV IPC without capability: {:?}; {}; {}",
                outcome.termination,
                String::from_utf8_lossy(&outcome.stdout.bytes),
                String::from_utf8_lossy(&outcome.stderr.bytes)
            );
        }
        assert!(!outside.exists());
    }
    Ok(())
}

#[test]
fn postgres_without_database_ipc_refuses_initdb() -> Result<(), Box<dyn std::error::Error>> {
    postgres_case(false)
}

#[test]
fn postgres_database_ipc_allows_mmap_lifecycle_in_both_modes()
-> Result<(), Box<dyn std::error::Error>> {
    postgres_case(true)
}

/// Cleanup is owned by the unconfined test supervisor, not the timed sandbox.
struct SysvObjects<'a> {
    executor: &'a SystemExecutor,
    ids: Option<[String; 2]>,
}
impl SysvObjects<'_> {
    fn cleanup(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let Some(ids) = &self.ids else {
            return Ok(());
        };
        let script = r#"
my @errors;
push @errors, "shm cleanup: $!\n"
    if !shmctl($ARGV[0], IPC_RMID, 0) && !$!{EINVAL} && !$!{EIDRM};
push @errors, "sem cleanup: $!\n"
    if !semctl($ARGV[1], 0, IPC_RMID, 0) && !$!{EINVAL} && !$!{EIDRM};
die join("", @errors) if @errors;
"#;
        let argv = [
            "/usr/bin/perl",
            "-MIPC::SysV=IPC_RMID",
            "-MErrno=EINVAL,EIDRM",
            "-e",
            script,
            &ids[0],
            &ids[1],
        ]
        .map(OsString::from);
        let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::from_secs(3));
        options.process_group = true;
        options.stderr = StreamPolicy::Capture { limit: 1024 };
        let result = self.executor.execute(options)?;
        if !successful_probe(&result) {
            return Err(format!(
                "supervisor IPC cleanup: {}",
                String::from_utf8_lossy(&result.stderr.bytes)
            )
            .into());
        }
        self.ids = None;
        Ok(())
    }
}
impl Drop for SysvObjects<'_> {
    fn drop(&mut self) {
        let _retry_on_failure = self.cleanup();
    }
}

#[test]
fn database_off_cannot_mutate_or_remove_foreign_sysv_objects()
-> Result<(), Box<dyn std::error::Error>> {
    let executor = SystemExecutor::new(2)?;
    if !native_available(&executor)? {
        return Ok(());
    }
    // Setup performs only nonblocking local IPC calls. Its END guard owns a
    // partially allocated pair; once it returns, the Rust supervisor owns both.
    let allocate = r#"
my ($shm, $sem, $transferred);
END {
    if (!$transferred) {
        shmctl($shm, IPC_RMID, 0) if defined $shm;
        semctl($sem, 0, IPC_RMID, 0) if defined $sem;
    }
}
$shm = shmget(IPC_PRIVATE, 56, IPC_CREAT | 0600);
die "host shmget: $!\n" unless defined $shm;
shmwrite($shm, "safe", 0, 4) or die "host shmwrite: $!\n";
$sem = semget(IPC_PRIVATE, 1, IPC_CREAT | 0600);
die "host semget: $!\n" unless defined $sem;
semctl($sem, 0, SETVAL, 1) or die "host SETVAL: $!\n";
printf "%d %d\n", $shm, $sem;
$transferred = 1;
"#;
    let probe = r#"
sub denied { die "foreign operation was not denied: $!\n" unless !$_[0] && $! == EPERM; }
denied(shmwrite($ARGV[0], "bad!", 0, 4));
denied(shmctl($ARGV[0], IPC_RMID, 0));
denied(semop($ARGV[1], pack("s!3", 0, 1, 0)));
denied(semctl($ARGV[1], 0, IPC_RMID, 0));
"#;
    let verify = r#"
shmread($ARGV[0], my $value, 0, 4) or die "host shmread: $!\n";
die "host shared memory changed\n" unless $value eq "safe";
die "host semaphore changed\n" unless semctl($ARGV[1], 0, GETVAL, 0) == 1;
"#;
    for writer in [false, true] {
        let temp = crate::test_support::tempdir()?;
        let root = provenant_os::fs::canonicalize(temp.path())?;
        let source = database_profile(&root, Path::new("/usr/bin"), writer, false)?;
        let argv = [
            "/usr/bin/perl",
            "-MIPC::SysV=IPC_PRIVATE,IPC_CREAT,IPC_RMID,SETVAL",
            "-e",
            allocate,
        ]
        .map(OsString::from);
        let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::from_secs(3));
        options.stdout = StreamPolicy::Capture { limit: 1024 };
        options.stderr = StreamPolicy::Capture { limit: 1024 };
        let allocated = executor.execute(options)?;
        assert!(
            successful_probe(&allocated),
            "host IPC setup: {}",
            String::from_utf8_lossy(&allocated.stderr.bytes)
        );
        let ids = std::str::from_utf8(&allocated.stdout.bytes)?
            .split_whitespace()
            .map(|value| value.parse::<u32>().map(|id| id.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        let mut objects = SysvObjects {
            executor: &executor,
            ids: Some(ids.try_into().map_err(|_| "expected two IPC object IDs")?),
        };
        let ids = objects.ids.as_ref().ok_or("IPC ownership")?;
        let argv = [
            SANDBOX_EXEC,
            "-p",
            &source,
            "/usr/bin/perl",
            "-MIPC::SysV=IPC_RMID",
            "-MErrno=EPERM",
            "-e",
            probe,
            &ids[0],
            &ids[1],
        ]
        .map(OsString::from);
        let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::from_secs(5));
        options.process_group = true;
        options.stderr = StreamPolicy::Capture { limit: 2048 };
        let outcome = executor.execute(options)?;
        let argv = [
            "/usr/bin/perl",
            "-MIPC::SysV=GETVAL",
            "-e",
            verify,
            &ids[0],
            &ids[1],
        ]
        .map(OsString::from);
        let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::from_secs(3));
        options.stderr = StreamPolicy::Capture { limit: 1024 };
        let unchanged = executor.execute(options)?;
        // Cleanup runs in the Rust-owned host context even if the sandbox
        // timed out; Drop retries it if any earlier operation returns/panics.
        objects.cleanup()?;
        assert!(
            successful_probe(&outcome) && successful_probe(&unchanged),
            "writer={writer}: foreign SysV: {:?}; {}; {}",
            outcome.termination,
            String::from_utf8_lossy(&outcome.stderr.bytes),
            String::from_utf8_lossy(&unchanged.stderr.bytes)
        );
    }
    Ok(())
}
