//! Temporary shared primitives until their owners land on main.
use std::{fs, io::Write, path::{Component, Path, PathBuf}};
use crate::{io, Result};

// port: belongs in provenant-core
pub(crate) fn resolve(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() { path.to_path_buf() } else {
        std::env::current_dir().map_err(|e| io(path, e))?.join(path)
    };
    resolve_inner(&absolute, 0)
}

// port: belongs in provenant-core
fn resolve_inner(path: &Path, depth: usize) -> Result<PathBuf> {
    if depth > 40 { return Err(crate::invalid(format!("symlink loop: {}", path.display()))); }
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => { result.pop(); }
            Component::CurDir => {}
            other => {
                result.push(other.as_os_str());
                match fs::symlink_metadata(&result) {
                    Ok(meta) if meta.is_symlink() => {
                        let link = fs::read_link(&result).map_err(|e| io(&result, e))?;
                        let target = if link.is_absolute() { link } else { result.parent().unwrap_or(Path::new("/")).join(link) };
                        result = resolve_inner(&target, depth + 1)?;
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io(&result, e)),
                }
            }
        }
    }
    Ok(result)
}

// port: belongs in provenant-supervisor
// Safe std-only staging. Create-new prevents clobbering another staging file.
pub(crate) fn atomic_write(path: &Path, prefix: &str, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
    for sequence in 0..1024 {
        let temp = parent.join(format!("{prefix}{}.{sequence}.tmp", std::process::id()));
        let mut file = match fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temp) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io(&temp, e)),
        };
        let result = (|| {
            file.write_all(bytes).map_err(|e| io(&temp, e))?;
            file.sync_all().map_err(|e| io(&temp, e))?;
            drop(file);
            fs::rename(&temp, path).map_err(|e| io(path, e))
        })();
        let cleanup = fs::remove_file(&temp);
        result?;
        if let Err(e) = cleanup {
            if e.kind() != std::io::ErrorKind::NotFound { return Err(io(&temp, e)); }
        }
        return Ok(());
    }
    Err(crate::invalid("could not reserve an installation staging file"))
}

// port: belongs in provenant-supervisor
pub(crate) fn replace_link(destination: &Path, source: &Path) -> Result<()> {
    let parent = destination.parent().unwrap_or(Path::new("."));
    for sequence in 0..1024 {
        let temp = parent.join(format!(".workflow.{}.{sequence}.tmp", std::process::id()));
        match std::os::unix::fs::symlink(source, &temp) {
            Ok(()) => {},
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io(&temp, e)),
        }
        let result = fs::rename(&temp, destination).map_err(|e| io(destination, e));
        let cleanup = fs::remove_file(&temp);
        result?;
        if let Err(e) = cleanup {
            if e.kind() != std::io::ErrorKind::NotFound { return Err(io(&temp, e)); }
        }
        return Ok(());
    }
    Err(crate::invalid("could not reserve a workflow staging link"))
}

// port: belongs in provenant-records
pub(crate) fn json_bytes<T: serde::Serialize>(record: &T) -> Result<Vec<u8>> {
    // Value sorts object keys recursively; Python json.dump defaults to ASCII.
    let value = serde_json::to_value(record).map_err(|e| crate::invalid(e.to_string()))?;
    let text = serde_json::to_string_pretty(&value).map_err(|e| crate::invalid(e.to_string()))?;
    let mut output = String::new();
    for c in text.chars() {
        if c.is_ascii() { output.push(c); }
        else {
            use std::fmt::Write;
            for unit in c.encode_utf16(&mut [0; 2]) {
                write!(&mut output, "\\u{unit:04x}").map_err(|e| crate::invalid(e.to_string()))?;
            }
        }
    }
    output.push('\n');
    Ok(output.into_bytes())
}

// port: belongs in provenant-records
pub(crate) fn schema_one(value: &serde_json::Value) -> bool {
    value == &serde_json::Value::Bool(true) || value.as_f64() == Some(1.0)
}

// port: belongs in provenant-records
pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes.iter().flat_map(|b| [DIGITS[(b >> 4) as usize] as char, DIGITS[(b & 15) as usize] as char]).collect()
}
