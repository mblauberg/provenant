//! Optional oracle runner. Fixtures contain no provider state or credentials.
use serde_json::Value;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

const ORACLE_TIMEOUT: Duration = Duration::from_secs(30);
const REAP_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_ORACLE_IO_BYTES: usize = 16 * 1024 * 1024;

fn kill_and_reap_bounded(child: &mut std::process::Child) {
    let _ = child.kill();
    let reap_deadline = Instant::now() + REAP_TIMEOUT;
    while Instant::now() < reap_deadline {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) | Err(_) => std::thread::sleep(POLL_INTERVAL),
        }
    }
}

fn bounded_status(child: &mut std::process::Child) -> io::Result<ExitStatus> {
    let deadline = Instant::now() + ORACLE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => (),
            Err(error) => {
                kill_and_reap_bounded(child);
                return Err(error);
            }
        }
        if Instant::now() >= deadline {
            kill_and_reap_bounded(child);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Python oracle exceeded the 30-second deadline",
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn read_bounded(file: &mut File, stream: &str) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(MAX_ORACLE_IO_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_ORACLE_IO_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Python oracle {stream} exceeded the 16 MiB limit"),
        ));
    }
    Ok(bytes)
}

fn bounded_output(mut command: Command, input: &[u8]) -> io::Result<Output> {
    if input.len() > MAX_ORACLE_IO_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Python oracle input exceeded the 16 MiB limit",
        ));
    }

    let mut stdin = tempfile::tempfile()?;
    stdin.write_all(input)?;
    stdin.seek(SeekFrom::Start(0))?;
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;

    command
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::from(stderr.try_clone()?));
    let mut child = command.spawn()?;
    let status = bounded_status(&mut child)?;
    Ok(Output {
        status,
        stdout: read_bounded(&mut stdout, "stdout")?,
        stderr: read_bounded(&mut stderr, "stderr")?,
    })
}

pub fn oracle(script: &str, input: &Value) -> Option<Value> {
    let python = std::env::var_os("PROVENANT_PORT_PYTHON").unwrap_or_else(|| "python3".into());
    let required = std::env::var("PROVENANT_PORT_DIFF_REQUIRED").is_ok_and(|v| v == "1");
    let unavailable = |reason: &str| {
        if required {
            panic!("PROVENANT_PORT_DIFF_REQUIRED=1 but Python oracle is unavailable: {reason}");
        }
        eprintln!("skip: Python oracle {reason}");
    };
    let path = std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());
    let explicit_environment = || {
        let mut command = Command::new(&python);
        command
            .env_clear()
            .env("PATH", &path)
            .env("LC_ALL", "C.UTF-8");
        command
    };
    let mut version_command = explicit_environment();
    version_command.args([
        "-I",
        "-c",
        "import sys; raise SystemExit(0 if sys.version_info >= (3, 12) else 1)",
    ]);
    match bounded_output(version_command, &[]) {
        Ok(output) if output.status.success() => (),
        Ok(_) => {
            unavailable("requires Python >= 3.12");
            return None;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            unavailable("interpreter not found");
            return None;
        }
        Err(error) if error.kind() == io::ErrorKind::TimedOut => {
            panic!("Python version oracle timed out: {error}");
        }
        Err(error) => {
            unavailable(&format!("could not start: {error}"));
            return None;
        }
    }
    let scripts = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../skills/orchestrate/scripts");
    let skills = scripts.join("../..");
    let mut command = explicit_environment();
    command
        .args(["-I", "-c", &format!("import sys, json\nsys.dont_write_bytecode=True\nsys.path[:0] = [sys.argv[1], sys.argv[2]]\nfixture=json.load(sys.stdin)\n{script}")])
        .arg(scripts)
        .arg(skills);
    let input = serde_json::to_vec(input).expect("JSON Value serialization is infallible");
    let output = bounded_output(command, &input).unwrap_or_else(|error| {
        panic!("Python oracle did not finish within its bounded limits: {error}")
    });
    assert!(
        output.status.success(),
        "oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(provenant_records::checked_json::from_slice(&output.stdout).unwrap())
}

#[test]
fn interfaces_match_python_policy_and_wire_values() {
    use crate::{
        agy_home::HomePlan, instruction_quarantine::quarantinable, lane_protection::is_full_sha,
    };
    let plans: Vec<_> = ["agy", "codex", "claude"].into_iter().flat_map(|adapter|
        ["full", "read_only", "workspace_write"].into_iter().flat_map(move |sandbox|
            [None, Some("sandbox-exec")].into_iter().map(move |confinement|
                serde_json::json!({"adapter": adapter, "applied": {"sandbox": sandbox, "confinement": confinement}})))).collect();
    let changes = vec![
        vec![],
        vec![".agents/x"],
        vec![".AGENTS/x"],
        vec![".agents/x (unresolved conflict)"],
        vec!["src/x"],
    ];
    let shas = vec![
        "a".repeat(40),
        "0".repeat(64),
        "A".repeat(40),
        "a".repeat(41),
    ];
    let fixture = serde_json::json!({"plans": plans, "changes": changes, "shas": shas});
    let Some(actual) = oracle(
        r#"
import agy_home, instruction_quarantine, lane_protection
print(json.dumps({
    'home': [p['adapter'] == 'agy' and p['applied'].get('confinement') == 'sandbox-exec' for p in fixture['plans']],
    'shim': [agy_home.needs_ps_shim(p) for p in fixture['plans']],
    'quarantine': [instruction_quarantine.quarantinable(c) for c in fixture['changes']],
    'sha': [bool(lane_protection.SHA.fullmatch(s)) for s in fixture['shas']]
}))
"#,
        &fixture,
    ) else {
        return;
    };
    let projections: Vec<_> = plans
        .iter()
        .map(|p| HomePlan {
            adapter: p["adapter"].as_str().unwrap(),
            sandbox: p["applied"]["sandbox"].as_str().unwrap(),
            confinement: p["applied"]["confinement"].as_str(),
        })
        .collect();
    assert_eq!(
        actual,
        serde_json::json!({
            "home": projections.iter().map(|p| p.stages_home()).collect::<Vec<_>>(),
            "shim": projections.iter().map(|p| p.needs_ps_shim(if cfg!(target_os="macos") {"darwin"} else {"linux"})).collect::<Vec<_>>(),
            "quarantine": changes.iter().map(|c| quarantinable(c.iter().copied())).collect::<Vec<_>>(),
            "sha": shas.iter().map(|s| is_full_sha(s)).collect::<Vec<_>>()
        })
    );
}
