//! Test-only Python oracle, with an explicit minimal subprocess environment.
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
    io::{Read, Write},
    process::{Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

fn unavailable(reason: &str) -> Option<Command> {
    eprintln!("skip: {reason}");
    assert!(
        std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref() != Ok("1"),
        "required Python differential unavailable: {reason}"
    );
    None
}

/// Both the version probe and the oracle itself use this bounded collector.
fn run(command: &mut Command, input: Vec<u8>) -> std::io::Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    const LIMIT: u64 = 32 * 1024 * 1024;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let collect = |stream: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut output = Vec::new();
            stream.take(LIMIT + 1).read_to_end(&mut output).unwrap();
            assert!(
                output.len() <= LIMIT as usize,
                "oracle output exceeded 32 MiB"
            );
            output
        })
    };
    let stdout = collect(Box::new(stdout));
    let stderr = collect(Box::new(stderr));
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut timed_out = false;
    let status = 'observe: loop {
        match child.try_wait()? {
            Some(status) => break status,
            None if Instant::now() < deadline => std::thread::yield_now(),
            None => {
                child.kill()?;
                timed_out = true;
                let reap_deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    if let Some(status) = child.try_wait()? {
                        break 'observe status;
                    }
                    assert!(
                        Instant::now() < reap_deadline,
                        "oracle did not reap after kill"
                    );
                    std::thread::yield_now();
                }
            }
        }
    };
    writer.join().unwrap()?;
    let stdout = stdout.join().unwrap();
    let stderr = stderr.join().unwrap();
    assert!(!timed_out, "Python oracle exceeded 60 second deadline");
    Ok((status, stdout, stderr))
}

fn command() -> Option<Command> {
    let interpreter = std::env::var_os("PROVENANT_PORT_PYTHON").unwrap_or_else(|| "python3".into());
    let make = || {
        let mut command = Command::new(&interpreter);
        command.env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        command.args(["-I", "-B"]);
        command
    };
    let output = match run(
        make().args([
            "-c",
            "import json,sys; print(json.dumps(list(sys.version_info[:2])))",
        ]),
        vec![],
    ) {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return unavailable("Python interpreter absent");
        }
        Err(error) => panic!("could not probe Python oracle: {error}"),
    };
    assert!(
        output.0.success(),
        "Python version probe failed: {}",
        String::from_utf8_lossy(&output.2)
    );
    let version: Vec<u32> = serde_json::from_slice(&output.1).unwrap();
    if version.as_slice() < [3, 12].as_slice() {
        return unavailable("Python oracle requires >=3.12");
    }
    Some(make())
}
pub(crate) fn python<T: DeserializeOwned>(
    code: &str,
    input: &Value,
    api_key: Option<&str>,
) -> Option<T> {
    let mut command = command()?;
    if let Some(key) = api_key {
        command.env("ANTHROPIC_API_KEY", key);
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap();
    command
        .args([
            "-c",
            &format!("import os,sys; sys.path.insert(0, os.getcwd()); {code}"),
        ])
        .current_dir(root);
    let (status, stdout, stderr) = run(&mut command, serde_json::to_vec(input).unwrap()).unwrap();
    assert!(
        status.success(),
        "Python oracle failed: {}",
        String::from_utf8_lossy(&stderr)
    );
    Some(crate::decode_json(&stdout).unwrap())
}
