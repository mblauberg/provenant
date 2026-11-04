//! Non-system shared libraries a pinned executable loads, such as Homebrew
//! Git's `pcre2` and `gettext`. A deny-default macOS profile must grant their
//! exact canonical files, or dyld aborts the child before it runs.
use std::io::Read;
use std::path::{Path, PathBuf};

const HEADER_LIMIT: u64 = 1 << 20;
const LIBRARY_LIMIT: usize = 64;
const MH_DYLIB: u32 = 6;

/// Canonical, recursively resolved library files outside the system roots.
/// Anything that is not an absolute-path Mach-O dependency is ignored.
pub(crate) fn linked_libraries(executable: &Path) -> Vec<PathBuf> {
    linked_libraries_filtered(executable, |_| true)
}

/// Resolve regular Mach-O dynamic libraries, recursing only through kept files.
/// The caller excludes lane-owned paths and credential stores.
pub(crate) fn linked_dynamic_libraries(
    executable: &Path,
    allowed: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    linked_libraries_filtered(executable, |path| {
        path.is_file()
            && allowed(path)
            && read_image(path).is_some_and(|image| {
                image_header(&image).is_some_and(|(word, _)| word(&image, 12) == Some(MH_DYLIB))
            })
    })
}

fn linked_libraries_filtered(executable: &Path, allowed: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
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
            if found.len() < LIBRARY_LIMIT && allowed(&path) && !found.contains(&path) {
                found.push(path.clone());
                pending.push(path);
            }
        }
    }
    found
}

fn read_image(image: &Path) -> Option<Vec<u8>> {
    use std::io::{Seek, SeekFrom};
    let read = |offset: u64| -> std::io::Result<Vec<u8>> {
        let mut file = std::fs::File::open(image)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.take(HEADER_LIMIT).read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    let head = read(0).ok()?;
    match slice_offset(&head)? {
        0 => Some(head),
        offset => read(offset).ok(),
    }
}

fn dependencies(image: &Path) -> Vec<String> {
    read_image(image)
        .and_then(|image| load_commands(&image))
        .unwrap_or_default()
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
    if !matches!(magic, 0xcafe_babe | 0xcafe_babf | 0xbeba_feca | 0xbfba_feca) {
        return Some(0);
    }
    let word = if matches!(magic, 0xbeba_feca | 0xbfba_feca) {
        le32
    } else {
        be32
    };
    let wide = matches!(magic, 0xcafe_babf | 0xbfba_feca);
    let entry = if wide { 32 } else { 20 };
    for index in 0..usize::try_from(word(bytes, 4)?).ok()?.min(16) {
        let at = 8 + index * entry;
        if word(bytes, at)? != host {
            continue;
        }
        return Some(if wide {
            if magic == 0xbfba_feca {
                u64::from(word(bytes, at + 12)?) << 32 | u64::from(word(bytes, at + 8)?)
            } else {
                u64::from(word(bytes, at + 8)?) << 32 | u64::from(word(bytes, at + 12)?)
            }
        } else {
            u64::from(word(bytes, at + 8)?)
        });
    }
    None
}

type WordReader = fn(&[u8], usize) -> Option<u32>;

/// Byte order and header length for 32- and 64-bit Mach-O images.
fn image_header(image: &[u8]) -> Option<(WordReader, usize)> {
    let header: (WordReader, usize) = match be32(image, 0)? {
        0xfeed_face => (be32, 28),
        0xfeed_facf => (be32, 32),
        0xcefa_edfe => (le32, 28),
        0xcffa_edfe => (le32, 32),
        _ => return None,
    };
    (image.len() >= header.1).then_some(header)
}

fn load_commands(image: &[u8]) -> Option<Vec<String>> {
    const LOAD: u32 = 0xc;
    const WEAK: u32 = 0x8000_0018;
    const REEXPORT: u32 = 0x8000_001f;
    const UPWARD: u32 = 0x8000_0023;
    let (word, mut at) = image_header(image)?;
    let count = usize::try_from(word(image, 16)?).ok()?;
    let mut names = Vec::new();
    for _ in 0..count.min(4096) {
        let command = word(image, at)?;
        let size = usize::try_from(word(image, at + 4)?).ok()?;
        if size < 8 {
            return None;
        }
        if matches!(command, LOAD | WEAK | REEXPORT | UPWARD) {
            let offset = usize::try_from(word(image, at + 8)?).ok()?;
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
    fn dynamic_library_headers_accept_thin_and_universal_images()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let executable = root.join("rg");
        let library = root.join("library");
        let name = library.to_str().unwrap().as_bytes();
        let size = (24 + name.len() + 1).next_multiple_of(8);
        let mut rg = vec![0; 32];
        rg[..4].copy_from_slice(&0xfeed_facf_u32.to_le_bytes());
        rg[16..20].copy_from_slice(&1_u32.to_le_bytes());
        rg.extend(0xc_u32.to_le_bytes());
        rg.extend((size as u32).to_le_bytes());
        rg.extend(24_u32.to_le_bytes());
        rg.resize(56, 0);
        rg.extend(name);
        rg.resize(32 + size, 0);
        std::fs::write(&executable, rg)?;
        for big_endian in [false, true] {
            let word = |value: u32| {
                if big_endian {
                    value.to_be_bytes()
                } else {
                    value.to_le_bytes()
                }
            };
            for wide in [false, true] {
                let mut thin = vec![0; if wide { 32 } else { 28 }];
                thin[..4].copy_from_slice(&word(if wide { 0xfeed_facf } else { 0xfeed_face }));
                thin[12..16].copy_from_slice(&word(6));
                for universal in [false, true] {
                    let mut image = Vec::new();
                    if universal {
                        image.extend(word(if wide { 0xcafe_babf } else { 0xcafe_babe }));
                        image.extend(word(1));
                        image.extend(word(if cfg!(target_arch = "aarch64") {
                            0x0100_000c
                        } else {
                            0x0100_0007
                        }));
                        image.extend(word(0));
                        if wide {
                            image.extend(if big_endian {
                                64_u64.to_be_bytes()
                            } else {
                                64_u64.to_le_bytes()
                            });
                        } else {
                            image.extend(word(64));
                        }
                        image.resize(64, 0);
                    }
                    image.extend(&thin);
                    std::fs::write(&library, image)?;
                    assert_eq!(
                        super::linked_dynamic_libraries(&executable, |_| true),
                        std::slice::from_ref(&library)
                    );
                }
            }
        }
        for bytes in [
            vec![],
            b"plain text".to_vec(),
            0xfeed_facf_u32.to_le_bytes().to_vec(),
        ] {
            std::fs::write(&library, bytes)?;
            assert!(super::linked_dynamic_libraries(&executable, |_| true).is_empty());
        }
        Ok(())
    }

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
