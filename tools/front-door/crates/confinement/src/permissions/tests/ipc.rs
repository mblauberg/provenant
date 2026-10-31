//! Optional native PostgreSQL smoke; the SysV probe also runs without PostgreSQL.
#![cfg(target_os = "macos")]

use super::*;

#[test]
fn postgres_initialises_serves_a_read_only_query_and_stops_in_writer_profile()
-> Result<(), Box<dyn std::error::Error>> {
    let executor = SystemExecutor::new(2)?;
    let cancel = CancelToken::new();
    let context = BackendContext {
        executor: &executor,
        cancel: &cancel,
        env: &[],
    };
    let (availability, diagnostic) = probe_sandbox_with_diagnostic(&context)?;
    if !matches!(availability, Availability::Available { .. }) {
        assert_ne!(
            std::env::var("PROVENANT_CONFINEMENT_REQUIRED").as_deref(),
            Ok("1"),
            "native sandbox unavailable: {availability:?}; {diagnostic:?}"
        );
        return Ok(());
    }
    // An override also supports non-Homebrew PostgreSQL 18 installations.
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
    let Some(bin) = bin else {
        assert_ne!(
            std::env::var("PROVENANT_POSTGRES_REQUIRED").as_deref(),
            Ok("1"),
            "PostgreSQL 18 unavailable; set PROVENANT_TEST_POSTGRES_BIN"
        );
        return Ok(());
    };
    let bin = provenant_os::fs::canonicalize(&bin)?;
    let temp = crate::test_support::tempdir()?;
    let root = provenant_os::fs::canonicalize(temp.path())?;
    let scratch = root.join("scratch");
    std::fs::create_dir(&scratch)?;
    let (mut plan, _) = write_grants_fixture();
    plan.read_only = None;
    plan.private_state = None;
    plan.scratch.path = scratch.clone();
    plan.write_roots.clear();
    plan.network = NetworkAccess::Local;
    plan.mode = AccessMode::WorktreeWrite(Box::new(WorktreeWriteAuthority {
        git_grants_digest: [7; 32],
        publishing: false,
        common_git_dir: Some(root.join("git")),
        cargo_cache: PathGrant {
            path: scratch.join("cache"),
            scope: PathScope::Subtree,
        },
    }));
    let git = WorktreeWriteGrants::from_paths(
        &root.join("worktree"),
        &root.join("git/private"),
        &root.join("git"),
        "fixture",
    );
    let profile = ProviderProfile {
        spec: ProviderProfileSpec {
            allow_process_fork: true,
            tool_exec_roots: [
                PathBuf::from("/bin"),
                PathBuf::from("/usr/bin"),
                bin.clone(),
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
    // Test-only runtime reads include Homebrew's linked libraries and PG data.
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
    let source = String::from_utf8(render_profile(
        &plan,
        &profile,
        &reads,
        &[],
        Some(&git),
        &root.join("home"),
    )?)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port().to_string();
    drop(listener);
    let script = r#"
set -eu
"$PGBIN/initdb" -D "$TMPDIR/data" --no-locale --encoding=UTF8 -A trust -N > "$TMPDIR/initdb.log" 2>&1 || { cat "$TMPDIR/initdb.log"; exit 1; }
printf 'initdb completed\n'
"$PGBIN/postgres" -D "$TMPDIR/data" -h 127.0.0.1 -k '' -p "$PGPORT" -c shared_buffers=8MB -c max_connections=10 -c dynamic_shared_memory_type=posix > "$TMPDIR/postgres.log" 2>&1 &
pg=$!
trap 'printf "stopping PostgreSQL\n"; kill -TERM "$pg"; wait "$pg"; printf "PostgreSQL stopped\n"' EXIT
i=0
until "$PGBIN/psql" -X -h 127.0.0.1 -p "$PGPORT" -d postgres -tAc 'SELECT 42' > "$TMPDIR/query" 2>/dev/null; do
    i=$((i + 1))
    if [ "$i" -ge 50 ]; then cat "$TMPDIR/postgres.log"; exit 1; fi
    sleep 0.1
done
test "$(cat "$TMPDIR/query")" = 42
printf 'read-only loopback query completed\n'
if touch "$OUTSIDE" 2> "$TMPDIR/denial"; then exit 1; fi
grep -q 'Operation not permitted' "$TMPDIR/denial"
printf 'outside-root write denied\n'
"#;
    let outside = root.join("outside-write-roots");
    let env = [
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("LC_ALL".into(), "C".into()),
        ("TMPDIR".into(), scratch.into_os_string()),
        ("PGBIN".into(), bin.into_os_string()),
        ("PGPORT".into(), port.into()),
        ("PGCONNECT_TIMEOUT".into(), "1".into()),
        (
            "PGOPTIONS".into(),
            "-c default_transaction_read_only=on".into(),
        ),
        ("OUTSIDE".into(), outside.clone().into_os_string()),
    ];
    let argv = [SANDBOX_EXEC, "-p", &source, "/bin/sh", "-c", script].map(OsString::from);
    let mut options = ExecOptions::new(&argv, Path::new("/"), Duration::from_secs(30));
    options.env = Some(&env);
    options.cancel = Some(&cancel);
    options.stdout = StreamPolicy::Capture { limit: 8192 };
    options.stderr = StreamPolicy::Capture { limit: 8192 };
    let outcome = executor.execute(options)?;
    let server_log = std::fs::read_to_string(root.join("scratch/postgres.log"))
        .unwrap_or_else(|error| format!("PostgreSQL log unavailable: {error}"));
    assert!(
        successful_probe(&outcome),
        "PostgreSQL smoke: {:?}; {}; {}; server log: {}",
        outcome.termination,
        String::from_utf8_lossy(&outcome.stdout.bytes),
        String::from_utf8_lossy(&outcome.stderr.bytes),
        server_log,
    );
    assert!(!outside.exists());
    Ok(())
}
