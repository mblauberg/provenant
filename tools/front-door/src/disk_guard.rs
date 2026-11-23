//! Session disk advisory.
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
struct Sample {
    name: &'static str,
    free: u64,
}

fn warning(limits: &provenant_config::DiskLimits, samples: &[Sample]) -> Option<String> {
    for sample in samples {
        if limits.level(sample.free) != provenant_config::DiskLevel::Ready {
            return Some(format!(
                "Disk pressure on {}: {}.",
                sample.name,
                limits.detail(sample.free)
            ));
        }
    }
    None
}

fn existing(path: &Path) -> io::Result<(PathBuf, u64)> {
    let mut current = path;
    loop {
        match std::fs::metadata(current) {
            Ok(metadata) => return Ok((current.to_owned(), metadata.dev())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                current = current.parent().ok_or(error)?;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Cheap, best-effort session advisory. Never walks the cache or state trees.
pub(crate) fn session_warning(state: &Path, home: &Path, cwd: &Path) -> Option<String> {
    fn observe(state: &Path, home: &Path, cwd: &Path) -> io::Result<Option<String>> {
        // Resolve an existing state alias before reading the bounded config.
        let config_state = match std::fs::canonicalize(state) {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => state.to_owned(),
            Err(error) => return Err(error),
        };
        let settings = provenant_config::GcSettings::read(&config_state)?;
        let limits = settings.disk_limits(&config_state)?;
        let cache = home.join("Library/Caches/provenant-lane-cargo");
        let mut devices = Vec::new();
        let mut samples = Vec::new();
        for (name, path) in [
            ("state", state),
            ("checkout", cwd),
            ("legacy lane Cargo cache", cache.as_path()),
        ] {
            let (path, device) = existing(path)?;
            if devices.contains(&device) {
                continue;
            }
            let (free, _) = provenant_os::fs::filesystem_bytes_at(&path)?;
            devices.push(device);
            samples.push(Sample { name, free });
        }
        Ok(warning(&limits, &samples))
    }
    observe(state, home, cwd).ok().flatten()
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)] // Also included by the harness=false worktree driver.
    use super::*;
    #[test]
    fn session_threshold_and_distinct_volumes() -> Result<(), Box<dyn std::error::Error>> {
        let limits = provenant_config::DiskLimits {
            free_floor: 100,
            free_floor_source: provenant_records::diagnostic::SettingSource::Configuration,
            critical_free: 50,
            critical_free_source: provenant_records::diagnostic::SettingSource::ShippedDefault,
        };
        let state = Sample {
            name: "state",
            free: 101,
        };
        let checkout = Sample {
            name: "checkout",
            free: 101,
        };
        assert!(warning(&limits, &[state, checkout]).is_none());
        for (free, expected_level) in [
            (100, provenant_config::DiskLevel::Ready),
            (99, provenant_config::DiskLevel::Warning),
            (50, provenant_config::DiskLevel::Warning),
            (49, provenant_config::DiskLevel::Critical),
        ] {
            let sample = Sample {
                name: "checkout",
                free,
            };
            assert_eq!(limits.level(free), expected_level);
            assert_eq!(
                warning(&limits, &[sample]).is_some(),
                expected_level != provenant_config::DiskLevel::Ready
            );
        }
        let text = warning(
            &limits,
            &[Sample {
                name: "checkout",
                free: 49,
            }],
        )
        .ok_or("missing warning")?;
        assert!(text.contains("checkout"));
        assert!(text.contains("gc.critical_free"));
        let invalid = provenant_config::GcSettings {
            free_floor: Some("bad".into()),
            ..Default::default()
        };
        let temp = tempfile::tempdir()?;
        assert!(invalid.disk_limits(temp.path()).is_err());
        Ok(())
    }

    #[test]
    fn session_sampling_follows_aliases_and_skips_failures()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        std::fs::create_dir(&state)?;
        let alias = temp.path().join("alias");
        symlink(&state, &alias)?;
        assert_eq!(existing(&alias)?.1, existing(&state)?.1);
        assert_eq!(existing(&alias.join("missing"))?.1, existing(&state)?.1);
        std::fs::write(state.join("config.toml"), "schema_version=1\n[gc")?;
        assert!(session_warning(&alias, temp.path(), temp.path()).is_none());
        Ok(())
    }
}
