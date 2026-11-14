//! Build a bounded, link-free snapshot of user skills before confinement.
use crate::SetupStep;
use provenant_os::fs::{Directory, FileKind};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// An entry omitted while capturing a skill tree. Paths are source-relative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillWarning {
    pub path: PathBuf,
    pub reason: &'static str,
}

/// Prepared content is consumed by the launch setup and its receipt together.
#[derive(Debug, Default)]
pub struct StagedUserSkills {
    pub root: Option<PathBuf>,
    pub source: Option<String>,
    pub file_count: usize,
    pub warnings: Vec<SkillWarning>,
    pub(crate) setup: Vec<SetupStep>,
}

impl StagedUserSkills {
    pub fn setup(&self) -> &[SetupStep] {
        &self.setup
    }

    pub fn discard(&mut self) {
        self.root = None;
        self.file_count = 0;
        self.setup.clear();
    }
}

struct Capture<'a> {
    home: &'a Path,
    root: &'a Path,
    source: &'a Path,
    budget: u64,
    entries_left: usize,
    ancestors: BTreeSet<(u64, u64)>,
    #[cfg(test)]
    before_link_read: Option<&'a dyn Fn(&Path)>,
    result: StagedUserSkills,
}

impl Capture<'_> {
    fn warn(&mut self, path: &Path, reason: &'static str) {
        self.result.warnings.push(SkillWarning {
            path: path.into(),
            reason,
        });
    }

    fn directory(&mut self, dir: Directory, relative: &Path) {
        let Ok(metadata) = dir.metadata() else {
            self.warn(relative, "changed_during_copy");
            return;
        };
        let Ok(current) = dir.current_path() else {
            self.warn(relative, "changed_during_copy");
            return;
        };
        if !self.ancestors.insert((metadata.device, metadata.inode)) {
            self.warn(relative, "cycle");
            return;
        }
        if self.ancestors.len() > 128 {
            self.warn(relative, "depth_limit");
            self.ancestors.remove(&(metadata.device, metadata.inode));
            return;
        }
        if !relative.as_os_str().is_empty() {
            self.result
                .setup
                .push(SetupStep::CreateDir(self.root.join(relative)));
        }
        let entries = match dir.entries() {
            Ok(entries) => entries,
            Err(_) => {
                self.warn(relative, "changed_during_copy");
                self.ancestors.remove(&(metadata.device, metadata.inode));
                return;
            }
        };
        for entry in entries {
            let Ok(name) = entry else {
                self.warn(relative, "changed_during_copy");
                continue;
            };
            let path = relative.join(&name);
            if self.entries_left == 0 {
                self.warn(&path, "entry_limit");
                break;
            }
            self.entries_left -= 1;
            if dir.current_path().ok().as_deref() != Some(current.as_path()) {
                self.warn(&path, "changed_during_copy");
                continue;
            }
            let child_path = current.join(&name);
            if !child_path.starts_with(self.source)
                && provenant_confinement::refuses_lane_root(&child_path, true, Some(self.home))
            {
                self.warn(&path, "credential_root");
                continue;
            }
            let Ok(before) = dir.metadata_at(&name, false) else {
                self.warn(&path, "changed_during_copy");
                continue;
            };
            match before.kind {
                FileKind::Regular => match dir.read_stable(&name, self.budget) {
                    Ok(bytes) => {
                        self.budget -= bytes.len() as u64;
                        self.result.file_count += 1;
                        self.result.setup.push(SetupStep::WriteFile {
                            path: self.root.join(&path),
                            bytes,
                        });
                    }
                    Err(provenant_os::fs::FsError::TooLarge { .. }) => {
                        self.warn(&path, "size_limit")
                    }
                    Err(_) => self.warn(&path, "changed_during_copy"),
                },
                FileKind::Directory => match dir.open_verified_directory(&name) {
                    Ok(child) => self.directory(child, &path),
                    Err(_) => self.warn(&path, "changed_during_copy"),
                },
                FileKind::Symlink => self.link(&dir, &name, &path, before),
                FileKind::Other => self.warn(&path, "unsupported_type"),
            }
        }
        self.ancestors.remove(&(metadata.device, metadata.inode));
    }

    fn link(
        &mut self,
        parent: &Directory,
        name: &std::ffi::OsStr,
        relative: &Path,
        before: provenant_os::fs::Metadata,
    ) {
        let Ok(target_before) = parent.metadata_at(name, true) else {
            self.warn(relative, "changed_during_copy");
            return;
        };
        let target = parent
            .current_path()
            .ok()
            .and_then(|dir| std::fs::canonicalize(dir.join(name)).ok());
        let Some(target) = target else {
            self.warn(relative, "changed_during_copy");
            return;
        };
        if !target.starts_with(self.source)
            && provenant_confinement::refuses_lane_root(&target, true, Some(self.home))
        {
            self.warn(relative, "credential_root");
            return;
        }
        let unchanged = || {
            parent
                .metadata_at(name, false)
                .is_ok_and(|after| before.unchanged(&after))
                && parent
                    .metadata_at(name, true)
                    .is_ok_and(|after| target_before.unchanged(&after))
        };
        if !unchanged() {
            self.warn(relative, "changed_during_copy");
            return;
        }
        if target.is_dir() {
            let Ok(child) = Directory::open_anchor(&target) else {
                self.warn(relative, "changed_during_copy");
                return;
            };
            if child.current_path().ok().as_deref() != Some(target.as_path())
                || !child
                    .metadata()
                    .is_ok_and(|opened| target_before.unchanged(&opened))
                || !unchanged()
            {
                self.warn(relative, "changed_during_copy");
                return;
            }
            self.directory(child, relative);
        } else {
            let Some(parent_path) = target.parent() else {
                self.warn(relative, "changed_during_copy");
                return;
            };
            let Some(file_name) = target.file_name() else {
                self.warn(relative, "changed_during_copy");
                return;
            };
            let Ok(directory) = Directory::open_anchor(parent_path) else {
                self.warn(relative, "changed_during_copy");
                return;
            };
            if directory.current_path().ok().as_deref() != Some(parent_path) || !unchanged() {
                self.warn(relative, "changed_during_copy");
                return;
            }
            if !directory
                .metadata_at(file_name, false)
                .is_ok_and(|opened| target_before.unchanged(&opened))
            {
                self.warn(relative, "changed_during_copy");
                return;
            }
            #[cfg(test)]
            if let Some(hook) = self.before_link_read {
                hook(relative);
            }
            match directory.read_stable(file_name, self.budget) {
                Ok(bytes) if unchanged() => {
                    self.budget -= bytes.len() as u64;
                    self.result.file_count += 1;
                    self.result.setup.push(SetupStep::WriteFile {
                        path: self.root.join(relative),
                        bytes,
                    });
                }
                Err(provenant_os::fs::FsError::TooLarge { .. }) => {
                    self.warn(relative, "size_limit")
                }
                _ => self.warn(relative, "changed_during_copy"),
            }
        }
    }
}

/// Capture a selected source; its ordinary entries are allowed even where the
/// selected skills directory sits beneath an adapter credential directory.
/// Link targets are checked independently against the credential guard.
pub(crate) fn capture(
    source: &Path,
    destination: &Path,
    home: &Path,
    max_bytes: u64,
) -> StagedUserSkills {
    let skipped = |reason| StagedUserSkills {
        warnings: vec![SkillWarning {
            path: PathBuf::new(),
            reason,
        }],
        ..StagedUserSkills::default()
    };
    let Ok(canonical) = std::fs::canonicalize(source) else {
        return if std::fs::symlink_metadata(source).is_ok() {
            skipped("changed_during_copy")
        } else {
            StagedUserSkills::default()
        };
    };
    if canonical != source && provenant_confinement::refuses_lane_root(&canonical, true, Some(home))
    {
        return skipped("credential_root");
    }
    let Ok(dir) = Directory::open_anchor(&canonical) else {
        return skipped("changed_during_copy");
    };
    if dir.current_path().ok().as_deref() != Some(canonical.as_path()) {
        return skipped("changed_during_copy");
    }
    let mut capture = Capture {
        home,
        root: destination,
        source: &canonical,
        budget: max_bytes,
        entries_left: 100_000,
        ancestors: BTreeSet::new(),
        #[cfg(test)]
        before_link_read: None,
        result: StagedUserSkills::default(),
    };
    capture.directory(dir, Path::new(""));
    capture.result.root = Some(destination.to_owned());
    capture.result.source = source
        .strip_prefix(home)
        .ok()
        .map(|path| format!("~/{}", path.display()));
    capture.result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn stages_links_without_granting_host_paths_or_credentials()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().canonicalize()?;
        let source = home.join(".gemini/skills");
        let external = home.join("checkout/skill");
        let secret = home.join(".codex/auth.json");
        std::fs::create_dir_all(&source)?;
        std::fs::create_dir_all(&external)?;
        std::fs::create_dir_all(secret.parent().unwrap())?;
        std::fs::write(source.join("SKILL.md"), b"local")?;
        std::fs::write(external.join("SKILL.md"), b"external")?;
        std::fs::write(&secret, b"secret-sentinel")?;
        let installed = home
            .join(provenant_core::DEFAULT_STATE_SUFFIX)
            .join("releases/example/skill");
        std::fs::create_dir_all(&installed)?;
        std::fs::write(installed.join("SKILL.md"), b"installed")?;
        symlink(source.join("SKILL.md"), source.join("internal"))?;
        symlink(&installed, source.join("installed"))?;
        symlink(&external, source.join("linked"))?;
        symlink(&secret, source.join("credential"))?;
        symlink(&source, external.join("cycle"))?;
        let destination = home.join("attempt/skills");
        let staged = capture(&source, &destination, &home, 1024);
        assert_eq!(staged.file_count, 4);
        for (path, expected) in [
            ("internal", b"local".as_slice()),
            ("installed/SKILL.md", b"installed"),
            ("linked/SKILL.md", b"external"),
        ] {
            assert!(staged.setup.iter().any(|step| matches!(step,
                SetupStep::WriteFile { path: staged_path, bytes } if staged_path == &destination.join(path) && bytes == expected)));
        }
        assert!(
            staged
                .warnings
                .iter()
                .any(|w| w.path == Path::new("credential") && w.reason == "credential_root")
        );
        assert!(
            staged
                .warnings
                .iter()
                .any(|w| w.path == Path::new("linked/cycle") && w.reason == "cycle")
        );
        for step in &staged.setup {
            match step {
                SetupStep::WriteFile { path, bytes } => {
                    assert!(path.starts_with(&destination));
                    assert!(
                        !bytes
                            .windows(b"secret-sentinel".len())
                            .any(|window| window == b"secret-sentinel")
                    );
                }
                SetupStep::CreateDir(path) => assert!(path.starts_with(&destination)),
                _ => panic!("capture must not create links"),
            }
        }
        Ok(())
    }

    #[test]
    fn oversized_file_is_skipped_with_warning() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("skills");
        std::fs::create_dir_all(&source)?;
        std::fs::write(source.join("large"), b"12345")?;
        let staged = capture(&source, &temp.path().join("staged"), temp.path(), 4);
        assert_eq!(staged.file_count, 0);
        assert!(
            staged
                .warnings
                .iter()
                .any(|w| w.path == Path::new("large") && w.reason == "size_limit")
        );
        Ok(())
    }

    #[test]
    fn skill_locations_declare_installed_release_provenance() {
        for spec in provenant_records::ADAPTER_SPECS
            .iter()
            .filter(|spec| spec.skills.is_some())
        {
            let releases = Path::new(provenant_core::DEFAULT_STATE_SUFFIX).join("releases");
            assert!(
                spec.skill_install_roots
                    .iter()
                    .any(|root| Path::new(root) == releases)
            );
        }
    }

    #[test]
    fn linked_source_inside_credential_root_is_skipped() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().canonicalize()?;
        let credential_root = home.join(".gemini/private");
        std::fs::create_dir_all(&credential_root)?;
        std::fs::write(credential_root.join("secret"), b"sentinel")?;
        let source = home.join("skills");
        symlink(&credential_root, &source)?;
        let staged = capture(&source, &home.join("stage"), &home, 1024);
        assert!(staged.root.is_none());
        assert_eq!(staged.file_count, 0);
        assert!(
            staged
                .warnings
                .iter()
                .any(|warning| warning.reason == "credential_root")
        );
        Ok(())
    }

    #[test]
    fn replaced_link_target_is_skipped_before_copy() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let home = temp.path().canonicalize()?;
        let source = home.join("skills");
        let destination = home.join("stage");
        let target = home.join("resource");
        let secret = home.join(".codex/auth.json");
        std::fs::create_dir_all(&source)?;
        std::fs::create_dir_all(secret.parent().ok_or("missing parent")?)?;
        std::fs::write(&target, b"safe")?;
        std::fs::write(&secret, b"secret-sentinel")?;
        symlink(&target, source.join("resource"))?;
        let hook = |_: &Path| {
            std::fs::rename(&target, home.join("old-resource")).unwrap();
            symlink(&secret, &target).unwrap();
        };
        let mut capture = Capture {
            home: &home,
            root: &destination,
            source: &source,
            budget: 1024,
            entries_left: 100,
            ancestors: BTreeSet::new(),
            before_link_read: Some(&hook),
            result: StagedUserSkills::default(),
        };
        capture.directory(Directory::open_anchor(&source)?, Path::new(""));
        assert_eq!(capture.result.file_count, 0);
        assert!(
            capture
                .result
                .warnings
                .iter()
                .any(|warning| warning.path == Path::new("resource")
                    && warning.reason == "changed_during_copy")
        );
        assert!(capture.result.setup().iter().all(|step| !matches!(step,
            SetupStep::WriteFile { bytes, .. } if bytes.windows(15).any(|part| part == b"secret-sentinel"))));
        Ok(())
    }
}
