//! One explicit, bounded oracle runner for the crate's differential tests.
use serde_json::Value;
use std::{
    fs,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn output(command: &mut Command) -> Option<Vec<u8>> {
    let dir = tempfile::tempdir().unwrap();
    let stdout = dir.path().join("stdout");
    let stderr = dir.path().join("stderr");
    let child = command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(fs::File::create(&stdout).unwrap())
        .stderr(fs::File::create(&stderr).unwrap())
        .spawn();
    let Ok(mut child) = child else {
        return None;
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            // kill is synchronous for these isolated interpreter fixtures;
            // observation and reaping still have a separate bounded deadline.
            let reap = Instant::now() + Duration::from_secs(5);
            while child.try_wait().unwrap().is_none() {
                assert!(Instant::now() < reap, "oracle did not reap");
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("oracle exceeded 30 second deadline");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(
        status.success(),
        "oracle failed: {}",
        String::from_utf8_lossy(&read_limited(&stderr))
    );
    let bytes = read_limited(&stdout);
    assert!(bytes.len() <= 1_048_576, "oracle output exceeded limit");
    Some(bytes)
}

fn read_limited(path: &Path) -> Vec<u8> {
    let mut bytes = vec![];
    fs::File::open(path)
        .unwrap()
        .take(1_048_577)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= 1_048_576, "oracle output exceeded limit");
    bytes
}

fn interpreter(kind: &str, default: &str, minimum: u64) -> Option<std::ffi::OsString> {
    let executable = std::env::var_os(kind).unwrap_or_else(|| default.into());
    let script = if default == "node" {
        "process.stdout.write(process.versions.node.split('.')[0])"
    } else {
        "import sys; print(sys.version_info.major * 100 + sys.version_info.minor)"
    };
    let mut command = Command::new(&executable);
    if default == "node" {
        command.args(["-e", script]);
    } else {
        command.args(["-I", "-c", script]);
    }
    let available = output(&mut command)
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .is_some_and(|version| version >= minimum);
    if available {
        return Some(executable);
    }
    assert_ne!(
        std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref(),
        Ok("1"),
        "{kind} is missing or too old"
    );
    eprintln!("skip: {kind} is missing or too old");
    None
}

pub(crate) fn node(script: &str, args: &[String]) -> Option<Value> {
    let node = interpreter("PROVENANT_PORT_NODE", "node", 24)?;
    let mut command = Command::new(node);
    // Production TS uses emitted .js specifiers. Resolve those to the actual
    // source files without copying, evaluating or rewriting source text.
    let hook = r#"import {registerHooks} from 'node:module';
registerHooks({resolve(specifier, context, next) {
 try { return next(specifier, context); } catch (error) {
  if (error.code === 'ERR_MODULE_NOT_FOUND' && specifier.endsWith('.js'))
   return next(specifier.slice(0,-3)+'.ts', context);
  throw error;
 }
}});"#;
    command
        .args([
            "--experimental-transform-types",
            "--input-type=module",
            "--import",
            &format!("data:text/javascript,{hook}"),
            "-e",
            script,
            "--",
        ])
        .args(args)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../runtime/fabric"));
    Some(
        serde_json::from_slice(
            &output(&mut command).expect("oracle interpreter disappeared after availability check"),
        )
        .unwrap(),
    )
}

pub(crate) fn python(script: &str, args: &[String]) -> Option<Value> {
    let python = interpreter("PROVENANT_PORT_PYTHON", "python3", 312)?;
    let mut command = Command::new(python);
    command.args(["-I", "-c", script]).args(args);
    Some(
        serde_json::from_slice(
            &output(&mut command).expect("oracle interpreter disappeared after availability check"),
        )
        .unwrap(),
    )
}
