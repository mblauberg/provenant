//! Derives the graph test module tree from the files under `src/graph/graph_tests/`,
//! so a new test file cannot be left out of the build by forgetting a line.
//!
//! Each `name.rs` becomes `mod name { include!(..) }`. Files in a sibling
//! `name/` directory are spliced into that module after its own file; they
//! declare their own modules and `use super::*` the helpers it defines.

use std::{env, fs, path::Path, path::PathBuf};

fn spliced(dir: &Path) -> String {
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .expect("graph test directory")
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    files.sort();
    files
        .iter()
        .map(|file| format!("    include!({file:?});\n"))
        .collect()
}

fn modules(dir: &Path) -> String {
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut stems: Vec<String> = fs::read_dir(dir)
        .expect("graph test directory")
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .map(|path| {
            path.file_stem()
                .expect("file stem")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    stems.sort();
    for entry in fs::read_dir(dir).expect("graph test directory") {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            let stem = path.file_name().expect("directory name").to_string_lossy();
            assert!(
                stems.iter().any(|name| name == &*stem),
                "orphan graph test directory: {}",
                path.display()
            );
            for child in fs::read_dir(&path).expect("graph test subdirectory") {
                let child = child.expect("directory entry").path();
                assert!(
                    !child.is_dir(),
                    "nested graph test directory is not dispatched: {}",
                    child.display()
                );
            }
        }
    }
    stems
        .iter()
        .map(|stem| {
            let nested = dir.join(stem);
            let children = if nested.is_dir() {
                spliced(&nested)
            } else {
                String::new()
            };
            format!(
                "mod {stem} {{\n    include!({:?});\n{children}}}\n",
                dir.join(format!("{stem}.rs"))
            )
        })
        .collect()
}

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("out dir")).join("graph_test_modules.rs");
    fs::write(out, modules(&manifest.join("src/graph/graph_tests")))
        .expect("write graph test modules");
}
