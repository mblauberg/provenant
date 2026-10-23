//! Temporary safe OS helpers until the supervisor exposes these contracts.
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

// port: belongs in provenant-supervisor
pub(crate) fn lock(path: &Path) -> io::Result<File> {
    fs::create_dir_all(parent(path))?;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(path)?;
    file.lock()?;
    Ok(file)
}

// port: belongs in provenant-core
pub(crate) fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

// port: belongs in provenant-supervisor
pub(crate) fn read_bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "record exceeds size limit",
        ));
    }
    Ok(bytes)
}

// port: belongs in provenant-supervisor
pub(crate) fn publish_lease(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::create_dir_all(parent(path))?;
    // create_new gives independent calls distinct temporary files without global state.
    for serial in 0..1024 {
        let mut name = std::ffi::OsString::from(".");
        name.push(
            path.file_name()
                .ok_or_else(|| io::Error::other("missing lease filename"))?,
        );
        name.push(format!(".{}.{serial}", std::process::id()));
        let temp = parent(path).join(name);
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
        {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let result = (|| {
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&temp, path)
        })();
        let cleanup = fs::remove_file(&temp);
        return match (result, cleanup) {
            (Err(e), _) => Err(e),
            (Ok(()), Err(e)) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "lease temporary files exhausted",
    ))
}

// port: belongs in provenant-records
pub(crate) fn ascii_json(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            for unit in c.encode_utf16(&mut [0; 2]) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

// port: belongs in provenant-records
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub schema_version: u8,
    pub status: String,
    pub holder: String,
    pub previous_holder: String,
    pub generation: i64,
    pub updated_at: String,
    pub expires_at: String,
    pub handoff_evidence: String,
}
