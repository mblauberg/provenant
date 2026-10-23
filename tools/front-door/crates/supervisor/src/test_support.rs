use std::path::Path;
use std::process::Command;

pub(crate) fn python(root: &Path, script: &str) -> Option<serde_json::Value> {
    let output = match Command::new("python3")
        .args(["-c", script])
        .current_dir(root)
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipped differential fixture: python3 is absent");
            return None;
        }
        Err(error) => panic!("could not run Python oracle: {error}"),
    };
    assert!(
        output.status.success(),
        "Python oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(serde_json::from_slice(&output.stdout).expect("Python oracle JSON"))
}
