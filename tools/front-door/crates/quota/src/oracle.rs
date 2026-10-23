//! Test-only interpreter selection and subprocess boundary (PORTING.md R9).
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
    ffi::OsStr,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::OnceLock,
};

#[derive(Clone, Copy)]
pub(crate) enum Interpreter {
    Python,
    Node,
}
impl Interpreter {
    pub(crate) fn supported_version(self, output: &str) -> bool {
        let version = match self {
            Self::Python => output.trim().strip_prefix("Python "),
            Self::Node => output.trim().strip_prefix('v'),
        };
        let Some(version) = version else { return false };
        let mut parts = version.split('.').map(str::parse::<u32>);
        match (self, parts.next(), parts.next()) {
            (Self::Python, Some(Ok(major)), Some(Ok(minor))) => (major, minor) >= (3, 12),
            (Self::Node, Some(Ok(major)), Some(Ok(_))) => major >= 24,
            _ => false,
        }
    }
}

pub(crate) fn interpreter(language: Interpreter) -> Option<&'static Path> {
    static PYTHON: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    static NODE: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    let (cache, variable, default, floor) = match language {
        Interpreter::Python => (&PYTHON, "PROVENANT_PORT_PYTHON", "python3", "Python 3.12+"),
        Interpreter::Node => (&NODE, "PROVENANT_PORT_NODE", "node", "Node 24+"),
    };
    let result = cache.get_or_init(|| {
        let path = PathBuf::from(std::env::var_os(variable).unwrap_or_else(|| default.into()));
        let supported = Command::new(&path)
            .arg("--version")
            .output()
            .is_ok_and(|out| {
                out.status.success()
                    && language.supported_version(&String::from_utf8_lossy(&out.stdout))
            });
        if supported {
            Ok(path)
        } else {
            Err(format!(
                "differential oracle requires {floor}; {} is missing or unsupported ({variable})",
                path.display()
            ))
        }
    });
    match result {
        Ok(path) => Some(path),
        Err(reason) => {
            assert!(
                std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref() != Ok("1"),
                "{reason}"
            );
            eprintln!("skip: {reason}");
            None
        }
    }
}

pub(crate) fn run<T: DeserializeOwned>(
    language: Interpreter,
    args: &[&OsStr],
    input: &Value,
) -> Option<T> {
    let mut child = Command::new(interpreter(language)?)
        .args(args)
        .env("TZ", "UTC")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(input).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "oracle: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(crate::decode_record(&output.stdout).unwrap())
}

#[test]
fn node_floor_is_24() {
    for (text, supported) in [
        ("v22.18.0", false),
        ("v23.9.0", false),
        ("v24.0.0", true),
        ("v25.1.0\n", true),
        ("node", false),
    ] {
        assert_eq!(
            Interpreter::Node.supported_version(text),
            supported,
            "{text}"
        );
    }
}
