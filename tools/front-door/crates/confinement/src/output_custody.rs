//! Identity-bound publication over the supervisor's pending safe filesystem contract.
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub use crate::pending::{CustodyFs, FileKind, Metadata};

#[derive(Debug, Error)]
pub enum CustodyError {
    #[error("{0}")]
    Refused(&'static str),
    #[error("{operation} {path:?}: {source}")]
    Io { operation: &'static str, path: OsString, #[source] source: io::Error },
}

fn io_error(operation: &'static str, path: &OsStr, source: io::Error) -> CustodyError {
    CustodyError::Io { operation, path: path.to_owned(), source }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstalledOutput {
    pub digest: String,
    pub device: u64,
    pub inode: u64,
}

pub enum Source<'a, F> { Path(&'a Path), Handle(&'a F) }

/// Split lexical bytes: Path::components would discard a trailing slash or final dot.
#[cfg(unix)]
pub fn destination_parts(path: &Path) -> Result<(bool, Vec<OsString>, OsString), CustodyError> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let bytes = path.as_os_str().as_bytes();
    let mut parts: Vec<_> = bytes.split(|c| *c == b'/').collect();
    let leaf = parts.pop().unwrap_or_default();
    if leaf.is_empty() || leaf == b"." || leaf == b".." {
        return Err(CustodyError::Refused("output destination has no file name"));
    }
    if parts.contains(&&b".."[..]) {
        return Err(CustodyError::Refused("output destination may not traverse a parent directory"));
    }
    Ok((bytes.starts_with(b"/"), parts.into_iter().filter(|s| !s.is_empty() && *s != b".")
        .map(|s| OsString::from_vec(s.to_vec())).collect(), OsString::from_vec(leaf.to_vec())))
}

fn open_parent<O: CustodyFs>(os: &O, path: &Path) -> Result<(O::Directory, OsString), CustodyError> {
    let (absolute, parts, leaf) = destination_parts(path)?;
    let mut current = os.root(absolute).map_err(|e| io_error("open parent", path.as_os_str(), e))?;
    for part in parts {
        let entry = os.metadata_at(&current, &part, false).map_err(|e| io_error("stat parent", &part, e))?;
        let follow = entry.kind == FileKind::Symlink;
        if follow {
            let container = os.directory_metadata(&current).map_err(|e| io_error("stat directory", &part, e))?;
            if entry.uid != 0 || container.uid != 0 || container.mode & 0o022 != 0 {
                return Err(CustodyError::Refused("output destination parent is a symlink"));
            }
        }
        current = os.open_directory(&current, &part, follow).map_err(|e| io_error("open directory", &part, e))?;
    }
    Ok((current, leaf))
}

fn validate_leaf<O: CustodyFs>(os: &O, dir: &O::Directory, leaf: &OsStr) -> Result<(), CustodyError> {
    let entry = match os.metadata_at(dir, leaf, false) {
        Ok(entry) => entry,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_error("stat destination", leaf, e)),
    };
    match entry.kind {
        FileKind::Directory => Err(CustodyError::Refused("output destination is a directory")),
        FileKind::Symlink => match os.metadata_at(dir, leaf, true) {
            Ok(target) if target.kind != FileKind::Regular => Err(CustodyError::Refused("output destination symlink does not target a regular file")),
            Ok(_) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_error("stat destination target", leaf, e)),
        },
        FileKind::Regular => Ok(()),
        FileKind::Other => Err(CustodyError::Refused("output destination is not a regular file")),
    }
}

pub fn hash_file(file: &mut (impl Read + Seek)) -> io::Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 { break; }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

struct Temporary<'a, O: CustodyFs> { os: &'a O, dir: &'a O::Directory, name: Option<OsString> }
impl<O: CustodyFs> Drop for Temporary<'_, O> {
    fn drop(&mut self) {
        if let Some(name) = &self.name { let _ = self.os.unlink(self.dir, name); }
    }
}

pub fn install<O: CustodyFs>(os: &O, source: Source<'_, O::File>, destination: &Path) -> Result<InstalledOutput, CustodyError> {
    let (dir, leaf) = open_parent(os, destination)?;
    validate_leaf(os, &dir, &leaf)?;
    let mut source = match source {
        Source::Path(path) => os.open_source(path).map_err(|e| io_error("open source", path.as_os_str(), e))?,
        Source::Handle(file) => {
            let mut file = os.duplicate(file).map_err(|e| io_error("duplicate source", &leaf, e))?;
            file.seek(SeekFrom::Start(0)).map_err(|e| io_error("seek source", &leaf, e))?;
            file
        }
    };
    if os.file_metadata(&source).map_err(|e| io_error("stat source", &leaf, e))?.kind != FileKind::Regular {
        return Err(CustodyError::Refused("dispatch output source is not a regular file"));
    }
    let mut temporary = Temporary { os, dir: &dir, name: None };
    let mut allocated = None;
    for _ in 0..32 {
        let name = os.temporary_name().map_err(|e| io_error("allocate temporary", &leaf, e))?;
        match os.create_new(&dir, &name) {
            Ok(file) => { temporary.name = Some(name); allocated = Some(file); break; }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_error("create temporary", &name, e)),
        }
    }
    let mut file = allocated.ok_or(CustodyError::Refused("could not allocate an output temporary file"))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let count = source.read(&mut buffer).map_err(|e| io_error("read source", &leaf, e))?;
        if count == 0 { break; }
        hash.update(&buffer[..count]);
        file.write_all(&buffer[..count]).map_err(|e| io_error("write temporary", &leaf, e))?;
    }
    os.sync_file(&file).map_err(|e| io_error("sync temporary", &leaf, e))?;
    drop(file);
    if let Some(name) = &temporary.name {
        os.replace(&dir, name, &leaf).map_err(|e| io_error("replace destination", &leaf, e))?;
    }
    temporary.name = None;
    os.sync_directory(&dir).map_err(|e| io_error("sync parent", &leaf, e))?;
    let mut installed = os.open_read(&dir, &leaf).map_err(|e| io_error("open installed", &leaf, e))?;
    let meta = os.file_metadata(&installed).map_err(|e| io_error("stat installed", &leaf, e))?;
    if meta.kind != FileKind::Regular || meta.links != 1 {
        return Err(CustodyError::Refused("installed output is not a retained regular file"));
    }
    let digest = format!("sha256:{:x}", hash.finalize());
    if hash_file(&mut installed).map_err(|e| io_error("hash installed", &leaf, e))? != digest {
        return Err(CustodyError::Refused("installed output identity changed"));
    }
    Ok(InstalledOutput { digest, device: meta.device, inode: meta.inode })
}

fn retains(meta: Metadata, output: &InstalledOutput) -> bool {
    meta.kind == FileKind::Regular && meta.links == 1 && (meta.device, meta.inode) == (output.device, output.inode)
}

pub fn verify<O: CustodyFs>(os: &O, destination: &Path, output: &InstalledOutput) -> Result<(), CustodyError> {
    {
        let (dir, leaf) = open_parent(os, destination)?;
        let mut file = os.open_read(&dir, &leaf).map_err(|e| io_error("open output", &leaf, e))?;
        let before = os.file_metadata(&file).map_err(|e| io_error("stat output", &leaf, e))?;
        if !retains(before, output) || hash_file(&mut file).map_err(|e| io_error("hash output", &leaf, e))? != output.digest {
            return Err(CustodyError::Refused("output path no longer names the installed identity"));
        }
        let after = os.file_metadata(&file).map_err(|e| io_error("stat output", &leaf, e))?;
        if (after.device, after.inode, after.size, after.modified_ns) != (before.device, before.inode, before.size, before.modified_ns) {
            return Err(CustodyError::Refused("output identity changed while being verified"));
        }
    }
    let (dir, leaf) = open_parent(os, destination)?;
    let file = os.open_read(&dir, &leaf).map_err(|e| io_error("reopen output", &leaf, e))?;
    if !retains(os.file_metadata(&file).map_err(|e| io_error("stat output", &leaf, e))?, output) {
        return Err(CustodyError::Refused("output path no longer names the installed identity"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    #[test]
    fn lexical_parent_refuses_traversal_and_missing_filename() {
        for path in ["", "/", "a/", "a/.", "a/..", "a/../b"] {
            assert!(destination_parts(Path::new(path)).is_err(), "{path}");
        }
        let (absolute, parts, leaf) = destination_parts(Path::new("/a//./b")).unwrap();
        assert!(absolute);
        assert_eq!(parts, vec![OsString::from("a")]);
        assert_eq!(leaf, "b");
    }
    #[test]
    fn digest_starts_at_zero_and_covers_binary_chunks() {
        let mut data = Cursor::new(vec![255; 2 * 1024 * 1024 + 1]);
        data.set_position(900);
        let expected = format!("sha256:{:x}", Sha256::digest(data.get_ref()));
        assert_eq!(hash_file(&mut data).unwrap(), expected);
    }
    #[test]
    fn custody_requires_regular_single_link_and_same_inode() {
        let output = InstalledOutput { digest: String::new(), device: 1, inode: 2 };
        let meta = Metadata { kind: FileKind::Regular, device: 1, inode: 2, links: 1, size: 0, modified_ns: 0, uid: 0, mode: 0o600 };
        assert!(retains(meta, &output));
        for other in [Metadata { links: 2, ..meta }, Metadata { inode: 3, ..meta },
            Metadata { device: 2, ..meta }, Metadata { kind: FileKind::Symlink, ..meta }] {
            assert!(!retains(other, &output));
        }
    }
}
