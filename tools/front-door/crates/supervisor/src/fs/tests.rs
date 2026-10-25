use super::*;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{PermissionsExt, symlink};
#[cfg(feature = "fixture")]
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Barrier};
#[cfg(feature = "fixture")]
use std::time::{Duration, Instant};

fn names(dir: &Directory) -> Vec<OsString> {
    dir.entries().unwrap().collect::<Result<_>>().unwrap()
}

#[test]
fn entry_snapshot_refuses_over_budget_without_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    assert!(entry_names(&dir, 0).unwrap().is_empty());
    for name in ["a", "b", "c"] {
        dir.create_new(OsStr::new(name)).unwrap();
    }
    assert!(matches!(
        entry_names(&dir, 2),
        Err(FsError::EntryLimit { limit: 2 })
    ));
    assert_eq!(entry_names(&dir, 3).unwrap().len(), 3);
}

#[test]
fn payload_disposal_allows_lock_names_without_following_links() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    dir.create_new(OsStr::new("Cargo.lock")).unwrap();
    assert!(matches!(
        dir.unlink(OsStr::new("Cargo.lock")),
        Err(FsError::ReservedLockName { .. })
    ));
    dir.unlink_payload(OsStr::new("Cargo.lock")).unwrap();
    dir.mkdir(OsStr::new("directory.lock"), 0o700).unwrap();
    dir.rmdir_payload(OsStr::new("directory.lock")).unwrap();
    dir.create_new(OsStr::new("keep")).unwrap();
    symlink("keep", temp.path().join("link.lock")).unwrap();
    dir.unlink_payload(OsStr::new("link.lock")).unwrap();
    assert!(dir.metadata_at(OsStr::new("keep"), false).is_ok());
    for name in ["..", "a/b", "/absolute"] {
        assert!(matches!(
            dir.unlink_payload(OsStr::new(name)),
            Err(FsError::InvalidComponent { .. })
        ));
        assert!(matches!(
            dir.rmdir_payload(OsStr::new(name)),
            Err(FsError::InvalidComponent { .. })
        ));
    }
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
    assert!(dir.create_new(&name).unwrap_err().is_exists());
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
    let error = Directory::open_anchor(&root.join("anchor")).unwrap_err();
    assert!(matches!(error, FsError::Symlink { .. }));
    assert_eq!(error.raw_os_error(), Some(errno::ELOOP));
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
    for error in [
        root.open_read(OsStr::new("file-link")).unwrap_err(),
        root.open_verified_directory(OsStr::new("dir-link"))
            .unwrap_err(),
    ] {
        assert!(matches!(error, FsError::Symlink { .. }));
        assert_eq!(error.raw_os_error(), Some(errno::ELOOP));
    }
    let error = root.create_new(OsStr::new("file-link")).unwrap_err();
    assert!(matches!(error, FsError::Io { .. }));
    assert_eq!(error.raw_os_error(), Some(errno::EEXIST));
    root.unlink(OsStr::new("file-link")).unwrap();
    assert!(root.open_read(OsStr::new("target")).is_ok());
    let error = root.rmdir(OsStr::new("dir-link")).unwrap_err();
    assert!(error.is_not_directory());
    assert_eq!(error.raw_os_error(), Some(errno::ENOTDIR));
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
                .is_err_and(|e| e.raw_os_error() == Some(crate::sys::errno::EEXIST)))
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
    assert!(root.open_read(OsStr::new("a")).unwrap_err().is_not_found());
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
    os::tests::mkfifo(&temp.path().join("fifo")).unwrap();
    assert!(matches!(
        dir.open_read(OsStr::new("fifo")),
        Err(FsError::NotRegular { .. })
    ));
    let name = temporary_name().unwrap();
    assert_eq!(name.as_bytes().len(), b".provenant-staging.".len() + 24);
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
#[derive(Clone, Copy)]
struct FixedClock(provenant_core::MonoTime);
impl Clock for FixedClock {
    fn wall_now(&self) -> std::result::Result<provenant_core::WallTime, provenant_core::TimeError> {
        Ok(provenant_core::UtcMicros::from_micros(0).unwrap())
    }
    fn mono_now(&self) -> provenant_core::MonoTime {
        self.0
    }
}

fn one_second_budget() -> WaitBudget {
    WaitBudget::cli(provenant_core::MonoTime::from_micros(0), 1).unwrap()
}

#[test]
fn owner_lock_is_nonblocking_bounded_and_releases_only_when_its_guard_drops() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    let clock = FixedClock(provenant_core::MonoTime::from_micros(0));
    let cancel = CancelToken::new();
    let first = dir
        .lock(
            OsStr::new("owner.lock"),
            LockMode::Shared,
            one_second_budget(),
            &cancel,
            &clock,
        )
        .unwrap();
    let second = dir
        .lock(
            OsStr::new("owner.lock"),
            LockMode::Shared,
            one_second_budget(),
            &cancel,
            &clock,
        )
        .unwrap();
    assert!(matches!(
        dir.lock(
            OsStr::new("owner.lock"),
            LockMode::Exclusive,
            one_second_budget(),
            &cancel,
            &FixedClock(provenant_core::MonoTime::from_micros(1_000_000)),
        ),
        Err(FsError::LockTimeout { timeout, .. }) if timeout.is_zero()
    ));
    drop(first);
    drop(second);
    assert!(
        dir.lock(
            OsStr::new("owner.lock"),
            LockMode::Exclusive,
            one_second_budget(),
            &cancel,
            &clock
        )
        .is_ok()
    );
}

#[test]
fn object_safe_lock_preserves_typed_timeout_and_cancellation_errors() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    let object: &dyn SafeDir = &dir;
    let missing = match object.open_read(OsStr::new("missing")) {
        Err(error) => error,
        Ok(_) => panic!("opening a missing entry unexpectedly succeeded"),
    };
    assert!(matches!(&missing, FsError::Io { .. }));
    assert!(missing.is_not_found());
    let clock = FixedClock(provenant_core::MonoTime::from_micros(0));
    let cancelled = CancelToken::new();
    cancelled.cancel();
    assert!(matches!(
        object.lock(
            OsStr::new("typed.lock"),
            LockMode::Exclusive,
            one_second_budget(),
            &cancelled,
            &clock,
        ),
        Err(FsError::Cancelled { .. })
    ));

    let live = CancelToken::new();
    let held = object
        .lock(
            OsStr::new("typed.lock"),
            LockMode::Shared,
            one_second_budget(),
            &live,
            &clock,
        )
        .unwrap();
    assert!(matches!(
        object.lock(
            OsStr::new("typed.lock"),
            LockMode::Exclusive,
            one_second_budget(),
            &live,
            &FixedClock(provenant_core::MonoTime::from_micros(1_000_000)),
        ),
        Err(FsError::LockTimeout { .. })
    ));
    drop(held);
}

#[test]
fn owner_lock_refuses_cancelled_requests_hardlinks_and_writable_anchors() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    let cancel = CancelToken::new();
    cancel.cancel();
    let clock = FixedClock(provenant_core::MonoTime::from_micros(0));
    assert!(matches!(
        dir.lock(
            OsStr::new("cancelled.lock"),
            LockMode::Exclusive,
            one_second_budget(),
            &cancel,
            &clock
        ),
        Err(FsError::Cancelled { .. })
    ));
    assert!(!temp.path().join("cancelled.lock").exists());

    let lock_path = temp.path().join("linked.lock");
    drop(dir.create_new(OsStr::new("linked.lock")).unwrap());
    std::fs::hard_link(&lock_path, temp.path().join("alias")).unwrap();
    assert!(matches!(
        dir.lock(
            OsStr::new("linked.lock"),
            LockMode::Exclusive,
            one_second_budget(),
            &CancelToken::new(),
            &clock
        ),
        Err(FsError::LockRefused {
            reason: LockRefusal::LinkCount,
            ..
        })
    ));

    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    let unsafe_dir = Directory::open_anchor(temp.path()).unwrap();
    assert!(matches!(
        unsafe_dir.lock(
            OsStr::new("unsafe.lock"),
            LockMode::Exclusive,
            one_second_budget(),
            &CancelToken::new(),
            &clock
        ),
        Err(FsError::LockRefused {
            reason: LockRefusal::AnchorPermissions,
            ..
        })
    ));
}

#[test]
fn owner_lock_refuses_symlinked_lock_entries() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    dir.create_new(OsStr::new("target")).unwrap();
    symlink("target", temp.path().join("owner.lock")).unwrap();
    assert!(matches!(
        dir.lock(
            OsStr::new("owner.lock"),
            LockMode::Exclusive,
            one_second_budget(),
            &CancelToken::new(),
            &FixedClock(provenant_core::MonoTime::from_micros(0)),
        ),
        Err(FsError::Symlink { .. })
    ));
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
    let error = Directory::open_anchor(Path::new(&link)).unwrap_err();
    assert!(matches!(error, FsError::Symlink { .. }));
    assert_eq!(error.raw_os_error(), Some(errno::ELOOP));
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
        Err(FsError::CrossDevice { .. })
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
        Err(FsError::CrossDevice { .. })
    ));
}

#[test]
fn directory_symlinks_have_a_portable_typed_errno_and_nested_context() {
    let temp = tempfile::tempdir().unwrap();
    let root = Directory::open_anchor(temp.path()).unwrap();
    root.mkdir(OsStr::new("child"), 0o700).unwrap();
    let child = root.open_directory(OsStr::new("child")).unwrap();
    child.symlink(Path::new("."), OsStr::new("link")).unwrap();
    let error = child.open_directory(OsStr::new("link")).unwrap_err();
    assert!(matches!(&error, FsError::Symlink { path, .. } if path == Path::new("child/link")));
    assert!(error.is_symlink());
    assert_eq!(error.raw_os_error(), Some(crate::sys::errno::ELOOP));
    let converted: io::Error = error.into();
    assert_eq!(converted.raw_os_error(), Some(crate::sys::errno::ELOOP));
    assert_eq!(
        converted.kind(),
        io::Error::from_raw_os_error(crate::sys::errno::ELOOP).kind()
    );
    let missing = child.open_read(OsStr::new("missing")).unwrap_err();
    assert!(missing.is_not_found());
    assert!(matches!(&missing, FsError::Io { path, .. } if path == Path::new("child/missing")));
}

#[test]
fn atomic_publication_replaces_or_refuses_and_always_removes_staging_entries() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    let name = OsStr::new("published");
    assert!(matches!(
        dir.atomic_write(name, ".config.", b"old", PublishMode::NoReplace)
            .unwrap(),
        Publication::Durable
    ));
    let error = dir
        .atomic_write(name, ".config.", b"refused", PublishMode::NoReplace)
        .unwrap_err();
    assert!(error.is_exists());
    assert_eq!(dir.read_bounded(name, 10).unwrap(), b"old");
    assert!(matches!(
        dir.atomic_write(name, ".core.", b"new", PublishMode::Replace)
            .unwrap(),
        Publication::Durable
    ));
    assert_eq!(dir.read_bounded(name, 10).unwrap(), b"new");
    assert_eq!(names(&dir), [name]);
    dir.mkdir(OsStr::new("directory"), 0o700).unwrap();
    let error = dir
        .atomic_write(
            OsStr::new("directory"),
            ".config.",
            b"data",
            PublishMode::Replace,
        )
        .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(crate::sys::errno::EISDIR));
    assert_eq!(names(&dir).len(), 2);
    dir.replace_link(name, Path::new("one")).unwrap();
    dir.replace_link(name, Path::new("two")).unwrap();
    assert_eq!(
        std::fs::read_link(temp.path().join(name)).unwrap(),
        Path::new("two")
    );
    assert_eq!(names(&dir).len(), 2);
    assert!(matches!(
        dir.atomic_write(name, "../invalid", b"data", PublishMode::Replace),
        Err(FsError::InvalidComponent { .. })
    ));
}

#[test]
fn both_publication_modes_report_visible_directory_sync_failure_without_retrying() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    let mut sync_failure = FailAtomicWriteAt {
        phase: AtomicWritePhase::DirectorySync,
        errno: libc::EIO,
    };
    let first = dir
        .atomic_write_with_operations(
            OsStr::new("created"),
            ".stage.",
            b"visible",
            PublishMode::NoReplace,
            &mut sync_failure,
        )
        .unwrap();
    assert!(matches!(
        first,
        Publication::VisibleDurabilityUnconfirmed { .. }
    ));
    assert_eq!(
        dir.read_bounded(OsStr::new("created"), 7).unwrap(),
        b"visible"
    );
    assert_eq!(names(&dir), [OsString::from("created")]);

    dir.atomic_write(
        OsStr::new("replaced"),
        ".stage.",
        b"old",
        PublishMode::Replace,
    )
    .unwrap();
    let mut sync_failure = FailAtomicWriteAt {
        phase: AtomicWritePhase::DirectorySync,
        errno: libc::EIO,
    };
    let second = dir
        .atomic_write_with_operations(
            OsStr::new("replaced"),
            ".stage.",
            b"new",
            PublishMode::Replace,
            &mut sync_failure,
        )
        .unwrap();
    assert!(matches!(
        second,
        Publication::VisibleDurabilityUnconfirmed { .. }
    ));
    assert_eq!(dir.read_bounded(OsStr::new("replaced"), 3).unwrap(), b"new");
    assert_eq!(names(&dir).len(), 2);
}

struct FailAtomicWriteAt {
    phase: AtomicWritePhase,
    errno: i32,
}
impl AtomicWriteOperations for FailAtomicWriteAt {
    fn write(&mut self, file: &mut FileHandle, bytes: &[u8], path: &Path) -> Result<()> {
        if self.phase == AtomicWritePhase::Write {
            let partial = bytes.len() / 2;
            file.write_all(&bytes[..partial])
                .map_err(error("write staging file", path))?;
            Err(error("write staging file", path)(
                io::Error::from_raw_os_error(self.errno),
            ))
        } else {
            LocalAtomicWriteOperations.write(file, bytes, path)
        }
    }
    fn sync_file(&mut self, file: &FileHandle) -> Result<()> {
        if self.phase == AtomicWritePhase::FileSync {
            Err(error("fsync file", &file.path)(
                io::Error::from_raw_os_error(self.errno),
            ))
        } else {
            LocalAtomicWriteOperations.sync_file(file)
        }
    }
    fn rename(
        &mut self,
        directory: &Directory,
        temporary: &OsStr,
        name: &OsStr,
        mode: PublishMode,
    ) -> Result<()> {
        if self.phase == AtomicWritePhase::Rename {
            Err(FsError::Rename {
                from_path: directory.path.join(temporary),
                to_path: directory.path.join(name),
                source: io::Error::from_raw_os_error(self.errno),
            })
        } else {
            LocalAtomicWriteOperations.rename(directory, temporary, name, mode)
        }
    }
    fn sync_directory(&mut self, directory: &Directory) -> Result<()> {
        if self.phase == AtomicWritePhase::DirectorySync {
            Err(error("fsync directory", &directory.path)(
                io::Error::from_raw_os_error(self.errno),
            ))
        } else {
            LocalAtomicWriteOperations.sync_directory(directory)
        }
    }
}

#[test]
fn write_fsync_and_rename_faults_leave_the_previous_complete_file() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    let name = OsStr::new("receipt.json");
    let old = b"old complete receipt";
    let new = b"new complete receipt";
    dir.atomic_write(name, ".receipt.", old, PublishMode::Replace)
        .unwrap();

    for (phase, errno) in [
        (AtomicWritePhase::Write, libc::ENOSPC),
        (AtomicWritePhase::FileSync, libc::EIO),
        (AtomicWritePhase::Rename, libc::EIO),
    ] {
        let mut faults = FailAtomicWriteAt { phase, errno };
        let error = dir
            .atomic_write_with_operations(name, ".receipt.", new, PublishMode::Replace, &mut faults)
            .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(errno));
        assert_eq!(dir.read_bounded(name, 64).unwrap(), old);
        assert_eq!(names(&dir), [OsString::from("receipt.json")]);
    }
}

#[test]
fn directory_sync_fault_reports_new_complete_file_as_visible() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    let name = OsStr::new("receipt.json");
    let old = b"old complete receipt";
    let new = b"new complete receipt";
    dir.atomic_write(name, ".receipt.", old, PublishMode::Replace)
        .unwrap();

    let mut faults = FailAtomicWriteAt {
        phase: AtomicWritePhase::DirectorySync,
        errno: libc::EIO,
    };
    let publication = dir
        .atomic_write_with_operations(name, ".receipt.", new, PublishMode::Replace, &mut faults)
        .unwrap();
    match publication {
        Publication::VisibleDurabilityUnconfirmed { sync_error } => {
            assert_eq!(sync_error.raw_os_error(), Some(libc::EIO));
        }
        Publication::Durable => panic!("directory sync fault was not reported"),
    }
    assert_eq!(dir.read_bounded(name, 64).unwrap(), new);
    assert_eq!(names(&dir), [OsString::from("receipt.json")]);
}

#[cfg(feature = "fixture")]
struct PauseAtAtomicWritePhase {
    phase: AtomicWritePhase,
    socket: PathBuf,
}
#[cfg(feature = "fixture")]
impl PauseAtAtomicWritePhase {
    fn pause(&mut self, phase: AtomicWritePhase) -> Result<()> {
        if self.phase == phase {
            let mut stream = UnixStream::connect(&self.socket)
                .map_err(error("fixture socket connect", &self.socket))?;
            stream
                .write_all(b"R")
                .map_err(error("fixture handshake", &self.socket))?;
            loop {
                std::thread::park_timeout(Duration::from_secs(60));
            }
        }
        Ok(())
    }
}
#[cfg(feature = "fixture")]
impl AtomicWriteOperations for PauseAtAtomicWritePhase {
    fn write(&mut self, file: &mut FileHandle, bytes: &[u8], path: &Path) -> Result<()> {
        self.pause(AtomicWritePhase::Write)?;
        LocalAtomicWriteOperations.write(file, bytes, path)
    }
    fn sync_file(&mut self, file: &FileHandle) -> Result<()> {
        self.pause(AtomicWritePhase::FileSync)?;
        LocalAtomicWriteOperations.sync_file(file)
    }
    fn rename(
        &mut self,
        directory: &Directory,
        temporary: &OsStr,
        name: &OsStr,
        mode: PublishMode,
    ) -> Result<()> {
        self.pause(AtomicWritePhase::Rename)?;
        LocalAtomicWriteOperations.rename(directory, temporary, name, mode)
    }
    fn sync_directory(&mut self, directory: &Directory) -> Result<()> {
        self.pause(AtomicWritePhase::DirectorySync)?;
        LocalAtomicWriteOperations.sync_directory(directory)
    }
}

#[cfg(feature = "fixture")]
#[test]
fn atomic_write_crash_fixture() {
    let (Some(root), Some(socket), Some(phase)) = (
        std::env::var_os("PROVENANT_TEST_ATOMIC_ROOT"),
        std::env::var_os("PROVENANT_TEST_ATOMIC_SOCKET"),
        std::env::var_os("PROVENANT_TEST_ATOMIC_PHASE"),
    ) else {
        return;
    };
    let phase = match phase.to_str() {
        Some("write") => AtomicWritePhase::Write,
        Some("file-sync") => AtomicWritePhase::FileSync,
        Some("rename") => AtomicWritePhase::Rename,
        Some("directory-sync") => AtomicWritePhase::DirectorySync,
        _ => panic!("unknown atomic write fixture phase"),
    };
    let dir = Directory::open_anchor(Path::new(&root)).unwrap();
    let mut faults = PauseAtAtomicWritePhase {
        phase,
        socket: PathBuf::from(socket),
    };
    let _ = dir.atomic_write_with_operations(
        OsStr::new("receipt.json"),
        ".receipt.",
        b"new complete receipt",
        PublishMode::Replace,
        &mut faults,
    );
    panic!("fixture passed its requested crash boundary");
}

#[cfg(feature = "fixture")]
fn kill_fixture(
    child: &mut crate::sys::raw_exec::OwnedPid,
    reaper: &crate::sys::reaper::Reaper,
) -> bool {
    if child.signal(crate::identity::Signal::Kill).is_err() {
        reaper.resume();
        return false;
    }
    let Some(deadline) = Instant::now().checked_add(Duration::from_secs(2)) else {
        reaper.resume();
        return false;
    };
    loop {
        match child.observe() {
            Ok(Some(_)) => return true,
            Ok(None) => {}
            Err(_) => {
                reaper.resume();
                return false;
            }
        }
        if Instant::now() >= deadline {
            reaper.resume();
            return false;
        }
        std::thread::yield_now();
    }
}

#[cfg(feature = "fixture")]
#[test]
fn process_crash_at_publication_boundaries_leaves_old_or_new_complete_bytes() {
    for (phase, expected) in [
        ("write", &b"old complete receipt"[..]),
        ("file-sync", &b"old complete receipt"[..]),
        ("rename", &b"old complete receipt"[..]),
        ("directory-sync", &b"new complete receipt"[..]),
    ] {
        let temp = crate::fixture::short_tempdir(".tmp", "boundary.sock").unwrap();
        let dir = Directory::open_anchor(temp.path()).unwrap();
        dir.atomic_write(
            OsStr::new("receipt.json"),
            ".receipt.",
            b"old complete receipt",
            PublishMode::Replace,
        )
        .unwrap();
        let socket = temp.path().join("boundary.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let current_exe = std::env::current_exe().unwrap();
        let argv = [
            current_exe.into_os_string(),
            "--exact".into(),
            "fs::tests::atomic_write_crash_fixture".into(),
            "--nocapture".into(),
        ];
        let env = [
            ("PROVENANT_TEST_ATOMIC_ROOT".into(), temp.path().into()),
            (
                "PROVENANT_TEST_ATOMIC_SOCKET".into(),
                socket.into_os_string(),
            ),
            ("PROVENANT_TEST_ATOMIC_PHASE".into(), phase.into()),
        ];
        let reaper = crate::sys::reaper::Reaper::new_paused(1).unwrap();
        let permit = reaper.reserve().unwrap();
        let mut child = crate::sys::raw_exec::OwnedPid::spawn(
            permit,
            crate::sys::raw_exec::SpawnSpec {
                argv: &argv,
                env: Some(&env),
                cwd: temp.path(),
                input: false,
                secret_input: None,
                capture_stdout: false,
                capture_stderr: false,
            },
        )
        .unwrap();
        let mut stream = crate::sys::fixture_accept(&listener, Duration::from_secs(5)).unwrap();
        assert_eq!(
            crate::sys::fixture_read_byte(&mut stream, Duration::from_secs(5)).unwrap(),
            b'R'
        );
        assert!(
            kill_fixture(&mut child, &reaper),
            "fixture did not stop after boundary kill"
        );
        assert_eq!(
            dir.read_bounded(OsStr::new("receipt.json"), 64).unwrap(),
            expected
        );
        // Pre-rename kills can leave one staging file; publication consumes it.
        let mut entries = names(&dir);
        entries.retain(|entry| entry != "boundary.sock" && entry != "receipt.json");
        assert_eq!(entries.len(), usize::from(phase != "directory-sync"));
        assert!(
            entries
                .iter()
                .all(|entry| entry.as_bytes().starts_with(b".receipt."))
        );
        let leftovers = entries;
        dir.atomic_write(
            OsStr::new("receipt.json"),
            ".receipt.",
            b"later complete receipt",
            PublishMode::Replace,
        )
        .unwrap();
        assert_eq!(
            dir.read_bounded(OsStr::new("receipt.json"), 64).unwrap(),
            b"later complete receipt"
        );
        let mut entries = names(&dir);
        entries.retain(|entry| entry != "boundary.sock" && entry != "receipt.json");
        assert_eq!(entries, leftovers);
    }
}

#[test]
fn object_safe_atomic_write_preserves_requested_mode() {
    let temp = tempfile::tempdir().unwrap();
    let dir: &dyn SafeDir = &Directory::open_anchor(temp.path()).unwrap();
    assert!(matches!(
        dir.atomic_write(
            OsStr::new("provenant"),
            ".provenant-staging.",
            b"binary bytes",
            0o755,
            PublishMode::NoReplace,
        )
        .unwrap(),
        Publication::Durable
    ));
    assert_eq!(
        dir.metadata_at(OsStr::new("provenant"), false)
            .unwrap()
            .mode
            & 0o777,
        0o755
    );
    let mut file = dir.open_read(OsStr::new("provenant")).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"binary bytes");
}

#[test]
fn object_safe_read_link_uses_held_directory_and_rejects_noncomponents() {
    let temp = tempfile::tempdir().unwrap();
    let owner = Directory::open_anchor(temp.path()).unwrap();
    owner
        .symlink(Path::new("../target"), OsStr::new("managed-link"))
        .unwrap();
    let dir: &dyn SafeDir = &owner;
    assert_eq!(
        dir.read_link(OsStr::new("managed-link")).unwrap(),
        PathBuf::from("../target")
    );
    assert!(matches!(
        dir.read_link(OsStr::new("../escape")),
        Err(FsError::InvalidComponent { .. })
    ));
}

#[test]
fn object_safe_consumers_can_use_real_and_mock_files() {
    fn read_file(file: &mut dyn SafeFile) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }
    let temp = tempfile::tempdir().unwrap();
    let fs: &dyn SafeFs = &LocalFs;
    let dir = fs.open_anchor(temp.path()).unwrap();
    dir.atomic_write(
        OsStr::new("file"),
        ".test.",
        b"real",
        0o600,
        PublishMode::Replace,
    )
    .unwrap();
    assert_eq!(
        read_file(&mut *dir.open_read(OsStr::new("file")).unwrap()).unwrap(),
        b"real"
    );
    let mut mock = MockFile(std::io::Cursor::new(b"mock".to_vec()));
    assert_eq!(read_file(&mut mock).unwrap(), b"mock");
    assert_eq!(
        dir.entries().unwrap().collect::<Result<Vec<_>>>().unwrap(),
        [OsString::from("file")]
    );
}

struct MockFile(io::Cursor<Vec<u8>>);
impl Read for MockFile {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.0.read(bytes)
    }
}
impl Write for MockFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Seek for MockFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.0.seek(position)
    }
}
impl SafeFile for MockFile {
    fn metadata(&self) -> Result<Metadata> {
        Err(FsError::Io {
            operation: "mock metadata",
            path: PathBuf::from("<mock>"),
            source: io::ErrorKind::Unsupported.into(),
        })
    }
    fn duplicate(&self) -> Result<Box<dyn SafeFile>> {
        Ok(Box::new(Self(self.0.clone())))
    }
    fn sync(&self) -> Result<()> {
        Ok(())
    }
}

#[test]
fn typed_error_predicates_and_io_conversion_preserve_errno_and_kind() {
    for code in [
        errno::ENOENT,
        errno::EEXIST,
        errno::ELOOP,
        errno::EXDEV,
        errno::ENOTDIR,
        errno::EINVAL,
        errno::ENOTSUP,
        errno::EOPNOTSUPP,
        errno::ENOSYS,
        errno::EACCES,
    ] {
        let error =
            error("operation", Path::new("nested/entry"))(io::Error::from_raw_os_error(code));
        assert_eq!(error.is_not_found(), code == errno::ENOENT);
        assert_eq!(error.is_exists(), code == errno::EEXIST);
        assert_eq!(error.is_symlink(), code == errno::ELOOP);
        assert_eq!(error.is_loop(), code == errno::ELOOP);
        assert_eq!(error.is_cross_device(), code == errno::EXDEV);
        assert_eq!(error.is_not_directory(), code == errno::ENOTDIR);
        assert_eq!(
            error.is_unsupported(),
            [
                errno::EINVAL,
                errno::ENOTSUP,
                errno::EOPNOTSUPP,
                errno::ENOSYS
            ]
            .contains(&code)
        );
        let converted: io::Error = error.into();
        assert_eq!(converted.raw_os_error(), Some(code));
        assert_eq!(converted.kind(), io::Error::from_raw_os_error(code).kind());
    }
    let custom =
        error("read", Path::new("entry"))(io::Error::new(io::ErrorKind::UnexpectedEof, "short"));
    let converted: io::Error = custom.into();
    assert_eq!(converted.raw_os_error(), None);
    assert_eq!(converted.kind(), io::ErrorKind::UnexpectedEof);
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    dir.create_new(OsStr::new("file")).unwrap();
    let error = dir.open_directory(OsStr::new("file")).unwrap_err();
    assert!(matches!(error, FsError::Io { .. }));
    assert!(error.is_not_directory());
    assert_eq!(error.raw_os_error(), Some(errno::ENOTDIR));
}

#[test]
fn verified_regular_reads_clear_nonblocking_status() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    dir.create_new(OsStr::new("file")).unwrap();
    let file = dir.open_read(OsStr::new("file")).unwrap();
    assert!(!os::tests::is_nonblocking(&file.file));
    let source = FileHandle::open_source(&temp.path().join("file")).unwrap();
    assert!(!os::tests::is_nonblocking(&source.file));
}

#[test]
fn rename_errors_retain_both_paths_and_errno() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    let error = dir
        .replace(OsStr::new("missing"), &dir, OsStr::new("destination"))
        .unwrap_err();
    assert!(error.is_not_found());
    assert!(
        matches!(&error, FsError::Rename { from_path, to_path, .. } if from_path.ends_with("missing") && to_path.ends_with("destination"))
    );
    assert_eq!(io::Error::from(error).raw_os_error(), Some(errno::ENOENT));
}
#[test]
fn boxed_filesystem_capabilities_support_threads() {
    fn check<T: Send + Sync + ?Sized>() {}
    check::<dyn SafeFs>();
    check::<dyn SafeDir>();
    check::<dyn SafeFile>();
}

#[test]
fn stable_lock_namespace_refuses_every_mutation_without_splitting_exclusion() {
    let temp = tempfile::tempdir().unwrap();
    let dir = Directory::open_anchor(temp.path()).unwrap();
    let clock = FixedClock(provenant_core::MonoTime::from_micros(1_000_000));
    let token = CancelToken::new();
    let name = OsStr::new("writer.lock");
    let guard = dir
        .lock(
            name,
            LockMode::Exclusive,
            one_second_budget(),
            &token,
            &clock,
        )
        .unwrap();
    let inode = dir.metadata_at(name, false).unwrap();
    dir.create_new(OsStr::new("replacement")).unwrap();
    for result in [
        dir.unlink(name),
        dir.replace(OsStr::new("replacement"), &dir, name),
        dir.replace(name, &dir, OsStr::new("other")),
        dir.rename_noreplace(name, &dir, OsStr::new("other")),
        dir.exchange(OsStr::new("replacement"), &dir, name),
        dir.symlink(Path::new("replacement"), name),
    ] {
        assert!(matches!(result, Err(FsError::ReservedLockName { .. })));
    }
    assert!(matches!(
        dir.atomic_write(name, ".stage", b"new", PublishMode::Replace),
        Err(FsError::ReservedLockName { .. })
    ));
    assert!(matches!(
        dir.replace_link(name, Path::new("replacement")),
        Err(FsError::ReservedLockName { .. })
    ));
    for alias in ["writer.LOCK", "writer.locK"] {
        assert!(matches!(
            dir.unlink(OsStr::new(alias)),
            Err(FsError::ReservedLockName { .. })
        ));
        assert!(matches!(
            dir.replace(OsStr::new("replacement"), &dir, OsStr::new(alias)),
            Err(FsError::ReservedLockName { .. })
        ));
    }
    assert!(inode.same_identity(&dir.metadata_at(name, false).unwrap()));
    assert!(matches!(
        dir.lock(
            name,
            LockMode::Exclusive,
            one_second_budget(),
            &token,
            &clock
        ),
        Err(FsError::LockTimeout { .. })
    ));
    drop(guard);
    assert!(
        dir.lock(
            name,
            LockMode::Exclusive,
            one_second_budget(),
            &token,
            &clock
        )
        .is_ok()
    );
    assert!(matches!(
        dir.lock(
            OsStr::new("ambiguous"),
            LockMode::Exclusive,
            one_second_budget(),
            &token,
            &clock
        ),
        Err(FsError::InvalidLockName { .. })
    ));
}

#[test]
fn reanchor_rebinds_device_policy_without_reopening_the_directory() {
    let temp = tempfile::tempdir().unwrap();
    let original = temp.path().join("root");
    std::fs::create_dir(&original).unwrap();
    let mut directory = Directory::open_anchor(&original).unwrap();
    let identity = directory.metadata().unwrap();
    // Model a configured root reached across a mount from its original anchor.
    directory.device = identity.device.checked_add(1).unwrap();
    let clock = provenant_core::TestClock::new(
        provenant_core::WallTime::from_micros(0).unwrap(),
        provenant_core::MonoTime::from_micros(0),
    );
    let budget =
        provenant_core::WaitBudget::cli(provenant_core::MonoTime::from_micros(0), 1).unwrap();
    let cancel = provenant_core::CancelToken::new();
    assert!(matches!(
        directory.lock(
            OsStr::new("owner.lock"),
            LockMode::Exclusive,
            budget,
            &cancel,
            &clock
        ),
        Err(FsError::CrossDevice { .. })
    ));
    // Replace the pathname. Reanchoring must retain the validated inode,
    // rather than reopening the different directory now at that path.
    std::fs::rename(&original, temp.path().join("moved")).unwrap();
    std::fs::create_dir(&original).unwrap();
    let mut directory = directory.into_anchor().unwrap();
    assert!(directory.metadata().unwrap().same_identity(&identity));
    let guard = directory
        .lock(
            OsStr::new("owner.lock"),
            LockMode::Exclusive,
            budget,
            &cancel,
            &clock,
        )
        .unwrap();
    drop(guard);
    // Descendant lock checks still enforce the newly pinned device boundary.
    directory.device = identity.device.checked_add(1).unwrap();
    assert!(matches!(
        directory.lock(
            OsStr::new("owner.lock"),
            LockMode::Exclusive,
            budget,
            &cancel,
            &clock
        ),
        Err(FsError::CrossDevice { .. })
    ));
}

#[test]
fn a_handle_received_through_guardian_handoff_never_reanchors() {
    let temp = tempfile::tempdir().unwrap();
    let root = Directory::open_anchor(temp.path()).unwrap();
    let handoff = Directory::from_handoff(root.handoff_file().try_clone().unwrap()).unwrap();
    assert!(matches!(
        handoff.into_anchor(),
        Err(FsError::Symlink { .. })
    ));
}

#[test]
fn a_handle_reached_through_a_followed_link_never_reanchors() {
    let temp = tempfile::tempdir().unwrap();
    let root = Directory::open_anchor(temp.path()).unwrap();
    root.mkdir(OsStr::new("directory"), 0o700).unwrap();
    root.symlink(Path::new("directory"), OsStr::new("dir-link"))
        .unwrap();
    let linked = root
        .open_directory_with_links(OsStr::new("dir-link"), true)
        .unwrap();
    let error = linked.into_anchor().unwrap_err();
    assert!(matches!(error, FsError::Symlink { .. }));
    assert_eq!(error.raw_os_error(), Some(errno::ELOOP));
    let descendant = root
        .open_directory_with_links(OsStr::new("dir-link"), true)
        .unwrap();
    descendant.mkdir(OsStr::new("child"), 0o700).unwrap();
    let child = descendant.open_directory(OsStr::new("child")).unwrap();
    assert!(matches!(child.into_anchor(), Err(FsError::Symlink { .. })));
    let direct = root.open_directory(OsStr::new("directory")).unwrap();
    direct.into_anchor().unwrap();
}

#[test]
fn root_binding_resolves_aliases_and_missing_suffix_without_creation() {
    use provenant_core::PathResolver;
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("real");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, temp.path().join("alias")).unwrap();
    let missing = temp.path().join("alias/fresh/state");
    let expected = std::fs::canonicalize(&real).unwrap().join("fresh/state");
    assert_eq!(
        LocalFs.resolve_existing_ancestor(&missing).unwrap(),
        expected
    );
    assert!(!real.join("fresh").exists());
    std::fs::write(real.join("file"), b"not a directory").unwrap();
    assert!(
        LocalFs
            .resolve_existing_ancestor(&real.join("file/state"))
            .is_err()
    );
}

#[test]
fn trait_cross_directory_rename_preserves_open_inode_and_refuses_collision() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("source")).unwrap();
    std::fs::create_dir(temp.path().join("archive")).unwrap();
    std::fs::write(temp.path().join("source/file"), b"observed").unwrap();
    std::fs::write(temp.path().join("archive/occupied"), b"retained").unwrap();
    let source: Box<dyn SafeDir> = LocalFs.open_anchor(&temp.path().join("source")).unwrap();
    let archive: Box<dyn SafeDir> = LocalFs.open_anchor(&temp.path().join("archive")).unwrap();
    let mut writer = std::fs::OpenOptions::new()
        .append(true)
        .open(temp.path().join("source/file"))
        .unwrap();
    assert!(
        source
            .rename_noreplace_to(OsStr::new("file"), archive.as_ref(), OsStr::new("occupied"))
            .is_err()
    );
    source
        .rename_noreplace_to(OsStr::new("file"), archive.as_ref(), OsStr::new("saved"))
        .unwrap();
    writer.write_all(b" later").unwrap();
    assert_eq!(
        {
            let mut bytes = Vec::new();
            archive
                .open_read(OsStr::new("saved"))
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
            bytes
        },
        b"observed later"
    );
    assert_eq!(
        std::fs::read(temp.path().join("archive/occupied")).unwrap(),
        b"retained"
    );
    assert!(!temp.path().join("source/file").exists());
}
