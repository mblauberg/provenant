use std::{
    ffi::OsString,
    fs, io,
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(20);

fn wait_bounded(child: &mut Child) -> io::Result<ExitStatus> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            match child.kill() {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
                Err(error) => return Err(error),
            }
            let reap_deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if child.try_wait()?.is_some() {
                    break;
                }
                if Instant::now() >= reap_deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "killed Python oracle did not exit within 2 seconds",
                    ));
                }
                thread::sleep(Duration::from_millis(10));
            }
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Python oracle exceeded its 20 second limit",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn run(
    program: &str,
    arguments: &[OsString],
    root: &Path,
) -> io::Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
    let output_dir = tempfile::tempdir()?;
    let stdout_path = output_dir.path().join("stdout");
    let stderr_path = output_dir.path().join("stderr");
    let stdout = fs::File::create(&stdout_path)?;
    let stderr = fs::File::create(&stderr_path)?;
    let mut command = Command::new(program);
    command
        .args(arguments)
        .current_dir(root)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    let mut child = command.spawn()?;
    let status = wait_bounded(&mut child)?;
    Ok((status, fs::read(stdout_path)?, fs::read(stderr_path)?))
}

fn interpreter(root: &Path) -> Option<String> {
    let program = std::env::var("PROVENANT_PORT_PYTHON").unwrap_or_else(|_| "python3".into());
    let (status, stdout, stderr) = match run(&program, &["--version".into()], root) {
        Ok(output) => output,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            unavailable("interpreter is absent");
            return None;
        }
        Err(error) => panic!("cannot check Python oracle version: {error}"),
    };
    if !status.success() {
        panic!(
            "Python oracle version check failed: {}",
            String::from_utf8_lossy(&stderr)
        );
    }
    let version = String::from_utf8_lossy(&stdout);
    let Some((major, minor)) = version
        .split_whitespace()
        .last()
        .and_then(|s| s.split_once('.'))
        .and_then(|(major, rest)| rest.split('.').next().map(|minor| (major, minor)))
        .and_then(|(major, minor)| Some((major.parse::<u32>().ok()?, minor.parse::<u32>().ok()?)))
    else {
        panic!("cannot parse Python oracle version: {version}");
    };
    if (major, minor) < (3, 12) {
        unavailable("Python 3.12 or newer is required");
        return None;
    }
    Some(program)
}

fn unavailable(reason: &str) {
    if std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref() == Ok("1") {
        panic!("Python differential required but {reason}");
    }
    eprintln!("skip: Python differential {reason}");
}

pub(crate) fn python(module: &str, body: &str, path: &Path) -> Option<Vec<u8>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let root = root.canonicalize().expect("repository root exists");
    let program = interpreter(&root)?;
    let script = format!("import importlib.util,importlib.machinery,sys\nfrom pathlib import Path\nsys.path.insert(0, str(Path(sys.argv[1]).parent))\ns=importlib.util.spec_from_file_location('oracle',sys.argv[1],loader=importlib.machinery.SourceFileLoader('oracle',sys.argv[1]));m=importlib.util.module_from_spec(s);sys.modules[s.name]=m;s.loader.exec_module(m)\np=Path(sys.argv[2])\n{body}");
    let module_path = root.join(module);
    let args = [
        OsString::from("-I"),
        OsString::from("-c"),
        OsString::from(script),
        module_path.into_os_string(),
        path.as_os_str().to_owned(),
    ];
    let run = run(&program, &args, &root);
    match run {
        Ok((status, stdout, stderr)) => {
            assert!(status.success(), "{}", String::from_utf8_lossy(&stderr));
            Some(stdout)
        }
        Err(error) => panic!("cannot run bounded Python oracle: {error}"),
    }
}
