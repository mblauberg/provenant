use std::path::Path;
use std::process::Command;

pub(crate) fn python(root: &Path, script: &str) -> Option<serde_json::Value> {
    let interpreter = std::env::var_os("PROVENANT_PORT_PYTHON").unwrap_or_else(|| "python3".into());
    // Probe separately so a source fixture's exit(75) is still a failure.
    let version = match Command::new(&interpreter)
        .args([
            "-c",
            "import sys; sys.exit(0 if sys.version_info >= (3, 12) else 75)",
        ])
        .current_dir(root)
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            unavailable("Python oracle is absent");
            return None;
        }
        Err(error) => panic!("could not run Python oracle: {error}"),
    };
    if version.status.code() == Some(75) {
        unavailable("Python oracle requires >=3.12");
        return None;
    }
    assert!(version.status.success(), "Python version probe failed");
    let output = Command::new(&interpreter)
        .args(["-c", script])
        .current_dir(root)
        .output()
        .expect("could not run Python oracle");
    assert!(
        output.status.success(),
        "Python oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(serde_json::from_slice(&output.stdout).expect("Python oracle JSON"))
}

fn unavailable(reason: &str) {
    assert_ne!(
        std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref(),
        Ok("1"),
        "required differential: {reason}"
    );
    eprintln!("skip: {reason}");
}
