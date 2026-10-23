//! In-memory safe OS model; real descriptor/race conformance remains supervisor work.
use super::*;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::Cursor;
use std::rc::Rc;

#[derive(Clone)]
struct MemoryFile(Rc<RefCell<Node>>);
struct Node {
    metadata: Metadata,
    bytes: Cursor<Vec<u8>>,
    target: Option<String>,
}
impl Read for MemoryFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.borrow_mut().bytes.read(buf)
    }
}
impl Write for MemoryFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().bytes.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Seek for MemoryFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.0.borrow_mut().bytes.seek(pos)
    }
}
struct MemoryFs {
    nodes: RefCell<BTreeMap<String, MemoryFile>>,
    next: Cell<u64>,
    fail_sync: Cell<bool>,
}
impl MemoryFs {
    fn new() -> Self {
        let os = Self {
            nodes: RefCell::new(BTreeMap::new()),
            next: Cell::new(1),
            fail_sync: Cell::new(false),
        };
        os.add("", FileKind::Directory, b"", None);
        os.add("/", FileKind::Directory, b"", None);
        os.add("out", FileKind::Directory, b"", None);
        os.add("source", FileKind::Regular, b"hello\0world\xff", None);
        os
    }
    fn add(&self, path: &str, kind: FileKind, bytes: &[u8], target: Option<&str>) {
        let inode = self.next.get();
        self.next.set(inode + 1);
        let node = Node {
            metadata: Metadata {
                kind,
                inode,
                device: 1,
                size: bytes.len() as u64,
                links: 1,
                modified_ns: 1,
                uid: 1000,
                mode: 0o700,
            },
            bytes: Cursor::new(bytes.to_vec()),
            target: target.map(str::to_owned),
        };
        self.nodes
            .borrow_mut()
            .insert(path.into(), MemoryFile(Rc::new(RefCell::new(node))));
    }
    fn get(&self, path: &str) -> io::Result<MemoryFile> {
        self.nodes
            .borrow()
            .get(path)
            .cloned()
            .ok_or(io::ErrorKind::NotFound.into())
    }
    fn path(dir: &str, name: &OsStr) -> String {
        if dir.is_empty() {
            name.to_str().unwrap().into()
        } else if dir == "/" {
            format!("/{name}", name = name.to_str().unwrap())
        } else {
            format!("{dir}/{}", name.to_str().unwrap())
        }
    }
    fn bytes(&self, path: &str) -> Vec<u8> {
        self.get(path).unwrap().0.borrow().bytes.get_ref().clone()
    }
    fn metadata(&self, path: &str, follow: bool) -> io::Result<Metadata> {
        let file = self.get(path)?;
        let node = file.0.borrow();
        if follow {
            if let Some(target) = &node.target {
                return self.metadata(target, true);
            }
        }
        Ok(Metadata {
            size: node.bytes.get_ref().len() as u64,
            ..node.metadata
        })
    }
}
impl CustodyFs for MemoryFs {
    type Directory = String;
    type File = MemoryFile;
    fn root(&self, absolute: bool) -> io::Result<String> {
        Ok(if absolute { "/" } else { "" }.into())
    }
    fn directory_metadata(&self, dir: &String) -> io::Result<Metadata> {
        self.metadata(dir, false)
    }
    fn metadata_at(&self, dir: &String, name: &OsStr, follow: bool) -> io::Result<Metadata> {
        self.metadata(&Self::path(dir, name), follow)
    }
    fn open_directory(&self, dir: &String, name: &OsStr, follow: bool) -> io::Result<String> {
        let path = Self::path(dir, name);
        let meta = self.metadata(&path, follow)?;
        if meta.kind != FileKind::Directory {
            return Err(io::ErrorKind::NotADirectory.into());
        }
        if follow {
            Ok(self.get(&path)?.0.borrow().target.clone().unwrap_or(path))
        } else {
            Ok(path)
        }
    }
    fn open_read(&self, dir: &String, name: &OsStr) -> io::Result<MemoryFile> {
        self.open_source(Path::new(&Self::path(dir, name)))
    }
    fn open_source(&self, path: &Path) -> io::Result<MemoryFile> {
        let file = self.get(path.to_str().unwrap())?;
        if file.0.borrow().metadata.kind == FileKind::Symlink {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        file.0.borrow_mut().bytes.set_position(0);
        Ok(file)
    }
    fn duplicate(&self, file: &MemoryFile) -> io::Result<MemoryFile> {
        Ok(file.clone())
    }
    fn file_metadata(&self, file: &MemoryFile) -> io::Result<Metadata> {
        let node = file.0.borrow();
        Ok(Metadata {
            size: node.bytes.get_ref().len() as u64,
            ..node.metadata
        })
    }
    fn temporary_name(&self) -> io::Result<OsString> {
        Ok(format!(".cf-dispatch-output.{:024x}", self.next.get()).into())
    }
    fn create_new(&self, dir: &String, name: &OsStr) -> io::Result<MemoryFile> {
        let path = Self::path(dir, name);
        if self.nodes.borrow().contains_key(&path) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        self.add(&path, FileKind::Regular, b"", None);
        self.get(&path)
    }
    fn sync_file(&self, _: &MemoryFile) -> io::Result<()> {
        if self.fail_sync.get() {
            Err(io::ErrorKind::StorageFull.into())
        } else {
            Ok(())
        }
    }
    fn sync_directory(&self, _: &String) -> io::Result<()> {
        Ok(())
    }
    fn replace(&self, dir: &String, source: &OsStr, destination: &OsStr) -> io::Result<()> {
        let mut nodes = self.nodes.borrow_mut();
        let source = nodes
            .remove(&Self::path(dir, source))
            .ok_or(io::ErrorKind::NotFound)?;
        nodes.insert(Self::path(dir, destination), source);
        Ok(())
    }
    fn unlink(&self, dir: &String, name: &OsStr) -> io::Result<()> {
        self.nodes.borrow_mut().remove(&Self::path(dir, name));
        Ok(())
    }
}

#[test]
fn replaces_symlink_without_touching_target_and_certifies_identity() {
    for target in ["target", "missing"] {
        let os = MemoryFs::new();
        os.add("target", FileKind::Regular, b"keep", None);
        os.add("out/result", FileKind::Symlink, b"", Some(target));
        let output = install(
            &os,
            Source::Path(Path::new("source")),
            Path::new("out/result"),
        )
        .unwrap();
        assert_eq!(os.bytes("target"), b"keep");
        assert_eq!(os.bytes("out/result"), os.bytes("source"));
        verify(&os, Path::new("out/result"), &output).unwrap();
    }
}

#[test]
fn refuses_destination_directory_special_file_and_link_to_directory() {
    for (kind, target) in [
        (FileKind::Directory, None),
        (FileKind::Other, None),
        (FileKind::Symlink, Some("out")),
    ] {
        let os = MemoryFs::new();
        os.add("out/result", kind, b"", target);
        assert!(install(
            &os,
            Source::Path(Path::new("source")),
            Path::new("out/result")
        )
        .is_err());
    }
}

#[test]
fn refuses_untrusted_parent_links_but_accepts_root_owned_system_alias() {
    let os = MemoryFs::new();
    os.add("alias", FileKind::Symlink, b"", Some("out"));
    assert!(install(
        &os,
        Source::Path(Path::new("source")),
        Path::new("alias/result")
    )
    .is_err());
    os.add("/alias", FileKind::Symlink, b"", Some("out"));
    os.get("/alias").unwrap().0.borrow_mut().metadata.uid = 0;
    os.get("/").unwrap().0.borrow_mut().metadata.uid = 0;
    install(
        &os,
        Source::Path(Path::new("source")),
        Path::new("/alias/result"),
    )
    .unwrap();
    os.get("/").unwrap().0.borrow_mut().metadata.mode = 0o777;
    assert!(install(
        &os,
        Source::Path(Path::new("source")),
        Path::new("/alias/result")
    )
    .is_err());
}

#[test]
fn source_handle_is_read_from_start_and_special_sources_are_refused() {
    let os = MemoryFs::new();
    let mut file = os.open_source(Path::new("source")).unwrap();
    file.seek(SeekFrom::End(0)).unwrap();
    install(&os, Source::Handle(&file), Path::new("out/result")).unwrap();
    assert_eq!(os.bytes("out/result"), os.bytes("source"));
    for kind in [FileKind::Directory, FileKind::Other, FileKind::Symlink] {
        os.add("source", kind, b"", None);
        assert!(install(
            &os,
            Source::Path(Path::new("source")),
            Path::new("out/result")
        )
        .is_err());
    }
}

#[test]
fn verification_refuses_equal_bytes_at_new_inode_hardlink_and_changed_bytes() {
    for mutation in ["inode", "hardlink", "bytes", "symlink"] {
        let os = MemoryFs::new();
        let path = Path::new("out/result");
        let output = install(&os, Source::Path(Path::new("source")), path).unwrap();
        match mutation {
            "inode" => os.add("out/result", FileKind::Regular, &os.bytes("source"), None),
            "hardlink" => os.get("out/result").unwrap().0.borrow_mut().metadata.links = 2,
            "bytes" => os.get("out/result").unwrap().0.borrow_mut().bytes.get_mut()[0] = b'x',
            _ => os.add("out/result", FileKind::Symlink, b"", Some("source")),
        }
        assert!(verify(&os, path, &output).is_err(), "{mutation}");
    }
}

#[test]
fn failed_sync_leaves_old_output_and_removes_temporary() {
    let os = MemoryFs::new();
    os.add("out/result", FileKind::Regular, b"old", None);
    os.fail_sync.set(true);
    assert!(install(
        &os,
        Source::Path(Path::new("source")),
        Path::new("out/result")
    )
    .is_err());
    assert_eq!(os.bytes("out/result"), b"old");
    assert!(!os
        .nodes
        .borrow()
        .keys()
        .any(|key| key.contains(".cf-dispatch-output")));
}

#[test]
fn publication_bytes_digest_and_tamper_refusal_match_python() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let destination = tmp.path().join("result");
    let target = tmp.path().join("target");
    std::fs::write(&source, b"hello\0world\xff").unwrap();
    std::fs::write(&target, b"keep").unwrap();
    std::os::unix::fs::symlink(&target, &destination).unwrap();
    let Some(actual) = crate::python::oracle(
        r#"
import output_custody
from pathlib import Path
digest, dev, ino=output_custody.install(fixture['source'], fixture['destination'])
output_custody.verify(fixture['destination'], digest, dev, ino)
data=Path(fixture['destination']).read_bytes()
Path(fixture['destination']).write_bytes(b'changed')
try:
    output_custody.verify(fixture['destination'], digest, dev, ino)
    refused=False
except output_custody.CustodyError:
    refused=True
print(json.dumps({'digest': digest, 'bytes': list(data), 'target': list(Path(fixture['target']).read_bytes()), 'refused': refused}))
"#,
        &serde_json::json!({"source": source, "destination": destination, "target": target}),
    ) else {
        return;
    };
    let os = MemoryFs::new();
    os.add("target", FileKind::Regular, b"keep", None);
    os.add("out/result", FileKind::Symlink, b"", Some("target"));
    let output = install(
        &os,
        Source::Path(Path::new("source")),
        Path::new("out/result"),
    )
    .unwrap();
    verify(&os, Path::new("out/result"), &output).unwrap();
    let bytes = os.bytes("out/result");
    os.get("out/result").unwrap().0.borrow_mut().bytes = Cursor::new(b"changed".to_vec());
    assert_eq!(
        actual,
        serde_json::json!({"digest": output.digest, "bytes": bytes, "target": os.bytes("target"),
        "refused": verify(&os, Path::new("out/result"), &output).is_err()})
    );
}
