//! One bounded, explicit-environment helper for each differential interpreter.
use crate::bounded::{run_bounded, BoundedError, Options};
use std::ffi::OsString;
use std::path::Path;

pub(crate) fn environment() -> Vec<(OsString, OsString)> {
    let mut env: Vec<_> = ["PATH", "HOME", "TMPDIR", "TZ", "LANG", "LC_ALL"]
        .into_iter()
        .filter_map(|key| std::env::var_os(key).map(|value| (key.into(), value)))
        .collect();
    if !env.iter().any(|(key, _)| key == "PATH") {
        env.push(("PATH".into(), "/usr/bin:/bin".into()));
    }
    env
}

pub(crate) fn python(root: &Path, script: &str) -> Option<serde_json::Value> {
    oracle(root, script, false)
}
pub(crate) fn node(root: &Path, script: &str) -> Option<serde_json::Value> {
    oracle(root, script, true)
}
fn oracle(root: &Path, script: &str, node: bool) -> Option<serde_json::Value> {
    let (name, default, probe, flags): (_, _, _, &[&str]) = if node {
        (
            "PROVENANT_PORT_NODE",
            "node",
            "process.exit(Number(process.versions.node.split('.')[0]) >= 24 ? 0 : 75)",
            &["--input-type=module", "-e"],
        )
    } else {
        (
            "PROVENANT_PORT_PYTHON",
            "python3",
            "import sys; sys.exit(0 if sys.version_info >= (3, 12) else 75)",
            &["-I", "-c"],
        )
    };
    let interpreter = std::env::var_os(name).unwrap_or_else(|| default.into());
    let env = environment();
    let run = |code: &str| {
        let command: Vec<OsString> = std::iter::once(interpreter.clone())
            .chain(flags.iter().map(OsString::from))
            .chain([code.into()])
            .collect();
        let mut options = Options::new(root, 30.0);
        options.env = Some(&env);
        options.merge_stderr = false;
        options.output_limit_bytes = 4 * 1_048_576;
        run_bounded(&command, options)
    };
    // Probe separately so a source fixture's exit(75) is always a failure.
    let version = match run(probe) {
        Ok(output) => output,
        Err(BoundedError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
            unavailable(&format!("{default} oracle is absent"));
            return None;
        }
        Err(error) => panic!("could not run {default} oracle: {error}"),
    };
    if version.returncode == 75 && !version.timed_out {
        unavailable(if node {
            "Node oracle requires >=24"
        } else {
            "Python oracle requires >=3.12"
        });
        return None;
    }
    assert_eq!(
        version.returncode, 0,
        "{default} version probe failed: {:?}",
        version.stderr
    );
    assert!(!version.timed_out, "{default} version probe timed out");
    let source_script = if node {
        script.to_owned()
    } else {
        // -I removes cwd from sys.path. Add only the explicit oracle root,
        // never PYTHONPATH or user-site imports.
        format!(
            "import sys; from pathlib import Path; sys.path.insert(0, str(Path.cwd()));\n{script}"
        )
    };
    let output =
        run(&source_script).unwrap_or_else(|e| panic!("could not run {default} oracle: {e}"));
    assert!(!output.timed_out, "{default} oracle timed out");
    assert!(
        !output.output_truncated,
        "{default} oracle output exceeded bound"
    );
    assert_eq!(
        output.returncode, 0,
        "{default} oracle failed: {:?}",
        output.stderr
    );
    Some(serde_json::from_str(&output.output).expect("oracle JSON"))
}
fn unavailable(reason: &str) {
    eprintln!("skip: {reason}");
    assert_ne!(
        std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref(),
        Ok("1"),
        "required differential: {reason}"
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn python_oracles_run_in_isolated_mode() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        if let Some(value) = super::python(
            &root,
            "import json, sys; print(json.dumps(sys.flags.isolated))",
        ) {
            assert_eq!(value, serde_json::json!(1));
        }
    }
}
