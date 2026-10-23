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
