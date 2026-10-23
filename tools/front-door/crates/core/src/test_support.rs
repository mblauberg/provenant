use serde_json::Value;
use std::path::Path;
use std::process::Command;

pub(crate) fn python(module: &str, body: &str, paths: &[&Path]) -> Option<Value> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap();
    let prefix = r#"
import importlib.util, json, os, pathlib, sys
root = pathlib.Path(sys.argv[1])
spec = importlib.util.spec_from_file_location('oracle', root / sys.argv[2])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
"#;
    let result = Command::new("python3")
        .args(["-I", "-c", &format!("{prefix}\n{body}")])
        .arg(root)
        .arg(module)
        .args(paths)
        .output();
    match result {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("SKIP differential: python3 unavailable");
            None
        }
        Err(error) => panic!("cannot launch Python oracle: {error}"),
        Ok(output) => {
            assert!(
                output.status.success(),
                "Python oracle failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            Some(serde_json::from_slice(&output.stdout).unwrap())
        }
    }
}
