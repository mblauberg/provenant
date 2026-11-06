//! Mach-O dependency closure for executables selected by a provider launch.
//! Exact library files are read-only runtime requirements; unresolved names
//! remain visible in the prepared facts instead of disappearing.
use std::io::Read;
use std::path::{Path, PathBuf};

const HEADER_LIMIT: u64 = 1 << 20;
const LIBRARY_LIMIT: usize = 64;
const SLICE_LIMIT: usize = 16;
const COMMAND_LIMIT: usize = 4096;
const MH_DYLIB: u32 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClosureOutcome {
    Complete,
    Incomplete,
    UnsupportedFormat,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IncompleteReason {
    LoaderRelative(String),
    Missing(String),
    Malformed(PathBuf),
    Limit { max_libraries: usize },
    OmittedCredential(PathBuf),
    OmittedProtected(PathBuf),
    OmittedUnrepresentable(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LibraryAccess {
    Grant,
    AlreadyCovered,
    OmitCredential,
    OmitProtected,
    OmitUnrepresentable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DependencyClosure {
    pub outcome: ClosureOutcome,
    pub libraries: Vec<PathBuf>,
    pub reasons: Vec<IncompleteReason>,
    /// Resolved libraries deliberately kept out of the closure by policy.
    pub omitted: Vec<PathBuf>,
    /// Dependencies already reachable under an existing lane-owned root.
    pub already_covered: Vec<PathBuf>,
}

impl DependencyClosure {
    fn unsupported() -> Self {
        Self {
            outcome: ClosureOutcome::UnsupportedFormat,
            libraries: Vec::new(),
            reasons: Vec::new(),
            omitted: Vec::new(),
            already_covered: Vec::new(),
        }
    }

    fn finish(
        libraries: Vec<PathBuf>,
        reasons: Vec<IncompleteReason>,
        omitted: Vec<PathBuf>,
        already_covered: Vec<PathBuf>,
    ) -> Self {
        Self {
            outcome: if reasons.is_empty() {
                ClosureOutcome::Complete
            } else {
                ClosureOutcome::Incomplete
            },
            libraries,
            reasons,
            omitted,
            already_covered,
        }
    }
}

/// Resolve every architecture slice recursively. `allowed` filters grants and
/// recursion (for example lane-owned binaries and credential roots).
pub(crate) fn dependency_closure(
    executable: &Path,
    access: impl Fn(&Path) -> LibraryAccess,
) -> DependencyClosure {
    let Some(root_images) = read_images(executable) else {
        if has_macho_magic(executable) {
            return DependencyClosure::finish(
                Vec::new(),
                vec![IncompleteReason::Malformed(executable.to_owned())],
                Vec::new(),
                Vec::new(),
            );
        }
        return DependencyClosure::unsupported();
    };
    let mut found = Vec::<PathBuf>::new();
    let mut pending = vec![(executable.to_owned(), root_images)];
    let mut reasons = Vec::new();
    let mut omitted = Vec::new();
    let mut already_covered = Vec::new();
    while let Some((image_path, images)) = pending.pop() {
        let mut names = Vec::new();
        for image in images {
            match load_commands(&image) {
                Some(image_names) => {
                    for name in image_names {
                        if !names.contains(&name) {
                            names.push(name);
                        }
                    }
                }
                None => add_reason(
                    &mut reasons,
                    IncompleteReason::Malformed(image_path.clone()),
                ),
            }
        }
        for name in names {
            if name.starts_with("/usr/lib/") || name.starts_with("/System/") {
                continue;
            }
            if name.starts_with('@') {
                add_reason(&mut reasons, IncompleteReason::LoaderRelative(name));
                continue;
            }
            if !Path::new(&name).is_absolute() {
                add_reason(
                    &mut reasons,
                    IncompleteReason::Malformed(image_path.clone()),
                );
                continue;
            }
            let path = match std::fs::canonicalize(&name) {
                Ok(path) => path,
                Err(_) => {
                    add_reason(&mut reasons, IncompleteReason::Missing(name));
                    continue;
                }
            };
            if found.contains(&path) || already_covered.contains(&path) || omitted.contains(&path) {
                continue;
            }
            let covered = match access(&path) {
                LibraryAccess::Grant => false,
                LibraryAccess::AlreadyCovered => true,
                LibraryAccess::OmitCredential => {
                    add_reason(
                        &mut reasons,
                        IncompleteReason::OmittedCredential(path.clone()),
                    );
                    omitted.push(path);
                    continue;
                }
                LibraryAccess::OmitProtected => {
                    add_reason(
                        &mut reasons,
                        IncompleteReason::OmittedProtected(path.clone()),
                    );
                    omitted.push(path);
                    continue;
                }
                LibraryAccess::OmitUnrepresentable => {
                    add_reason(
                        &mut reasons,
                        IncompleteReason::OmittedUnrepresentable(path.clone()),
                    );
                    omitted.push(path);
                    continue;
                }
            };
            if found.len() + already_covered.len() == LIBRARY_LIMIT {
                add_reason(
                    &mut reasons,
                    IncompleteReason::Limit {
                        max_libraries: LIBRARY_LIMIT,
                    },
                );
                continue;
            }
            let Some(images) = read_images(&path) else {
                add_reason(&mut reasons, IncompleteReason::Malformed(path));
                continue;
            };
            // Dependencies can point to frameworks or other non-dylib files;
            // only actual dylib images are recursively traversed.
            let mut is_dylib = false;
            let mut malformed = false;
            for image in &images {
                match image_header(image) {
                    Some((word, _)) if word(image, 12) == Some(MH_DYLIB) => is_dylib = true,
                    Some(_) => {}
                    None => malformed = true,
                }
            }
            if malformed {
                add_reason(&mut reasons, IncompleteReason::Malformed(path));
                continue;
            }
            if !is_dylib {
                add_reason(&mut reasons, IncompleteReason::Malformed(path));
                continue;
            }
            if covered {
                already_covered.push(path.clone());
            } else {
                found.push(path.clone());
            }
            pending.push((path, images));
        }
    }
    DependencyClosure::finish(found, reasons, omitted, already_covered)
}

fn add_reason(reasons: &mut Vec<IncompleteReason>, reason: IncompleteReason) {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

fn has_macho_magic(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut bytes = [0; 4];
    if file.read_exact(&mut bytes).is_err() {
        return false;
    }
    matches!(
        u32::from_be_bytes(bytes),
        0xfeed_face
            | 0xfeed_facf
            | 0xcefa_edfe
            | 0xcffa_edfe
            | 0xcafe_babe
            | 0xcafe_babf
            | 0xbeba_feca
            | 0xbfba_feca
    )
}

/// Every architecture slice of a thin or universal file. `None` means that
/// the file is not a supported Mach-O image; an empty/malformed universal
/// header is distinguished by returning a malformed slice for the caller.
fn read_images(image: &Path) -> Option<Vec<Vec<u8>>> {
    use std::io::{Seek, SeekFrom};
    let read = |offset: u64| -> std::io::Result<Vec<u8>> {
        let mut file = std::fs::File::open(image)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.take(HEADER_LIMIT).read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    let head = read(0).ok()?;
    let Some(offsets) = slice_offsets(&head) else {
        if image_header(&head).is_some() {
            return Some(vec![head]);
        }
        return None;
    };
    if offsets == [0] {
        return image_header(&head).is_some().then_some(vec![head]);
    }
    if offsets.is_empty() || offsets.len() > SLICE_LIMIT {
        return Some(vec![Vec::new()]);
    }
    let mut images = Vec::with_capacity(offsets.len());
    for offset in offsets {
        let bytes = read(offset).ok()?;
        if image_header(&bytes).is_none() {
            return Some(vec![Vec::new()]);
        }
        images.push(bytes);
    }
    Some(images)
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// Returns `Some([0])` for thin Mach-O, Some(offsets) for a universal file,
/// and None when bytes are not a recognized Mach-O container.
fn slice_offsets(bytes: &[u8]) -> Option<Vec<u64>> {
    let magic = be32(bytes, 0)?;
    if !matches!(magic, 0xcafe_babe | 0xcafe_babf | 0xbeba_feca | 0xbfba_feca) {
        return image_header(bytes).map(|_| vec![0]);
    }
    let word = if matches!(magic, 0xbeba_feca | 0xbfba_feca) {
        le32
    } else {
        be32
    };
    let wide = matches!(magic, 0xcafe_babf | 0xbfba_feca);
    let count = usize::try_from(word(bytes, 4)?).ok()?;
    let entry = if wide { 32 } else { 20 };
    let mut offsets = Vec::new();
    for index in 0..count.min(SLICE_LIMIT + 1) {
        let at = 8 + index * entry;
        offsets.push(if wide {
            if magic == 0xbfba_feca {
                u64::from(word(bytes, at + 12)?) << 32 | u64::from(word(bytes, at + 8)?)
            } else {
                u64::from(word(bytes, at + 8)?) << 32 | u64::from(word(bytes, at + 12)?)
            }
        } else {
            u64::from(word(bytes, at + 8)?)
        });
    }
    Some(offsets)
}

type WordReader = fn(&[u8], usize) -> Option<u32>;

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
    const LAZY: u32 = 0x0000_0020;
    const REEXPORT: u32 = 0x8000_001f;
    const UPWARD: u32 = 0x8000_0023;
    let (word, mut at) = image_header(image)?;
    let count = usize::try_from(word(image, 16)?).ok()?;
    if count > COMMAND_LIMIT {
        return None;
    }
    let mut names = Vec::new();
    for _ in 0..count {
        let command = word(image, at)?;
        let size = usize::try_from(word(image, at + 4)?).ok()?;
        if size < 8 {
            return None;
        }
        if matches!(command, LOAD | WEAK | REEXPORT | UPWARD | LAZY) {
            let offset = usize::try_from(word(image, at + 8)?).ok()?;
            if offset < 24 || offset >= size {
                return None;
            }
            let raw = image.get(at.checked_add(offset)?..at.checked_add(size)?)?;
            let end = raw.iter().position(|byte| *byte == 0).unwrap_or(raw.len());
            let name = std::str::from_utf8(&raw[..end]).ok()?;
            names.push(name.to_owned());
        }
        at = at.checked_add(size)?;
    }
    Some(names)
}

#[cfg(test)]
mod tests {
    use super::{
        ClosureOutcome, IncompleteReason, LibraryAccess, dependency_closure, load_commands,
    };
    use std::path::Path;

    fn image_with_dependencies(names: &[&str]) -> Vec<u8> {
        let mut image = vec![0; 32];
        image[..4].copy_from_slice(&0xfeed_facf_u32.to_le_bytes());
        image[12..16].copy_from_slice(&super::MH_DYLIB.to_le_bytes());
        image[16..20].copy_from_slice(&(names.len() as u32).to_le_bytes());
        for name in names {
            let size = (24 + name.len() + 1).next_multiple_of(8);
            image.extend(0xc_u32.to_le_bytes());
            image.extend((size as u32).to_le_bytes());
            image.extend(24_u32.to_le_bytes());
            image.resize(image.len() + 12, 0);
            image.extend(name.as_bytes());
            image.resize(image.len() + size - 24 - name.len(), 0);
        }
        image
    }

    #[test]
    fn typed_closure_reports_relative_missing_and_malformed_dependencies() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("tool");
        std::fs::write(
            &root,
            image_with_dependencies(&[
                "@rpath/librelative.dylib",
                "/missing/libabsent.dylib",
                "relative.dylib",
            ]),
        )
        .unwrap();
        let closure = dependency_closure(&root, |_| LibraryAccess::Grant);
        assert_eq!(closure.outcome, ClosureOutcome::Incomplete);
        assert!(closure.reasons.contains(&IncompleteReason::LoaderRelative(
            "@rpath/librelative.dylib".into()
        )));
        assert!(closure.reasons.contains(&IncompleteReason::Missing(
            "/missing/libabsent.dylib".into()
        )));
        assert!(
            closure
                .reasons
                .iter()
                .any(|reason| matches!(reason, IncompleteReason::Malformed(_)))
        );
        let mut truncated = image_with_dependencies(&["/usr/lib/libSystem.B.dylib"]);
        truncated.truncate(40);
        std::fs::write(&root, truncated).unwrap();
        let truncated = dependency_closure(&root, |_| LibraryAccess::Grant);
        assert_eq!(truncated.outcome, ClosureOutcome::Incomplete);
        assert!(
            truncated
                .reasons
                .iter()
                .any(|reason| matches!(reason, IncompleteReason::Malformed(_)))
        );
    }

    #[test]
    fn closure_deduplicates_cycles_and_reports_all_loader_relative_forms() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let a = root.join("a.dylib");
        let b = root.join("b.dylib");
        let c = root.join("c.dylib");
        std::fs::write(
            &a,
            image_with_dependencies(&[b.to_str().unwrap(), b.to_str().unwrap()]),
        )
        .unwrap();
        std::fs::write(&b, image_with_dependencies(&[c.to_str().unwrap()])).unwrap();
        std::fs::write(&c, image_with_dependencies(&[a.to_str().unwrap()])).unwrap();
        let tool = root.join("tool");
        std::fs::write(&tool, image_with_dependencies(&[a.to_str().unwrap()])).unwrap();
        let closure = dependency_closure(&tool, |_| LibraryAccess::Grant);
        assert_eq!(closure.outcome, ClosureOutcome::Complete);
        assert_eq!(closure.libraries.len(), 3);
        for library in [&a, &b, &c] {
            assert!(closure.libraries.contains(library));
        }
        std::fs::write(
            &tool,
            image_with_dependencies(&[
                "@rpath/a.dylib",
                "@loader_path/b.dylib",
                "@executable_path/c.dylib",
            ]),
        )
        .unwrap();
        let closure = dependency_closure(&tool, |_| LibraryAccess::Grant);
        assert_eq!(closure.outcome, ClosureOutcome::Incomplete);
        assert_eq!(closure.reasons.len(), 3);
        assert!(
            closure
                .reasons
                .iter()
                .all(|reason| matches!(reason, IncompleteReason::LoaderRelative(_)))
        );
    }

    #[test]
    fn protected_dependency_is_omitted_without_reading() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let protected = root.join("protected.dylib");
        std::fs::write(&protected, b"not a Mach-O image").unwrap();
        let tool = root.join("tool");
        std::fs::write(
            &tool,
            image_with_dependencies(&[protected.to_str().unwrap()]),
        )
        .unwrap();
        let closure = dependency_closure(&tool, |path| {
            if path == protected {
                LibraryAccess::OmitProtected
            } else {
                LibraryAccess::Grant
            }
        });
        assert_eq!(closure.outcome, ClosureOutcome::Incomplete);
        assert_eq!(
            closure.reasons,
            vec![IncompleteReason::OmittedProtected(protected.clone())]
        );
        assert_eq!(closure.omitted, vec![protected]);
        assert!(closure.libraries.is_empty());
    }

    #[test]
    fn unrepresentable_dependency_is_reported_in_closure_receipt()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let library = root.join("library.dylib");
        std::fs::write(&library, b"not read")?;
        let executable = root.join("tool");
        std::fs::write(
            &executable,
            image_with_dependencies(&[library.to_str().ok_or("library")?]),
        )?;
        let closure = dependency_closure(&executable, |_| LibraryAccess::OmitUnrepresentable);
        assert_eq!(closure.outcome, ClosureOutcome::Incomplete);
        assert!(closure.libraries.is_empty());
        assert_eq!(
            closure.reasons,
            [IncompleteReason::OmittedUnrepresentable(library.clone())]
        );
        assert_eq!(closure.omitted, [library]);
        let receipt = crate::PreparedToolRequirement {
            executable,
            dependency_closure: closure,
        }
        .receipt();
        assert_eq!(receipt["reason"], "unrepresentable_path");
        Ok(())
    }

    #[test]
    fn system_only_closure_is_complete_but_non_macho_is_unsupported() {
        let temp = tempfile::tempdir().unwrap();
        let image = temp.path().join("tool");
        std::fs::write(
            &image,
            image_with_dependencies(&[
                "/usr/lib/libSystem.B.dylib",
                "/System/Library/Frameworks/Foo",
            ]),
        )
        .unwrap();
        assert_eq!(
            dependency_closure(&image, |_| LibraryAccess::Grant),
            super::DependencyClosure {
                outcome: ClosureOutcome::Complete,
                libraries: Vec::new(),
                reasons: Vec::new(),
                omitted: Vec::new(),
                already_covered: Vec::new(),
            }
        );
        std::fs::write(&image, b"plain text").unwrap();
        assert_eq!(
            dependency_closure(&image, |_| LibraryAccess::Grant).outcome,
            ClosureOutcome::UnsupportedFormat
        );
    }

    #[test]
    fn recursive_closure_grants_only_library_files_and_reports_the_limit() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let leaf = root.join("leaf.dylib");
        let leaf_bytes = image_with_dependencies(&[]);
        std::fs::write(&leaf, leaf_bytes).unwrap();
        let executable = root.join("tool");
        std::fs::write(
            &executable,
            image_with_dependencies(&[leaf.to_str().unwrap()]),
        )
        .unwrap();
        let closure = dependency_closure(&executable, |_| LibraryAccess::Grant);
        assert_eq!(closure.outcome, ClosureOutcome::Complete);
        assert_eq!(closure.libraries, vec![leaf]);
        assert!(!closure.libraries.iter().any(|path| path == Path::new("/")));

        let dependencies: Vec<_> = (0..=super::LIBRARY_LIMIT)
            .map(|index| root.join(format!("lib{index}.dylib")))
            .collect();
        for library in &dependencies {
            std::fs::write(library, image_with_dependencies(&[])).unwrap();
        }
        let names: Vec<_> = dependencies
            .iter()
            .map(|path| path.to_str().unwrap())
            .collect();
        let limited = root.join("limited-tool");
        std::fs::write(&limited, image_with_dependencies(&names)).unwrap();
        let closure = dependency_closure(&limited, |_| LibraryAccess::Grant);
        assert_eq!(closure.outcome, ClosureOutcome::Incomplete);
        assert_eq!(closure.libraries.len(), super::LIBRARY_LIMIT);
        assert!(closure.reasons.contains(&IncompleteReason::Limit {
            max_libraries: super::LIBRARY_LIMIT,
        }));

        for index in 0..dependencies.len() {
            let next = dependencies
                .get(index + 1)
                .map(|path| path.to_str().unwrap());
            let names = next.into_iter().collect::<Vec<_>>();
            std::fs::write(&dependencies[index], image_with_dependencies(&names)).unwrap();
        }
        std::fs::write(
            &limited,
            image_with_dependencies(&[dependencies[0].to_str().unwrap()]),
        )
        .unwrap();
        let chained = dependency_closure(&limited, |_| LibraryAccess::Grant);
        assert_eq!(chained.outcome, ClosureOutcome::Incomplete);
        assert_eq!(chained.libraries.len(), super::LIBRARY_LIMIT);
        assert!(chained.reasons.contains(&IncompleteReason::Limit {
            max_libraries: super::LIBRARY_LIMIT
        }));
    }

    #[test]
    fn parser_lists_loader_relative_names_for_the_closure_resolver() {
        let image = image_with_dependencies(&[
            "/opt/fixture/lib/libone.dylib",
            "@loader_path/libtwo.dylib",
        ]);
        assert_eq!(
            load_commands(&image),
            Some(vec![
                "/opt/fixture/lib/libone.dylib".to_owned(),
                "@loader_path/libtwo.dylib".to_owned(),
            ])
        );
    }

    #[test]
    fn universal_dependency_closure_visits_every_architecture_slice() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let first = root.join("first.dylib");
        let second = root.join("second.dylib");
        std::fs::write(&first, image_with_dependencies(&[])).unwrap();
        std::fs::write(&second, image_with_dependencies(&[])).unwrap();
        let first_slice = image_with_dependencies(&[first.to_str().unwrap()]);
        let second_slice = image_with_dependencies(&[second.to_str().unwrap()]);
        let second_offset = 64 + first_slice.len().next_multiple_of(8);
        let mut universal = Vec::new();
        universal.extend(0xcafe_babe_u32.to_be_bytes());
        universal.extend(2_u32.to_be_bytes());
        for (cpu, offset, slice) in [
            (0x0100_0007_u32, 64_u32, &first_slice),
            (0x0100_000c, second_offset as u32, &second_slice),
        ] {
            universal.extend(cpu.to_be_bytes());
            universal.extend(0_u32.to_be_bytes());
            universal.extend(offset.to_be_bytes());
            universal.extend((slice.len() as u32).to_be_bytes());
            universal.extend(0_u32.to_be_bytes());
        }
        universal.resize(64, 0);
        universal.extend(first_slice);
        universal.resize(second_offset, 0);
        universal.extend(second_slice);
        let library = root.join("universal.dylib");
        std::fs::write(&library, universal).unwrap();
        let executable = root.join("tool");
        std::fs::write(
            &executable,
            image_with_dependencies(&[library.to_str().unwrap()]),
        )
        .unwrap();

        let closure = dependency_closure(&executable, |_| LibraryAccess::Grant);
        assert_eq!(closure.outcome, ClosureOutcome::Complete);
        let mut libraries = closure.libraries;
        libraries.sort();
        let mut expected = vec![library, first, second];
        expected.sort();
        assert_eq!(libraries, expected);
    }
}
