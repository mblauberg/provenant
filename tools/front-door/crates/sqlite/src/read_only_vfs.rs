//! Process-lifetime copy of SQLite's native VFS with read-only file opens.
//! mode=ro alone still asks the native VFS to open WAL read/write/create.
use rusqlite::ffi;
use std::{
    ffi::{c_char, c_int},
    sync::OnceLock,
};

type Open = unsafe extern "C" fn(
    *mut ffi::sqlite3_vfs,
    *const c_char,
    *mut ffi::sqlite3_file,
    c_int,
    *mut c_int,
) -> c_int;

#[repr(C)]
struct ReadOnlyVfs {
    native: ffi::sqlite3_vfs,
    open: Open,
}

/// Writer-only setup: retain companions for later read-only observations.
pub(super) fn persist_wal(db: &rusqlite::Connection) -> rusqlite::Result<()> {
    let mut enabled: c_int = 1;
    // SAFETY: Connection lends its valid native handle for this synchronous
    // file-control call; the live integer matches PERSIST_WAL's argument type.
    let handle = unsafe { db.handle() };
    // SAFETY: handle and enabled remain live; a null name selects the main DB.
    let code = unsafe {
        ffi::sqlite3_file_control(
            handle,
            std::ptr::null(),
            ffi::SQLITE_FCNTL_PERSIST_WAL,
            (&mut enabled as *mut c_int).cast(),
        )
    };
    if code == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(rusqlite::Error::SqliteFailure(ffi::Error::new(code), None))
    }
}

pub(super) fn register() -> rusqlite::Result<()> {
    static REGISTERED: OnceLock<c_int> = OnceLock::new();
    let code = *REGISTERED.get_or_init(|| {
        // SAFETY: SQLite initialises and returns a process-lifetime native VFS.
        let base = unsafe { ffi::sqlite3_vfs_find(std::ptr::null()) };
        if base.is_null() {
            return ffi::SQLITE_CANTOPEN;
        }
        // SAFETY: base was checked non-null and its registered VFS is live.
        let mut native = unsafe { *base };
        let Some(open) = native.xOpen else {
            return ffi::SQLITE_CANTOPEN;
        };
        native.zName = c"provenant-read-only".as_ptr();
        native.pNext = std::ptr::null_mut();
        native.xOpen = Some(read_open);
        native.xDelete = Some(refuse_delete);
        let wrapper = Box::leak(Box::new(ReadOnlyVfs { native, open }));
        // SAFETY: the registered native copy and its static name live forever;
        // all unchanged callbacks retain the native VFS's pAppData and layout.
        unsafe { ffi::sqlite3_vfs_register(&mut wrapper.native, 0) }
    });
    if code == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(rusqlite::Error::SqliteFailure(ffi::Error::new(code), None))
    }
}

unsafe extern "C" fn read_open(
    vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    output: *mut c_int,
) -> c_int {
    // Temporary files and journals are never part of a local observation.
    if flags & (ffi::SQLITE_OPEN_MAIN_DB | ffi::SQLITE_OPEN_WAL) == 0 {
        return ffi::SQLITE_READONLY;
    }
    let flags = (flags
        & !(ffi::SQLITE_OPEN_READWRITE
            | ffi::SQLITE_OPEN_CREATE
            | ffi::SQLITE_OPEN_DELETEONCLOSE
            | ffi::SQLITE_OPEN_EXCLUSIVE))
        | ffi::SQLITE_OPEN_READONLY;
    // SAFETY: SQLite passes the VFS registered above. repr(C) places native
    // first, so the pointer also addresses its immutable Open callback.
    let open = unsafe { (*vfs.cast::<ReadOnlyVfs>()).open };
    // SAFETY: the native callback receives SQLite's original live pointers,
    // its copied VFS data, and a narrowed read-only access mask.
    unsafe { open(vfs, name, file, flags, output) }
}

unsafe extern "C" fn refuse_delete(_: *mut ffi::sqlite3_vfs, _: *const c_char, _: c_int) -> c_int {
    ffi::SQLITE_READONLY
}

/// Reader with a writable descriptor only for SQLite's exclusive lock protocol.
pub(super) fn register_locking_reader() -> rusqlite::Result<()> {
    static REGISTERED: OnceLock<c_int> = OnceLock::new();
    let code = *REGISTERED.get_or_init(|| {
        // SAFETY: SQLite returns a process-lifetime registered native VFS.
        let base = unsafe { ffi::sqlite3_vfs_find(std::ptr::null()) };
        if base.is_null() {
            return ffi::SQLITE_CANTOPEN;
        }
        // SAFETY: base is non-null and points to the registered native VFS.
        let mut native = unsafe { *base };
        let Some(open) = native.xOpen else {
            return ffi::SQLITE_CANTOPEN;
        };
        native.zName = c"provenant-locking-reader".as_ptr();
        native.pNext = std::ptr::null_mut();
        native.xOpen = Some(locking_read_open);
        native.xDelete = Some(refuse_delete);
        let wrapper = Box::leak(Box::new(ReadOnlyVfs { native, open }));
        // SAFETY: copied native data and static name remain live for the process.
        unsafe { ffi::sqlite3_vfs_register(&mut wrapper.native, 0) }
    });
    if code == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(rusqlite::Error::SqliteFailure(ffi::Error::new(code), None))
    }
}

unsafe extern "C" fn locking_read_open(
    vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    output: *mut c_int,
) -> c_int {
    if flags & (ffi::SQLITE_OPEN_MAIN_DB | ffi::SQLITE_OPEN_WAL) == 0 {
        return ffi::SQLITE_READONLY;
    }
    let main = flags & ffi::SQLITE_OPEN_MAIN_DB != 0;
    let flags = (flags
        & !(ffi::SQLITE_OPEN_READONLY
            | ffi::SQLITE_OPEN_READWRITE
            | ffi::SQLITE_OPEN_CREATE
            | ffi::SQLITE_OPEN_DELETEONCLOSE
            | ffi::SQLITE_OPEN_EXCLUSIVE))
        | if main {
            ffi::SQLITE_OPEN_READWRITE
        } else {
            ffi::SQLITE_OPEN_READONLY
        };
    // SAFETY: repr(C) preserves the native VFS prefix and its saved callback.
    let open = unsafe { (*vfs.cast::<ReadOnlyVfs>()).open };
    // SAFETY: native open receives SQLite's live file/name/output buffers.
    let code = unsafe { open(vfs, name, file, flags, output) };
    if code != ffi::SQLITE_OK {
        if !main && code == ffi::SQLITE_CANTOPEN && exclusive_main_lock(name) && absent_wal(name) {
            // SQLite has already acquired the main database's exclusive lock.
            // A closed, checkpointed WAL database may have no WAL at all. Its
            // empty read-only view needs no inode or shared-memory companion.
            // SAFETY: SQLite supplied the live file output buffer.
            unsafe { (*file).pMethods = &EMPTY_WAL };
            if !output.is_null() {
                // SAFETY: output is SQLite's live flags buffer.
                unsafe { *output = ffi::SQLITE_OPEN_READONLY | ffi::SQLITE_OPEN_WAL };
            }
            return ffi::SQLITE_OK;
        }
        return code;
    }
    // SAFETY: a successful native open initialises the file and method table.
    let methods = unsafe { (*file).pMethods };
    let replacement = locking_methods(methods);
    match replacement {
        Some(replacement) => {
            // SAFETY: file remains live and replacement is an immutable process-lifetime table.
            unsafe { (*file).pMethods = replacement };
            if !output.is_null() {
                // SAFETY: SQLite supplied its live output flag pointer. The pager is read-only;
                // only the native descriptor retains write access to support F_WRLCK.
                unsafe {
                    *output = (flags & !ffi::SQLITE_OPEN_READWRITE) | ffi::SQLITE_OPEN_READONLY
                };
            }
            ffi::SQLITE_OK
        }
        None => {
            // SAFETY: native open succeeded and supplied a live table.
            let close = unsafe { (*methods).xClose };
            if let Some(close) = close {
                // SAFETY: file is the successfully opened native file being abandoned.
                unsafe { close(file) };
            }
            ffi::SQLITE_IOERR
        }
    }
}

static LOCKING_TABLES: OnceLock<std::sync::Mutex<Vec<(usize, usize)>>> = OnceLock::new();

fn locking_methods(
    original: *const ffi::sqlite3_io_methods,
) -> Option<*const ffi::sqlite3_io_methods> {
    // Native VFS method tables are process-lifetime constants. Cache one hardened
    // copy per native table, never one allocation per connection or file open.
    let mut tables = LOCKING_TABLES
        .get_or_init(|| std::sync::Mutex::new(Vec::new()))
        .lock()
        .ok()?;
    if let Some((_, copy)) = tables.iter().find(|(key, _)| *key == original as usize) {
        return Some(*copy as *const ffi::sqlite3_io_methods);
    }
    if original.is_null() {
        return None;
    }
    // SAFETY: callers pass the successfully opened native file's live method table.
    let mut copy = unsafe { *original };
    copy.xWrite = Some(refuse_write);
    copy.xTruncate = Some(refuse_truncate);
    copy.xFileControl = Some(refuse_control);
    copy.xShmMap = Some(refuse_shm);
    // No writable mmap or shared-memory mapping can bypass the write callbacks.
    copy.iVersion = copy.iVersion.min(2);
    copy.xFetch = None;
    copy.xUnfetch = None;
    let copy = Box::leak(Box::new(copy)) as *const ffi::sqlite3_io_methods;
    tables.push((original as usize, copy as usize));
    Some(copy)
}

unsafe extern "C" fn refuse_write(
    _: *mut ffi::sqlite3_file,
    _: *const std::ffi::c_void,
    _: c_int,
    _: ffi::sqlite3_int64,
) -> c_int {
    ffi::SQLITE_READONLY
}
unsafe extern "C" fn refuse_truncate(_: *mut ffi::sqlite3_file, _: ffi::sqlite3_int64) -> c_int {
    ffi::SQLITE_READONLY
}
unsafe extern "C" fn refuse_control(
    _: *mut ffi::sqlite3_file,
    _: c_int,
    _: *mut std::ffi::c_void,
) -> c_int {
    // Native SIZE_HINT/CHUNK_SIZE/OVERWRITE controls can resize or change files
    // without xWrite/xTruncate. No native file-control mutation is delegated.
    ffi::SQLITE_NOTFOUND
}
unsafe extern "C" fn refuse_shm(
    _: *mut ffi::sqlite3_file,
    _: c_int,
    _: c_int,
    _: c_int,
    _: *mut *mut std::ffi::c_void,
) -> c_int {
    ffi::SQLITE_READONLY
}

fn exclusive_main_lock(name: *const c_char) -> bool {
    if name.is_null() {
        return false;
    }
    // SAFETY: SQLite supplied this WAL filename, whose associated main file stays open.
    let main = unsafe { ffi::sqlite3_database_file_object(name) };
    if main.is_null() {
        return false;
    }
    // SAFETY: SQLite returned its live associated database file.
    let methods = unsafe { (*main).pMethods };
    let Some(tables) = LOCKING_TABLES.get() else {
        return false;
    };
    let Ok(tables) = tables.lock() else {
        return false;
    };
    let Some((original, _)) = tables.iter().find(|(_, copy)| *copy == methods as usize) else {
        return false;
    };
    // SAFETY: cached originals are immutable process-lifetime native method tables.
    let control = unsafe { (*(*original as *const ffi::sqlite3_io_methods)).xFileControl };
    drop(tables);
    let Some(control) = control else {
        return false;
    };
    let mut state: c_int = 0;
    // SAFETY: LOCKSTATE is a read-only query with a live main file and integer output.
    let code = unsafe {
        control(
            main,
            ffi::SQLITE_FCNTL_LOCKSTATE,
            (&mut state as *mut c_int).cast(),
        )
    };
    code == ffi::SQLITE_OK && state == ffi::SQLITE_LOCK_EXCLUSIVE
}

fn absent_wal(name: *const c_char) -> bool {
    use std::os::unix::ffi::OsStrExt;
    if name.is_null() {
        return false;
    }
    // SAFETY: the SQLite xOpen callback receives a NUL-terminated filename.
    let bytes = unsafe { std::ffi::CStr::from_ptr(name) }.to_bytes();
    matches!(std::fs::symlink_metadata(std::path::Path::new(std::ffi::OsStr::from_bytes(bytes))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}

static EMPTY_WAL: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
    iVersion: 1,
    xClose: Some(empty_close),
    xRead: Some(empty_read),
    xWrite: Some(refuse_write),
    xTruncate: Some(refuse_truncate),
    xSync: Some(empty_sync),
    xFileSize: Some(empty_size),
    xLock: Some(empty_lock),
    xUnlock: Some(empty_lock),
    xCheckReservedLock: Some(empty_reserved),
    xFileControl: Some(refuse_control),
    xSectorSize: Some(empty_sector),
    xDeviceCharacteristics: Some(empty_device),
    xShmMap: None,
    xShmLock: None,
    xShmBarrier: None,
    xShmUnmap: None,
    xFetch: None,
    xUnfetch: None,
};
unsafe extern "C" fn empty_close(_: *mut ffi::sqlite3_file) -> c_int {
    ffi::SQLITE_OK
}
unsafe extern "C" fn empty_read(
    _: *mut ffi::sqlite3_file,
    buffer: *mut std::ffi::c_void,
    count: c_int,
    _: ffi::sqlite3_int64,
) -> c_int {
    if count < 0 {
        return ffi::SQLITE_IOERR_READ;
    }
    // SAFETY: SQLite lends a writable buffer of count bytes to xRead.
    unsafe { std::ptr::write_bytes(buffer.cast::<u8>(), 0, count as usize) };
    ffi::SQLITE_IOERR_SHORT_READ
}
unsafe extern "C" fn empty_size(
    _: *mut ffi::sqlite3_file,
    output: *mut ffi::sqlite3_int64,
) -> c_int {
    // SAFETY: SQLite supplies a live file-size output pointer.
    unsafe { *output = 0 };
    ffi::SQLITE_OK
}
unsafe extern "C" fn empty_sync(_: *mut ffi::sqlite3_file, _: c_int) -> c_int {
    ffi::SQLITE_READONLY
}
// WAL locking is exclusively on the real main file, never this empty WAL view.
unsafe extern "C" fn empty_lock(_: *mut ffi::sqlite3_file, _: c_int) -> c_int {
    ffi::SQLITE_OK
}
unsafe extern "C" fn empty_reserved(_: *mut ffi::sqlite3_file, output: *mut c_int) -> c_int {
    // SAFETY: SQLite supplies a live reserved-lock output pointer.
    unsafe { *output = 0 };
    ffi::SQLITE_OK
}
unsafe extern "C" fn empty_sector(_: *mut ffi::sqlite3_file) -> c_int {
    4096
}
unsafe extern "C" fn empty_device(_: *mut ffi::sqlite3_file) -> c_int {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{enable_wal, is_busy, open_connection, open_locking_read_only_connection};
    use std::{fs, path::Path, time::Duration};

    type FileSnapshot = Vec<(std::ffi::OsString, Vec<u8>)>;

    fn files(directory: &Path) -> Result<FileSnapshot, Box<dyn std::error::Error>> {
        let mut files = fs::read_dir(directory)?
            .map(|entry| {
                let entry = entry?;
                Ok((entry.file_name(), fs::read(entry.path())?))
            })
            .collect::<Result<Vec<_>, std::io::Error>>()?;
        files.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(files)
    }

    #[test]
    fn locking_reader_observes_wal_without_writes_or_companions_and_excludes_writers()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = std::env::temp_dir().join(format!(
            "provenant-locking-reader-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir(&directory)?;
        let path = directory.join("db.sqlite3");
        fs::File::create(&path)?;
        let writer = open_connection(&path, false)?;
        enable_wal(&writer).map_err(|_| "enable wal")?;
        writer.execute_batch("CREATE TABLE t(value INTEGER); PRAGMA wal_checkpoint(TRUNCATE);")?;
        let before_commit = fs::read(&path)?;
        writer.execute_batch("INSERT INTO t VALUES (42);")?;
        let committed_wal = fs::read(directory.join("db.sqlite3-wal"))?;
        drop(writer);
        let checkpointed = fs::read(&path)?;
        for with_wal in [true, false] {
            fs::write(
                &path,
                if with_wal {
                    &before_commit
                } else {
                    &checkpointed
                },
            )?;
            if with_wal {
                fs::write(directory.join("db.sqlite3-wal"), &committed_wal)?;
            }
            for suffix in if with_wal {
                vec!["-shm"]
            } else {
                vec!["-shm", "-wal"]
            } {
                match fs::remove_file(directory.join(format!("db.sqlite3{suffix}"))) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            let before = files(&directory)?;
            let reader = open_locking_read_only_connection(&path)
                .map_err(|error| format!("open with_wal={with_wal}: {error}"))?;
            let value: i64 = reader
                .query_row("SELECT value FROM t", [], |row| row.get(0))
                .map_err(|error| format!("read with_wal={with_wal}: {error}"))?;
            assert_eq!(value, 42);
            assert_eq!(
                files(&directory)?,
                before,
                "reader changed files while open"
            );
            reader.pragma_update(None, "query_only", false)?;
            assert!(reader.execute("INSERT INTO t VALUES (99)", []).is_err());
            assert_native_writes_refused(&reader)?;
            assert_eq!(files(&directory)?, before);
            let contender = open_connection(&path, false)?;
            contender.busy_timeout(Duration::ZERO)?;
            let error = contender
                .execute("INSERT INTO t VALUES (7)", [])
                .err()
                .ok_or("writer was not refused")?;
            assert!(
                is_busy(&error),
                "concurrent writer was not refused: {error}"
            );
            assert_eq!(files(&directory)?, before);
            drop(contender);
            drop(reader);
            assert_eq!(files(&directory)?, before, "reader changed files on close");
        }
        let writer = open_connection(&path, false)?;
        writer.execute("INSERT INTO t VALUES (7)", [])?;
        drop(writer);
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    fn assert_native_writes_refused(
        reader: &rusqlite::Connection,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut file: *mut ffi::sqlite3_file = std::ptr::null_mut();
        // SAFETY: the live handle and output pointer match FILE_POINTER's API.
        let handle = unsafe { reader.handle() };
        // SAFETY: the live native handle and output pointer match FILE_POINTER's API.
        let code = unsafe {
            ffi::sqlite3_file_control(
                handle,
                std::ptr::null(),
                ffi::SQLITE_FCNTL_FILE_POINTER,
                (&mut file as *mut *mut ffi::sqlite3_file).cast(),
            )
        };
        assert_eq!(code, ffi::SQLITE_OK);
        assert!(!file.is_null());
        // SAFETY: FILE_POINTER returned the open file and its live method table.
        let methods = unsafe { (*file).pMethods };
        // SAFETY: the open file owns a live immutable method table.
        let methods = unsafe { &*methods };
        let write = methods.xWrite.ok_or("write method")?;
        let truncate = methods.xTruncate.ok_or("truncate method")?;
        let control = methods.xFileControl.ok_or("file control")?;
        let byte = 0u8;
        // SAFETY: the live file and one-byte buffer match xWrite's signature.
        assert_eq!(
            unsafe { write(file, (&byte as *const u8).cast(), 1, 0) },
            ffi::SQLITE_READONLY
        );
        // SAFETY: the file remains open; truncation is refused before native work.
        assert_eq!(unsafe { truncate(file, 0) }, ffi::SQLITE_READONLY);
        let mut size: ffi::sqlite3_int64 = 1_000_000;
        // SAFETY: SIZE_HINT receives its documented live integer argument.
        assert_eq!(
            unsafe {
                control(
                    file,
                    ffi::SQLITE_FCNTL_SIZE_HINT,
                    (&mut size as *mut ffi::sqlite3_int64).cast(),
                )
            },
            ffi::SQLITE_NOTFOUND
        );
        Ok(())
    }
}
