//! Derives the graph test module tree from the files under `src/graph/graph_tests/`,
//! so a new test file cannot be left out of the build by forgetting a line.
//!
//! Each `name.rs` becomes `mod name { include!(..) }`. Files in a sibling
//! `name/` directory are spliced into that module after its own file; they
//! declare their own modules and `use super::*` the helpers it defines.

use std::{env, error::Error, fs, path::Path, path::PathBuf};

type BuildResult<T> = Result<T, Box<dyn Error>>;

/// The sorted `.rs` files directly under `dir`.
fn rust_files(dir: &Path) -> BuildResult<Vec<PathBuf>> {
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn spliced(dir: &Path) -> BuildResult<String> {
    Ok(rust_files(dir)?
        .iter()
        .map(|file| format!("    include!({file:?});\n"))
        .collect())
}

fn modules(dir: &Path) -> BuildResult<String> {
    let mut stems = Vec::new();
    for path in rust_files(dir)? {
        let stem = path.file_stem().ok_or("graph test file has no stem")?;
        stems.push(stem.to_string_lossy().into_owned());
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        let stem = path.file_name().ok_or("graph test directory has no name")?;
        if !stems
            .iter()
            .any(|name| name.as_str() == stem.to_string_lossy())
        {
            return Err(format!("orphan graph test directory: {}", path.display()).into());
        }
        for child in fs::read_dir(&path)? {
            let child = child?.path();
            if child.is_dir() {
                return Err(format!(
                    "nested graph test directory is not dispatched: {}",
                    child.display()
                )
                .into());
            }
        }
    }
    let mut out = String::new();
    for stem in &stems {
        let nested = dir.join(stem);
        let children = if nested.is_dir() {
            spliced(&nested)?
        } else {
            String::new()
        };
        out.push_str(&format!(
            "mod {stem} {{\n    include!({:?});\n{children}}}\n",
            dir.join(format!("{stem}.rs"))
        ));
    }
    Ok(out)
}

fn main() -> BuildResult<()> {
    let manifest =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or("CARGO_MANIFEST_DIR unset")?);
    let out =
        PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR unset")?).join("graph_test_modules.rs");
    fs::write(out, modules(&manifest.join("src/graph/graph_tests"))?)?;
    Ok(())
}
