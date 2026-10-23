//! Private path codec awaiting records, and a test-only atomic publication backend.
#[cfg(test)]
use std::fs::{self, OpenOptions};
#[cfg(test)]
use std::io::{self, Write};
use std::path::Path;
#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};

// Test fixture backend only. Production composes PointerWriter with supervisor.
#[cfg(test)]
pub(crate) fn atomic_write(destination: &Path, content: &[u8], prefix: &str) -> io::Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::other("destination has no parent"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    for attempt in 0..128 {
        let temporary = parent.join(format!(
            "{prefix}{}-{stamp}-{attempt}.tmp",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let result = (|| {
            file.write_all(content)?;
            file.sync_all()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            drop(file);
            fs::rename(&temporary, destination)
        })();
        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) if result.is_ok() => return Err(error),
            Err(_) => {}
        }
        return result;
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "cannot allocate atomic-write temporary",
    ))
}

// port: belongs in provenant-records
#[derive(Debug, thiserror::Error)]
pub(crate) enum PathEncodingError {
    #[error("product root cannot be represented as a JSON string")]
    Unsupported,
}

// port: belongs in provenant-records
/// Python json.dumps(str(path), ensure_ascii=True), including Unix surrogateescape.
/// This path-only encoder is private until records supplies its OS-string codec.
pub(crate) fn path_json(path: &Path) -> Result<String, PathEncodingError> {
    #[cfg(not(unix))]
    if path.to_str().is_none() {
        return Err(PathEncodingError::Unsupported);
    }
    let mut output = String::from("\"");
    for chunk in path.as_os_str().as_encoded_bytes().utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '"' => output.push_str("\\\""),
                '\\' => output.push_str("\\\\"),
                '\u{8}' => output.push_str("\\b"),
                '\u{c}' => output.push_str("\\f"),
                '\n' => output.push_str("\\n"),
                '\r' => output.push_str("\\r"),
                '\t' => output.push_str("\\t"),
                '\u{20}'..='\u{7e}' => output.push(c),
                _ => {
                    for unit in c.encode_utf16(&mut [0; 2]) {
                        use std::fmt::Write;
                        write!(&mut output, "\\u{unit:04x}")
                            .map_err(|_| PathEncodingError::Unsupported)?;
                    }
                }
            }
        }
        for byte in chunk.invalid() {
            use std::fmt::Write;
            write!(&mut output, "\\udc{byte:02x}").map_err(|_| PathEncodingError::Unsupported)?;
        }
    }
    output.push('"');
    Ok(output)
}
