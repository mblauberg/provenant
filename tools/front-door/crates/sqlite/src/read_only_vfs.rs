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
