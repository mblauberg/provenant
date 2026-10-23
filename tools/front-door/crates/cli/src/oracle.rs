//! Test-only, isolated and bounded Python oracle. Production never invokes Python.
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
    fs::File,
    io::{Read, Seek, Write},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn run(
    program: &std::ffi::OsStr,
    args: &[&std::ffi::OsStr],
    input: &[u8],
    environment: Option<&[(&str, &str)]>,
) -> std::io::Result<std::process::Output> {
    let mut stdin = tempfile::tempfile()?;
    stdin.write_all(input)?;
    stdin.rewind()?;
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    let mut command = Command::new(program);
    command.args(args).env_clear();
    if let Some(environment) = environment {
        command.envs(environment.iter().copied());
    } else {
        for key in ["PATH", "HOME", "TMPDIR", "SYSTEMROOT"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8");
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let mut child = command
        .current_dir(root)
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::from(stderr.try_clone()?))
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill()?;
            let cleanup = Instant::now() + Duration::from_secs(2);
            while child.try_wait()?.is_none() {
                if Instant::now() >= cleanup {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "oracle cleanup timed out",
                    ));
                }
                std::thread::yield_now();
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "oracle timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    fn bytes(file: &mut File) -> std::io::Result<Vec<u8>> {
        file.rewind()?;
        let mut result = Vec::new();
        file.take(16 * 1024 * 1024).read_to_end(&mut result)?;
        Ok(result)
    }
    Ok(std::process::Output {
        status,
        stdout: bytes(&mut stdout)?,
        stderr: bytes(&mut stderr)?,
    })
}

pub(crate) fn python<T: DeserializeOwned>(code: &str, input: &Value) -> Option<T> {
    let interpreter = std::env::var_os("PROVENANT_PORT_PYTHON").unwrap_or_else(|| "python3".into());
    let version = run(
        &interpreter,
        &[
            "-I".as_ref(),
            "-c".as_ref(),
            "import sys; print(int(sys.version_info >= (3,12)))".as_ref(),
        ],
        b"",
        None,
    );
    let available = match version {
        Ok(output) => output.status.success() && output.stdout == b"1\n",
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => panic!("Python oracle probe failed: {e}"),
    };
    if !available {
        eprintln!(
            "skip: Python oracle {:?} absent or older than 3.12",
            interpreter
        );
        assert_ne!(
            std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref(),
            Ok("1"),
            "required Python oracle unavailable"
        );
        return None;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap();
    let code = format!("import sys; sys.path.insert(0,sys.argv[1]);\n{code}");
    let output = run(
        &interpreter,
        &[
            "-I".as_ref(),
            "-c".as_ref(),
            code.as_ref(),
            root.as_os_str(),
        ],
        &serde_json::to_vec(input).unwrap(),
        None,
    )
    .unwrap();
    assert!(
        output.status.success(),
        "Python oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

pub(crate) fn utility(
    program: &std::ffi::OsStr,
    args: &[std::ffi::OsString],
    environment: &[(&str, &str)],
) -> std::io::Result<std::process::Output> {
    let refs: Vec<_> = args.iter().map(|s| s.as_os_str()).collect();
    run(program, &refs, b"", Some(environment))
}
