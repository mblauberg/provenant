//! One harness=false target: current_exe is the exact fake-provider executable.
mod support;
use std::{
    io::{self, Read, Write},
    process::ExitCode,
};
use support::{Result, TestRig, code};

fn scenario(name: &str) -> Result<u8> {
    match name {
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
fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = if args.is_empty() || args[0].as_encoded_bytes().starts_with(b"--") {
        provenant_supervisor::fixture::run_libtest(&args, &["fake_provider"], |_| {
            suite().map_err(|error| io::Error::other(error.to_string()))
        })
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
