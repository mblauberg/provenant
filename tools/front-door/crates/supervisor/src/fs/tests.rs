use super::*;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::sync::{Arc, Barrier};

fn names(dir: &Directory) -> Vec<OsString> {
    dir.entries().unwrap().collect::<io::Result<_>>().unwrap()
}

#[test]
fn opens_a_directory_anchor() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap();
    let dir = Directory::open_anchor(&path).unwrap();
    assert_eq!(dir.metadata().unwrap().kind, FileKind::Directory);
    assert_eq!(
        dir.metadata().unwrap(),
        Metadata::from_std(std::fs::metadata(path).unwrap())
    );
}
#[test]
fn refuses_every_non_normal_component_before_effects() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    for name in [
        "",
        ".",
        "..",
        "../escape",
        "/absolute",
        "a/b",
        "a/.",
        "a//",
        "./a",
        "nul\0x",
    ] {
        let name = OsStr::new(name);
        assert!(matches!(
            dir.create_new(name),
            Err(FsError::InvalidComponent { .. })
        ));
        assert!(matches!(
            dir.mkdir(name, 0o700),
            Err(FsError::InvalidComponent { .. })
        ));
        assert!(matches!(
            dir.unlink(name),
            Err(FsError::InvalidComponent { .. })
        ));
        assert!(matches!(
            dir.replace(OsStr::new("source"), &dir, name),
            Err(FsError::InvalidComponent { .. })
        ));
    }
    assert!(names(&dir).is_empty());
}
#[test]
fn creates_reads_syncs_duplicates_and_preserves_non_utf8_names() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    let name = OsString::from("raw-λ");
    let mut file = dir.create_new(&name).unwrap();
    file.write_all(b"abcdef").unwrap();
    file.sync().unwrap();
    dir.sync().unwrap();
    assert_eq!(file.metadata().unwrap().mode & 0o777, 0o600);
    assert_eq!(
        dir.metadata_at(&name, false).unwrap(),
        file.metadata().unwrap()
    );
    assert_eq!(dir.read_bounded(&name, 6).unwrap(), b"abcdef");
    assert!(matches!(
        dir.read_bounded(&name, 5),
        Err(FsError::TooLarge { limit: 5 })
    ));
    let mut read = dir.open_read(&name).unwrap();
    let mut dup = read.duplicate().unwrap();
    let mut byte = [0];
    read.read_exact(&mut byte).unwrap();
    assert_eq!(byte, [b'a']);
    dup.read_exact(&mut byte).unwrap();
    assert_eq!(byte, [b'b']);
    assert_eq!(names(&dir), std::slice::from_ref(&name));
    assert_eq!(names(&dir), std::slice::from_ref(&name));
    assert!(dir.create_new(&name).is_err());
    dir.unlink(&name).unwrap();
    assert_eq!(read.read_bounded(10).unwrap(), b"cdef");
}
#[test]
fn anchor_survives_ancestor_symlink_swap_without_writing_outside() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("anchor")).unwrap();
    std::fs::create_dir(root.join("outside")).unwrap();
    let dir = Directory::open_anchor(&root.join("anchor")).unwrap();
    std::fs::rename(root.join("anchor"), root.join("held")).unwrap();
    symlink(root.join("outside"), root.join("anchor")).unwrap();
    dir.create_new(OsStr::new("owned")).unwrap();
    assert!(root.join("held/owned").is_file());
    assert!(!root.join("outside/owned").exists());
    assert!(Directory::open_anchor(&root.join("anchor")).is_err());
}
#[test]
fn final_symlinks_are_refused_and_unlinked_without_following() {
    let temp = tempfile::tempdir().unwrap();
    let root = Directory::open_anchor(temp.path()).unwrap();
    root.mkdir(OsStr::new("directory"), 0o700).unwrap();
    root.create_new(OsStr::new("target")).unwrap();
    root.symlink(Path::new("target"), OsStr::new("file-link"))
        .unwrap();
    root.symlink(Path::new("directory"), OsStr::new("dir-link"))
        .unwrap();
    assert_eq!(
        root.metadata_at(OsStr::new("file-link"), false)
            .unwrap()
            .kind,
        FileKind::Symlink
    );
    assert_eq!(
        root.metadata_at(OsStr::new("file-link"), true)
            .unwrap()
            .kind,
        FileKind::Regular
    );
    assert!(root.open_read(OsStr::new("file-link")).is_err());
    assert!(root
        .open_verified_directory(OsStr::new("dir-link"))
        .is_err());
    assert!(root.create_new(OsStr::new("file-link")).is_err());
    root.unlink(OsStr::new("file-link")).unwrap();
    assert!(root.open_read(OsStr::new("target")).is_ok());
    assert!(root.rmdir(OsStr::new("dir-link")).is_err());
    root.unlink(OsStr::new("dir-link")).unwrap();
    root.rmdir(OsStr::new("directory")).unwrap();
}
#[test]
fn competing_noreplace_publishers_have_exactly_one_winner() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Arc::new(Directory::open_anchor(temp.path()).unwrap());
    let barrier = Arc::new(Barrier::new(3));
    let mut writers = Vec::new();
    for name in ["first", "second"] {
        dir.create_new(OsStr::new(name))
            .unwrap()
            .write_all(name.as_bytes())
            .unwrap();
        let (dir, barrier) = (dir.clone(), barrier.clone());
        writers.push(std::thread::spawn(move || {
            barrier.wait();
            dir.rename_noreplace(OsStr::new(name), &dir, OsStr::new("published"))
        }));
    }
    barrier.wait();
    let outcomes: Vec<_> = writers.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(outcomes.iter().filter(|o| o.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| o
                .as_ref()
                .is_err_and(|e| e.raw_os_error() == Some(libc::EEXIST)))
            .count(),
        1
    );
    let content = dir.read_bounded(OsStr::new("published"), 10).unwrap();
    assert!(content == b"first" || content == b"second");
    assert_eq!(names(&dir).len(), 2);
}
#[test]
fn exchange_replace_and_cross_directory_operations_are_atomic_primitives() {
    let temp = tempfile::tempdir().unwrap();
    let root = Directory::open_anchor(temp.path()).unwrap();
    root.mkdir(OsStr::new("child"), 0o500).unwrap();
    let child = root.open_verified_directory(OsStr::new("child")).unwrap();
    child.make_writable().unwrap();
    assert_eq!(child.metadata().unwrap().mode & 0o700, 0o700);
    root.create_new(OsStr::new("a"))
        .unwrap()
        .write_all(b"A")
        .unwrap();
    child
        .create_new(OsStr::new("b"))
        .unwrap()
        .write_all(b"B")
        .unwrap();
    root.exchange(OsStr::new("a"), &child, OsStr::new("b"))
        .unwrap();
    assert_eq!(root.read_bounded(OsStr::new("a"), 1).unwrap(), b"B");
    assert_eq!(child.read_bounded(OsStr::new("b"), 1).unwrap(), b"A");
    root.replace(OsStr::new("a"), &child, OsStr::new("b"))
        .unwrap();
    assert_eq!(child.read_bounded(OsStr::new("b"), 1).unwrap(), b"B");
    assert!(root.open_read(OsStr::new("a")).is_err());
    child.unlink(OsStr::new("b")).unwrap();
    root.rmdir(OsStr::new("child")).unwrap();
}
#[test]
fn empty_bounds_and_adversarial_file_types_do_not_block() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    dir.create_new(OsStr::new("empty")).unwrap();
    assert_eq!(dir.read_bounded(OsStr::new("empty"), 0).unwrap(), b"");
    dir.create_new(OsStr::new("data"))
        .unwrap()
        .write_all(b"x")
        .unwrap();
    assert!(matches!(
        dir.read_bounded(OsStr::new("data"), 0),
        Err(FsError::TooLarge { .. })
    ));
    dir.mkdir(OsStr::new("directory"), 0o700).unwrap();
    assert!(matches!(
        dir.open_read(OsStr::new("directory")),
        Err(FsError::NotRegular { .. })
    ));
    // mkfifo has no shell dependencies and the open must never wait for a writer.
    let status = std::process::Command::new("/usr/bin/mkfifo")
        .arg(temp.path().join("fifo"))
        .status()
        .unwrap();
    assert!(status.success());
    assert!(matches!(
        dir.open_read(OsStr::new("fifo")),
        Err(FsError::NotRegular { .. })
    ));
    let name = temporary_name().unwrap();
    assert_eq!(name.as_bytes().len(), b".cf-dispatch-output.".len() + 24);
    assert_ne!(name, temporary_name().unwrap());
    std::fs::set_permissions(
        temp.path().join("empty"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
}

#[test]
fn non_utf8_names_reach_the_os_without_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    let name = OsString::from_vec(b"raw-\xff".to_vec());
    let baseline = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp.path().join(&name));
    match baseline {
        Ok(file) => {
            drop(file);
            assert_eq!(names(&dir), std::slice::from_ref(&name));
            dir.open_read(&name).unwrap();
            dir.unlink(&name).unwrap();
            dir.create_new(&name).unwrap();
            assert_eq!(names(&dir), [name]);
        }
        Err(e) => {
            // macOS sandbox/filesystem may reject invalid UTF-8 path bytes.
            let actual = dir.create_new(&name).unwrap_err();
            assert_eq!(actual.raw_os_error(), e.raw_os_error());
            assert!(names(&dir).is_empty());
        }
    }
}
#[test]
fn independent_open_file_descriptions_contend_for_a_nonblocking_lock() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    let first = dir.create_new(OsStr::new("lock")).unwrap();
    let second = dir.open_read(OsStr::new("lock")).unwrap();
    assert!(first.try_lock().unwrap());
    assert!(!second.try_lock().unwrap());
    first.unlock().unwrap();
    assert!(second.try_lock().unwrap());
}

#[test]
fn enumeration_owns_its_stream_and_is_independent_of_anchor_lifetime() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    for name in ["first", "second"] {
        dir.create_new(OsStr::new(name)).unwrap();
    }
    let mut entries = dir.entries().unwrap();
    // Consume one entry, then independently enumerate both through a fresh stream.
    let first = entries.next().unwrap().unwrap();
    assert_eq!(names(&dir).len(), 2);
    drop(dir);
    let second = entries.next().unwrap().unwrap();
    assert_ne!(first, second);
    assert!(entries.next().is_none());
    assert!(entries.next().is_none());
}

#[test]
fn trailing_separators_do_not_bypass_anchor_final_symlink_refusal() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap();
    std::fs::create_dir(path.join("target")).unwrap();
    symlink("target", path.join("link")).unwrap();
    let mut link = path.join("link").into_os_string();
    link.push("///");
    assert!(Directory::open_anchor(Path::new(&link)).is_err());
    let mut target = path.join("target").into_os_string();
    target.push("///");
    assert!(Directory::open_anchor(Path::new(&target)).is_ok());
    assert!(Directory::root(true).is_ok());
}

#[test]
fn identity_observations_refuse_replacement_on_either_side_of_open() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    dir.mkdir(OsStr::new("before"), 0o700).unwrap();
    dir.mkdir(OsStr::new("replacement"), 0o700).unwrap();
    let before = dir.metadata_at(OsStr::new("before"), false).unwrap();
    let replacement = dir.metadata_at(OsStr::new("replacement"), false).unwrap();
    let name = OsStr::new("entry");
    assert!(verify_directory_identity(name, before, before, before, before.device).is_ok());
    // Explicit observations pin races without scheduler timing or private hooks.
    for (opened, after) in [
        (replacement, replacement),
        (before, replacement),
        (replacement, before),
    ] {
        assert!(matches!(
            verify_directory_identity(name, before, opened, after, before.device),
            Err(FsError::RaceRefused { .. })
        ));
    }
    assert!(matches!(
        verify_directory_identity(name, before, before, before, before.device.wrapping_add(1)),
        Err(FsError::RaceRefused { .. })
    ));
}
#[test]
fn verified_open_refuses_the_platform_device_mount() {
    let root = Directory::root(true).unwrap();
    let parent = root.metadata().unwrap();
    let device = root.metadata_at(OsStr::new("dev"), false).unwrap();
    // /dev is an independent device mount on both supported CI/host platforms.
    assert_ne!(parent.device, device.device);
    assert!(matches!(
        root.open_verified_directory(OsStr::new("dev")),
        Err(FsError::RaceRefused { .. })
    ));
}
