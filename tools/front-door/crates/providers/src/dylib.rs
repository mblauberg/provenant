//! Non-system shared libraries a pinned executable loads, such as Homebrew
//! Git's `pcre2` and `gettext`. A deny-default macOS profile must grant their
//! exact canonical files, or dyld aborts the child before it runs.
use std::io::Read;
use std::path::{Path, PathBuf};

const HEADER_LIMIT: u64 = 1 << 20;
const LIBRARY_LIMIT: usize = 64;

/// Canonical, recursively resolved library files outside the system roots.
/// Anything that is not an absolute-path Mach-O dependency is ignored.
pub(crate) fn linked_libraries(executable: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let mut pending = vec![executable.to_owned()];
    while let Some(image) = pending.pop() {
        for name in dependencies(&image) {
            if name.starts_with("/usr/lib/") || name.starts_with("/System/") {
                continue;
            }
            let Ok(path) = std::fs::canonicalize(&name) else {
                continue;
            };
            if found.len() < LIBRARY_LIMIT && !found.contains(&path) {
                found.push(path.clone());
                pending.push(path);
            }
        }
    }
    found
}

fn dependencies(image: &Path) -> Vec<String> {
    use std::io::{Seek, SeekFrom};
    let read = |offset: u64| -> std::io::Result<Vec<u8>> {
        let mut file = std::fs::File::open(image)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.take(HEADER_LIMIT).read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    let Ok(head) = read(0) else {
        return Vec::new();
    };
    let image = match slice_offset(&head) {
        Some(0) => head,
        Some(offset) => read(offset).unwrap_or_default(),
        None => return Vec::new(),
    };
    load_commands(&image).unwrap_or_default()
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// Offset of the host architecture's image in a thin or universal file.
fn slice_offset(bytes: &[u8]) -> Option<u64> {
    const ARM64: u32 = 0x0100_000c;
    const X86_64: u32 = 0x0100_0007;
    let host = if cfg!(target_arch = "aarch64") {
        ARM64
    } else {
        X86_64
    };
    let magic = be32(bytes, 0)?;
    if !matches!(magic, 0xcafe_babe | 0xcafe_babf) {
        return Some(0);
    }
    let wide = magic == 0xcafe_babf;
    let entry = if wide { 32 } else { 20 };
    for index in 0..usize::try_from(be32(bytes, 4)?).ok()?.min(16) {
        let at = 8 + index * entry;
        if be32(bytes, at)? != host {
            continue;
        }
        return Some(if wide {
            u64::from(be32(bytes, at + 8)?) << 32 | u64::from(be32(bytes, at + 12)?)
        } else {
            u64::from(be32(bytes, at + 8)?)
        });
    }
    None
}

fn load_commands(image: &[u8]) -> Option<Vec<String>> {
    const LOAD: u32 = 0xc;
    const WEAK: u32 = 0x8000_0018;
    const REEXPORT: u32 = 0x8000_001f;
    const UPWARD: u32 = 0x8000_0023;
    if le32(image, 0)? != 0xfeed_facf {
        return None;
    }
    let count = usize::try_from(le32(image, 16)?).ok()?;
    let mut at = 32;
    let mut names = Vec::new();
    for _ in 0..count.min(4096) {
        let command = le32(image, at)?;
        let size = usize::try_from(le32(image, at + 4)?).ok()?;
        if size < 8 {
            return None;
        }
        if matches!(command, LOAD | WEAK | REEXPORT | UPWARD) {
            let offset = usize::try_from(le32(image, at + 8)?).ok()?;
            let raw = image.get(at + offset..at + size)?;
            let end = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
            if let Ok(name) = std::str::from_utf8(&raw[..end])
                && name.starts_with('/')
            {
                names.push(name.to_owned());
            }
        }
        at = at.checked_add(size)?;
    }
    Some(names)
}

#[cfg(test)]
mod tests {
    use super::load_commands;

    #[test]
    fn mach_o_absolute_dependencies_are_listed() {
        let mut image = vec![0; 32];
        image[..4].copy_from_slice(&0xfeed_facf_u32.to_le_bytes());
        image[16..20].copy_from_slice(&2_u32.to_le_bytes());
        for name in ["/opt/fixture/lib/libone.dylib", "@rpath/libtwo.dylib"] {
            let mut command = Vec::new();
            command.extend(0xc_u32.to_le_bytes());
            let size = (24 + name.len() + 1).next_multiple_of(8);
            command.extend(u32::try_from(size).unwrap_or(0).to_le_bytes());
            command.extend(24_u32.to_le_bytes());
            command.resize(24, 0);
            command.extend(name.as_bytes());
            command.resize(size, 0);
            image.extend(command);
        }
        assert_eq!(
            load_commands(&image),
            Some(vec!["/opt/fixture/lib/libone.dylib".to_owned()])
        );
    }
}
