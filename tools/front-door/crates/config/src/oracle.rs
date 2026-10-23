use std::{path::Path, process::{Command, Stdio}};

pub(crate) fn python(module: &str, body: &str, path: &Path) -> Option<Vec<u8>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let script = format!("import importlib.util,sys\nfrom pathlib import Path\nsys.path.insert(0, str(Path(sys.argv[1]).parent))\ns=importlib.util.spec_from_file_location('oracle',sys.argv[1]);m=importlib.util.module_from_spec(s);s.loader.exec_module(m)\np=Path(sys.argv[2])\n{body}");
    match Command::new("python3").args(["-c", &script]).arg(root.join(module)).arg(path).stdin(Stdio::null()).output() {
        Ok(done) => { assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr)); Some(done.stdout) }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => { eprintln!("skipping Python differential: python3 is absent"); None }
        Err(e) => panic!("cannot execute Python oracle: {e}"),
    }
}
