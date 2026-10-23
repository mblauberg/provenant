//! Partial port: RP-1003 records the remaining Python compatibility work.
//! Machine-local product-root pointers. Loading a stale path supports relocation.
use crate::paths::{expand_user, resolve, Homes, PathError};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const SCHEMA_VERSION: u64 = 1;
pub const POINTER_RELATIVE_PATH: &str = ".agent-fabric/product-root.json";

#[derive(Debug, Serialize, Deserialize)]
struct ProductPointer {
    schema_version: u64,
    product_root: String,
}
#[derive(Debug, Error)]
pub enum PointerError {
    #[error(transparent)]
    Path(#[from] PathError),
    #[error(
        "product pointer directory escapes the instance root: {parent} resolves to {resolved}"
    )]
    Escape { parent: PathBuf, resolved: PathBuf },
    #[error("product pointer directory must not be a symlink")]
    Symlink,
    #[error("cannot write product pointer: {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("product root cannot be represented as a JSON string")]
    Encoding,
    #[error("cannot encode product pointer")]
    Json(#[from] serde_json::Error),
}

/// Invalid/missing JSON is absent; an unresolvable home retains Python's
/// escaping RuntimeError as a typed path error instead of becoming absence.
pub fn load_pointer_path(
    instance_root: &Path,
    homes: &Homes,
) -> Result<Option<PathBuf>, PathError> {
    // read_text, unlike json.loads(bytes), does not accept a UTF-8 BOM.
    let Ok(raw) = fs::read_to_string(instance_root.join(POINTER_RELATIVE_PATH)) else {
        return Ok(None);
    };
    // Preserve Python's last-member-wins policy before decoding the typed record.
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Ok(None);
    };
    let Ok(pointer) = serde_json::from_value::<ProductPointer>(value) else {
        return Ok(None);
    };
    if pointer.schema_version != SCHEMA_VERSION || pointer.product_root.is_empty() {
        return Ok(None);
    }
    let candidate = expand_user(Path::new(&pointer.product_root), homes)?;
    Ok(candidate.is_absolute().then_some(candidate))
}
pub fn load_pointer_file(
    instance_root: &Path,
    homes: &Homes,
) -> Result<Option<PathBuf>, PathError> {
    Ok(load_pointer_path(instance_root, homes)?.filter(|v| v.exists()))
}

/// Safe publication seam. Supervisor's directory handles implement this at the
/// composition edge; core cannot import supervisor under PORTING.md R2.
/// Implementations must fsync staged bytes, set mode 0600, atomically replace
/// the destination (without following a destination symlink), and clean up.
pub trait PointerWriter {
    fn atomic_write(&self, destination: &Path, content: &[u8], prefix: &str) -> io::Result<()>;
}

pub fn write_pointer_file(
    instance_root: &Path,
    product_root: &Path,
    homes: &Homes,
    writer: &impl PointerWriter,
) -> Result<(), PointerError> {
    let product = expand_user(product_root, homes)?;
    let product = fs::canonicalize(&product).map_err(|source| PointerError::Io {
        path: product,
        source,
    })?;
    let instance = expand_user(instance_root, homes)?;
    let resolved_instance = resolve(&instance)?;
    let parent = instance.join(".agent-fabric");
    let resolved_parent = resolve(&parent)?;
    if parent.is_symlink() {
        if !resolved_parent.starts_with(&resolved_instance) {
            return Err(PointerError::Escape {
                parent,
                resolved: resolved_parent,
            });
        }
        return Err(PointerError::Symlink);
    }
    fs::create_dir_all(&parent).map_err(|source| PointerError::Io {
        path: parent.clone(),
        source,
    })?;
    let resolved_parent = resolve(&parent)?;
    if !resolved_parent.starts_with(&resolved_instance) {
        return Err(PointerError::Escape {
            parent,
            resolved: resolved_parent,
        });
    }
    let ignore = parent.join(".gitignore");
    writer
        .atomic_write(&ignore, b"*\n", ".gitignore.")
        .map_err(|source| PointerError::Io {
            path: ignore,
            source,
        })?;
    // Python's native path boundary uses surrogateescape on Unix, then an
    // explicit ASCII JSON encoder. No lossy UTF-8 conversion is permitted.
    let escaped = crate::pending::path_json(&product).map_err(|_| PointerError::Encoding)?;
    let content = format!("{{\"product_root\": {escaped}, \"schema_version\": 1}}\n");
    let pointer = parent.join("product-root.json");
    writer
        .atomic_write(&pointer, content.as_bytes(), ".product-root.")
        .map_err(|source| PointerError::Io {
            path: pointer,
            source,
        })
}

#[cfg(test)]
fn write_fixture_pointer(
    instance: &Path,
    product: &Path,
    homes: &Homes,
) -> Result<(), PointerError> {
    struct Writer;
    impl PointerWriter for Writer {
        fn atomic_write(&self, destination: &Path, content: &[u8], prefix: &str) -> io::Result<()> {
            crate::pending::atomic_write(destination, content, prefix)
        }
    }
    write_pointer_file(instance, product, homes, &Writer)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pointer_round_trip_and_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let instance = dir.path().join("instance");
        let product = dir.path().join("prød😀");
        fs::create_dir(&product).unwrap();
        write_fixture_pointer(&instance, &product, &Homes::default()).unwrap();
        assert_eq!(
            load_pointer_file(&instance, &Homes::default()).unwrap(),
            Some(fs::canonicalize(&product).unwrap())
        );
        assert_eq!(
            fs::read_to_string(instance.join(".agent-fabric/.gitignore")).unwrap(),
            "*\n"
        );
        let raw = fs::read_to_string(instance.join(POINTER_RELATIVE_PATH)).unwrap();
        assert!(raw.contains("pr\\u00f8d\\ud83d\\ude00"));
        assert!(raw.ends_with(", \"schema_version\": 1}\n"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(instance.join(POINTER_RELATIVE_PATH))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn missing_invalid_stale_and_unknown_members() {
        let dir = tempfile::tempdir().unwrap();
        let instance = dir.path();
        let path = instance.join(POINTER_RELATIVE_PATH);
        let homes = Homes::default();
        assert!(load_pointer_file(instance, &homes).unwrap().is_none());
        fs::create_dir(path.parent().unwrap()).unwrap();
        for raw in [
            "{",
            "[]",
            r#"{"schema_version":true,"product_root":"/tmp"}"#,
            r#"{"schema_version":1.0,"product_root":"/tmp"}"#,
            r#"{"schema_version":2,"product_root":"/tmp"}"#,
            r#"{"schema_version":1,"product_root":"relative"}"#,
            r#"{"schema_version":1,"product_root":""}"#,
        ] {
            fs::write(&path, raw).unwrap();
            assert!(
                load_pointer_path(instance, &homes).unwrap().is_none(),
                "{raw}"
            );
        }
        let stale = dir.path().join("old'\\product");
        fs::write(
            &path,
            serde_json::json!({"schema_version":1,"product_root":stale,"extra":42}).to_string(),
        )
        .unwrap();
        assert_eq!(load_pointer_path(instance, &homes).unwrap(), Some(stale));
        assert!(load_pointer_file(instance, &homes).unwrap().is_none());
    }
    #[test]
    fn concurrent_writes_leave_valid_complete_json() {
        let dir = tempfile::tempdir().unwrap();
        let instance = dir.path().join("instance");
        let products: Vec<_> = (0..8)
            .map(|n| {
                let p = dir.path().join(format!("product-{n}"));
                fs::create_dir(&p).unwrap();
                fs::canonicalize(p).unwrap()
            })
            .collect();
        std::thread::scope(|s| {
            let threads: Vec<_> = products
                .iter()
                .map(|product| {
                    s.spawn(|| write_fixture_pointer(&instance, product, &Homes::default()))
                })
                .collect();
            for thread in threads {
                thread.join().unwrap().unwrap();
            }
        });
        assert!(products.contains(
            &load_pointer_file(&instance, &Homes::default())
                .unwrap()
                .unwrap()
        ));
        assert_eq!(
            fs::read_dir(instance.join(".agent-fabric"))
                .unwrap()
                .count(),
            2
        );
    }
    #[cfg(unix)]
    #[test]
    fn refuses_linked_directories_but_replaces_linked_files() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let instance = dir.path().join("instance");
        let outside = dir.path().join("outside");
        fs::create_dir(&instance).unwrap();
        fs::create_dir(&outside).unwrap();
        symlink(&outside, instance.join(".agent-fabric")).unwrap();
        assert!(matches!(
            write_fixture_pointer(&instance, dir.path(), &Homes::default()),
            Err(PointerError::Escape { .. })
        ));
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        fs::remove_file(instance.join(".agent-fabric")).unwrap();
        symlink(&instance, instance.join(".agent-fabric")).unwrap();
        assert!(matches!(
            write_fixture_pointer(&instance, dir.path(), &Homes::default()),
            Err(PointerError::Symlink)
        ));
        fs::remove_file(instance.join(".agent-fabric")).unwrap();
        fs::create_dir(instance.join(".agent-fabric")).unwrap();
        let external = outside.join("file");
        fs::write(&external, "keep me\n").unwrap();
        symlink(&external, instance.join(".agent-fabric/.gitignore")).unwrap();
        symlink(&external, instance.join(POINTER_RELATIVE_PATH)).unwrap();
        write_fixture_pointer(&instance, dir.path(), &Homes::default()).unwrap();
        assert_eq!(fs::read_to_string(external).unwrap(), "keep me\n");
        assert!(!instance.join(".agent-fabric/.gitignore").is_symlink());
        assert!(!instance.join(POINTER_RELATIVE_PATH).is_symlink());
    }
}

#[cfg(test)]
mod differential {
    use super::*;
    #[test]
    fn python_pointer_bytes_and_validation() {
        let dir = tempfile::tempdir().unwrap();
        let instance = dir.path().join("instance");
        let product = dir.path().join("prodé😀");
        fs::create_dir(&product).unwrap();
        let Some(expected) = crate::test_support::python(
            "scripts/lib/product_root_resolver.py",
            r#"
p,product = map(pathlib.Path,sys.argv[3:5])
m.write_pointer_file(p,product)
print(json.dumps((p/m.POINTER_RELATIVE_PATH).read_text()))
"#,
            &[&instance, &product],
        ) else {
            return;
        };
        write_fixture_pointer(&instance, &product, &Homes::default()).unwrap();
        assert_eq!(
            serde_json::json!(fs::read_to_string(instance.join(POINTER_RELATIVE_PATH)).unwrap()),
            expected
        );
    }
}

#[cfg(all(test, unix))]
mod remaining_parity {
    use super::*;
    #[test]
    #[ignore = "RP-1003: records has no extended-value JSON decoder on main"]
    fn python_extended_pointer_json() {
        let dir = tempfile::tempdir().unwrap();
        let instance = dir.path().join("instance");
        let Some(expected) = crate::test_support::python(
            "scripts/lib/product_root_resolver.py",
            r#"
p = pathlib.Path(sys.argv[3]); p.mkdir(); (p/'.agent-fabric').mkdir()
(p/m.POINTER_RELATIVE_PATH).write_text(json.dumps({'schema_version':1,'product_root':str(p),'unused':float('inf')}))
print(json.dumps(str(m.load_pointer_file(p))))
"#,
            &[&instance],
        ) else {
            return;
        };
        assert_eq!(
            serde_json::json!(load_pointer_file(&instance, &Homes::default()).unwrap()),
            expected
        );
    }
}

#[cfg(all(test, unix))]
mod native_path_tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;
    #[test]
    fn path_wire_bytes_match_python_surrogateescape_and_ascii_escaping() {
        let mut fixtures: Vec<Vec<u8>> = vec![
            b"/a/\x7f\x08\x0c\t\n\r\"\\".to_vec(),
            "/prød😀".as_bytes().to_vec(),
            b"/p/\xed\xb2\x80".to_vec(),
            b"/p/\xe2(\xa1".to_vec(),
        ];
        fixtures.extend((0..=255u8).map(|b| vec![b'/', b'p', b'/', b]));
        let Some(expected) = crate::test_support::python_values(
            r#"
print(json.dumps([json.dumps(os.fsdecode(bytes(v))) for v in values]))
"#,
            &serde_json::json!(fixtures),
        ) else {
            return;
        };
        let actual: Vec<_> = fixtures
            .iter()
            .map(|raw| {
                crate::pending::path_json(Path::new(std::ffi::OsStr::from_bytes(raw))).unwrap()
            })
            .collect();
        assert_eq!(serde_json::json!(actual), expected);
    }
    #[test]
    fn pointer_writer_matches_python_for_non_utf8_native_paths_when_supported() {
        let dir = tempfile::tempdir().unwrap();
        let product = dir
            .path()
            .join(std::ffi::OsStr::from_bytes(b"product-\xff"));
        #[cfg(target_os = "macos")]
        {
            // APFS rejects invalid UTF-8 names: EPERM on older releases,
            // EILSEQ (92) on newer ones. This is an asserted platform result,
            // not a skipped test; Python must see the same errno. The byte
            // codec oracle above always covers the surrogateescape wire form
            // without file creation.
            const EPERM: i32 = 1;
            const EILSEQ: i32 = 92;
            let error = fs::create_dir(&product).unwrap_err();
            let errno = error.raw_os_error();
            assert!(
                matches!(errno, Some(EPERM | EILSEQ)),
                "APFS invalid UTF-8 filename refusal: {errno:?}"
            );
            let Some(expected) = crate::test_support::python(
                "",
                r#"
p=os.fsencode(sys.argv[3])+b'/python-product-\xff'
try: os.mkdir(p); print(json.dumps({'created':True}))
except OSError as e: print(json.dumps({'errno':e.errno}))
"#,
                &[dir.path()],
            ) else {
                return;
            };
            assert_eq!(expected, serde_json::json!({ "errno": errno }));
        }
        #[cfg(not(target_os = "macos"))]
        fs::create_dir(&product).unwrap();
        #[cfg(not(target_os = "macos"))]
        {
            let instance = dir.path().join("instance");
            let Some(expected) = crate::test_support::python(
                "scripts/lib/product_root_resolver.py",
                r#"
p,product=map(pathlib.Path,sys.argv[3:5])
m.write_pointer_file(p,product)
print(json.dumps(list((p/m.POINTER_RELATIVE_PATH).read_bytes())))
"#,
                &[&instance, &product],
            ) else {
                return;
            };
            write_fixture_pointer(&instance, &product, &Homes::default()).unwrap();
            assert_eq!(
                serde_json::json!(fs::read(instance.join(POINTER_RELATIVE_PATH)).unwrap()),
                expected
            );
        }
    }
}

#[cfg(test)]
mod expansion_error_tests {
    use super::*;
    #[test]
    fn unknown_home_error_matches_python_instead_of_missing_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(POINTER_RELATIVE_PATH);
        fs::create_dir(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"schema_version":1,"product_root":"~provenant-no-such-user-192837/tree"}"#,
        )
        .unwrap();
        let Some(expected) = crate::test_support::python(
            "scripts/lib/product_root_resolver.py",
            r#"
try: print(json.dumps(m.load_pointer_path(pathlib.Path(sys.argv[3]))))
except RuntimeError as e: print(json.dumps({'error':str(e)}))
"#,
            &[dir.path()],
        ) else {
            return;
        };
        let actual = match load_pointer_path(dir.path(), &Homes::default()) {
            Ok(value) => serde_json::json!(value),
            Err(error) => serde_json::json!({"error":error.to_string()}),
        };
        assert_eq!(actual, expected);
    }
}
