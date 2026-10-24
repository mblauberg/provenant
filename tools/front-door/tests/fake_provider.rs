//! One harness=false target: current_exe is the exact fake-provider executable.
mod support;
use std::{
    io::{self, Read, Write},
    process::ExitCode,
};
use support::{Result, TestRig, code};

fn scenario(name: &str) -> Result<u8> {
    match name {
        "exec" => {
            // Codex stub: both children must hold a connection before either
            // can finish. Only fixture auth files are read.
            use std::net::TcpStream;
            let mut address = String::new();
            io::stdin().read_to_string(&mut address)?;
            let home = std::path::PathBuf::from(
                std::env::var_os("CODEX_HOME").ok_or("missing CODEX_HOME")?,
            );
            let identity = std::fs::read(home.join("auth.json"))?;
            let mut peer = TcpStream::connect(address.trim())?;
            peer.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
            peer.write_all(&identity)?;
            peer.write_all(b"\n")?;
            let mut release = [0];
            peer.read_exact(&mut release)?;
            if release != *b"R" {
                return Err("invalid account barrier".into());
            }
            io::stdout().write_all(&identity)?;
            std::fs::write(home.join("stub-state"), &identity)?;
        }

        "malformed" => io::stdout().write_all(b"{malformed}\n")?,
        "truncated" => io::stdout().write_all(b"{\"type\":\"result\"")?,
        "input-required" => io::stdout().write_all(b"{\"state\":\"input_required\"}\n")?,
        "exit" => return Ok(23),
        "flood" => {
            let frame = [b'x'; 8192];
            for _ in 0..384 {
                io::stdout().write_all(&frame)?;
                io::stderr().write_all(&frame)?;
            }
        }
        "barrier" | "silence" => {
            // Explicit parent input releases the barrier, no sleeps or open waits.
            let mut byte = [0];
            io::stdin().read_exact(&mut byte)?;
            if byte != *b"R" {
                return Err("invalid fixture barrier".into());
            }
            if name == "barrier" {
                io::stdout().write_all(b"ready\nterminal\n")?;
            }
        }
        "mcp" => {
            // This scenario checks the transcript driver's framing/correlation;
            // it is a synthetic test transport, never an external recording.
            use std::io::BufRead;
            for line in io::stdin().lock().lines() {
                let request: serde_json::Value = serde_json::from_str(&line?)?;
                if let Some(id) = request.get("id") {
                    serde_json::to_writer(
                        io::stdout(),
                        &serde_json::json!({"jsonrpc":"2.0","id":id,"result":{}}),
                    )?;
                    io::stdout().write_all(b"\n")?;
                }
            }
        }
        "descendants" => {
            use provenant_supervisor::exec::{ExecOptions, Executor, SystemExecutor};
            let argv = [std::env::current_exe()?.into_os_string(), "barrier".into()];
            let environment: Vec<_> = std::env::vars_os().collect();
            let cwd = std::env::current_dir()?;
            let mut options = ExecOptions::new(&argv, &cwd, std::time::Duration::from_secs(3));
            options.env = Some(&environment);
            options.stdin = Some(b"R");
            let child = SystemExecutor::new(1)?.execute(options)?;
            if code(&child) != 0 {
                return Err("fixture descendant failed".into());
            }
            io::stdout().write_all(&child.stdout.bytes)?;
            io::stdout().write_all(b"reaped\n")?;
        }
        "record" => {
            // Only allowlisted fixture values, never the ambient environment.
            #[derive(serde::Serialize)]
            struct Record {
                argv: Vec<Vec<u8>>,
                agent_label: Option<String>,
            }
            serde_json::to_writer(
                io::stdout(),
                &Record {
                    argv: std::env::args_os()
                        .skip(1)
                        .map(|arg| arg.as_encoded_bytes().to_vec())
                        .collect(),
                    agent_label: std::env::var("PROVENANT_AGENT_LABEL").ok(),
                },
            )?;
        }
        _ => return Err("unknown fixture scenario".into()),
    }
    Ok(0)
}
fn suite() -> Result {
    let rig = TestRig::new()?;
    let executable = std::env::current_exe()?;
    for (name, expected) in [
        ("malformed", b"{malformed}\n".as_slice()),
        ("truncated", b"{\"type\":\"result\"".as_slice()),
        (
            "input-required",
            b"{\"state\":\"input_required\"}\n".as_slice(),
        ),
        ("silence", b"".as_slice()),
        ("barrier", b"ready\nterminal\n".as_slice()),
    ] {
        let out = rig.execute(&executable, &[name], Some(b"R"))?;
        assert_eq!(code(&out), 0);
        assert_eq!(out.stdout.bytes, expected);
    }
    let out = rig.execute(&executable, &["exit"], None)?;
    assert_eq!(code(&out), 23);
    let out = rig.execute(&executable, &["flood"], None)?;
    assert_eq!(code(&out), 0);
    assert!(out.stdout.truncated && out.stderr.truncated);
    assert_eq!(out.stdout.total_bytes, 3 * 1024 * 1024);
    let out = rig.execute(
        &executable,
        &["record", "fixture argument", "--fixture"],
        None,
    )?;
    let record: serde_json::Value = serde_json::from_slice(&out.stdout.bytes)?;
    assert_eq!(record["agent_label"], "fixture");
    assert_eq!(
        record["argv"],
        serde_json::json!([
            b"record".as_slice(),
            b"fixture argument".as_slice(),
            b"--fixture".as_slice()
        ])
    );
    let out = rig.execute(&executable, &["descendants"], None)?;
    assert_eq!(out.stdout.bytes, b"ready\nterminal\nreaped\n");
    let mut mcp = support::mcp::McpClient::default();
    let initialise = mcp.initialise("fixture-version")?;
    mcp.initialised()?;
    let list = mcp.list()?;
    let call = mcp.call("fixture", serde_json::json!({}))?;
    mcp.cancel(call)?;
    let replies = mcp.eof(&rig, &executable, &["mcp"])?;
    assert!(
        replies.contains_key(&initialise)
            && replies.contains_key(&list)
            && replies.contains_key(&call)
    );
    rig.assert_legacy_unchanged()
}
fn account_placement() -> Result {
    use provenant_confinement::permissions::{AccessMode, PathGrant, PathScope, PlanPermissions};
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    use provenant_providers::{
        Adapter, PrepareInput, Prompt, SetupStep, Stdin,
        codex::Codex,
        identity::{Accounts, RegisteredAccount},
    };
    use provenant_records::{
        ResolvedRoute, RouteConfidentiality, RouteEligibility, RouteReadiness,
    };
    use provenant_supervisor::exec::{ExecOptions, Executor, SystemExecutor};
    use std::{
        ffi::OsStr,
        fs,
        io::BufRead,
        net::{TcpListener, TcpStream},
        time::Duration,
    };
    let rig = TestRig::new()?;
    let executable = std::env::current_exe()?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let mut entries = Vec::new();
    for key in ["one", "two"] {
        let dir = rig.home.join(format!(".codex-{key}"));
        fs::create_dir(&dir)?;
        fs::write(dir.join("auth.json"), key)?;
        entries.push(RegisteredAccount::new("codex", key, dir)?);
    }
    let accounts = Accounts::new(entries)?;
    let mut specs = Vec::new();
    for key in ["one", "two"] {
        let scratch = rig.root.join(format!("scratch-{key}"));
        let state = rig.root.join(format!("state-{key}"));
        fs::create_dir(&scratch)?;
        fs::create_dir(&state)?;
        let grant = |path| PathGrant {
            path,
            scope: PathScope::Subtree,
        };
        let plan = ConfinedPlan::new(PlanRequest {
            scope: provenant_core::WorkScope::Project(provenant_core::ProjectId::from_bytes(
                [7; 16],
            )),
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
            permissions: PlanPermissions {
                read_only: None,
                mode: AccessMode::ReadOnly,
                scratch: grant(scratch),
                private_state: Some(grant(state.clone())),
                read_roots: vec![grant(rig.project.clone())],
                protected_roots: vec![],
                secret_roots: vec![],
                selected_authentication: Some("login".into()),
                include_continuation: false,
                network: vec![],
                capabilities: vec![],
                nested_confinement: false,
            },
        })?;
        let route = ResolvedRoute {
            adapter: "codex".into(),
            model: "fixture".into(),
            effort: None,
            catalogue_revision: "fixture".into(),
            account_key: Some(key.into()),
            host: "fixture".into(),
            alternatives: Default::default(),
            exclusions: vec![],
            eligibility: RouteEligibility::Eligible,
            readiness: RouteReadiness::Ready {
                availability_ref: "fixture".into(),
                expires_unix_seconds: i64::MAX,
            },
            confidentiality: RouteConfidentiality {
                evidence_ref: "fixture".into(),
            },
            pool_selection: None,
        };
        let prompt = Prompt::new(address.to_string());
        let account = accounts.selected(&route)?;
        let spec = Codex.prepare(PrepareInput {
            route: &route,
            confined: &plan,
            prompt: &prompt,
            continuation: None,
            grant_digest: "fixture",
            executable: &executable,
            home: &rig.home,
            user: OsStr::new("fixture"),
            account: Some(account),
            token: None,
            cwd: &rig.project,
            nonce: [1; 16],
            provenant: provenant_providers::LaneRuntime {
                executable: &executable,
                git: &std::fs::canonicalize(
                    provenant_cli::git::git_binary().ok_or("Git unavailable")?,
                )?,
                state: &rig.state,
                child_state: &rig.state.join(format!("children/{key}")),
                home: &rig.home,
                depth: 1,
                attempt: provenant_core::AttemptId::from_bytes([1; 16]),
                assets: &rig.project,
            },
        })?;
        let auth = account.directory().join("auth.json");
        let selected = &spec.profile.spec.authentication[0];
        assert_eq!(selected.roots.len(), 1);
        assert_eq!(selected.roots[0].path, auth);
        assert_eq!(selected.in_place_writes.len(), 1);
        assert_eq!(selected.in_place_writes[0].path, auth);
        for step in &spec.setup {
            match step {
                SetupStep::CreateDir(path) => std::fs::create_dir_all(path)?,
                SetupStep::WriteFile { path, bytes } => std::fs::write(path, bytes)?,
                SetupStep::Symlink { link, target } => std::os::unix::fs::symlink(target, link)?,
            }
        }
        specs.push((key, state, spec));
    }
    let coordinator = std::thread::spawn(move || -> std::io::Result<Vec<String>> {
        let mut peers = Vec::new();
        let mut identities = Vec::new();
        for _ in 0..2 {
            let (peer, _) = listener.accept()?;
            peer.set_read_timeout(Some(Duration::from_secs(5)))?;
            let mut line = String::new();
            std::io::BufReader::new(peer.try_clone()?).read_line(&mut line)?;
            identities.push(line.trim().to_owned());
            peers.push(peer);
        }
        for mut peer in peers {
            peer.write_all(b"R")?;
        }
        Ok(identities)
    });
    let results = std::thread::scope(|scope| {
        let children: Vec<_> = specs
            .iter()
            .map(|(key, state, spec)| {
                scope.spawn(move || -> std::result::Result<(), String> {
                    let Stdin::Bytes(input) = &spec.stdin else {
                        return Err("missing stdin".into());
                    };
                    let mut options =
                        ExecOptions::new(&spec.argv, &spec.cwd, Duration::from_secs(8));
                    options.env = Some(&spec.env);
                    options.stdin = Some(input);
                    let out = SystemExecutor::new(1)
                        .and_then(|e| e.execute(options))
                        .map_err(|e| e.to_string())?;
                    assert_eq!(code(&out), 0);
                    assert_eq!(out.stdout.bytes, key.as_bytes());
                    assert_eq!(
                        fs::read(state.join("stub-state")).map_err(|e| e.to_string())?,
                        key.as_bytes()
                    );
                    Ok(())
                })
            })
            .collect();
        children
            .into_iter()
            .map(|c| {
                c.join()
                    .unwrap_or_else(|_| Err("stub child panicked".into()))
            })
            .collect::<Vec<_>>()
    });
    // Wake a blocked accept even when a child failed before connecting.
    for _ in 0..2 {
        let _ = TcpStream::connect(address);
    }
    let mut seen = coordinator
        .join()
        .map_err(|_| "account coordinator panicked")??;
    for result in results {
        result?;
    }
    seen.sort();
    assert_eq!(seen, ["one", "two"]);
    rig.assert_legacy_unchanged()
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = if args.is_empty() || args[0].as_encoded_bytes().starts_with(b"--") {
        provenant_supervisor::fixture::run_libtest(
            &args,
            &["fake_provider", "account_placement"],
            |name| {
                let result = if name == "account_placement" {
                    account_placement()
                } else {
                    suite()
                };
                result.map_err(|error| io::Error::other(error.to_string()))
            },
        )
        .map(|()| 0)
        .map_err(Into::into)
    } else {
        args[0]
            .to_str()
            .ok_or_else(|| "fixture scenario must be UTF-8".into())
            .and_then(scenario)
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            let _ = writeln!(io::stderr(), "fake-provider: {error}");
            ExitCode::FAILURE
        }
    }
}
