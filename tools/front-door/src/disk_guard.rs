//! Lane-scoped disk policy, composed into the running guardian only.
use provenant_supervisor::DiskPressure;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
struct Sample {
    name: &'static str,
    free: u64,
    total: u64,
}

fn warning(
    settings: &provenant_config::GcSettings,
    samples: &[Sample],
) -> io::Result<Option<String>> {
    let Some(state) = samples.first() else {
        return Ok(None);
    };
    let floor = settings.free_floor_for_total(state.total)?;
    for sample in samples {
        let threshold = floor.saturating_mul(2).min(sample.total / 2);
        if sample.free < threshold {
            return Ok(Some(format!(
                "Disk pressure: {} bytes free on {}, warning below {} bytes; run `provenant disk report` or `provenant gc`.",
                sample.free, sample.name, threshold
            )));
        }
    }
    Ok(None)
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
        let cache = home.join("Library/Caches/provenant-lane-cargo");
        let mut devices = Vec::new();
        let mut samples = Vec::new();
        for (name, path) in [
            ("state", state),
            ("checkout", cwd),
            ("lane Cargo cache", cache.as_path()),
        ] {
            let (path, device) = existing(path)?;
            if devices.contains(&device) {
                continue;
            }
            let (free, total) = provenant_os::fs::filesystem_bytes_at(&path)?;
            devices.push(device);
            samples.push(Sample { name, free, total });
        }
        warning(&settings, &samples)
    }
    observe(state, home, cwd).ok().flatten()
}

#[derive(Default)]
pub(crate) struct LaneDisk {}
impl DiskPressure for LaneDisk {
    fn below_floor(&self, raw: &[u8]) -> io::Result<bool> {
        let context: provenant_run::HolderContext =
            serde_json::from_slice(raw).map_err(io::Error::other)?;
        let (state, cwd) = context.disk_roots();
        // Live policy: changes take effect on the next admission, guardian or
        // settlement observation, including percentage floor recalculation.
        let floor = provenant_config::GcSettings::read(state)?.free_floor_bytes(state)?;
        let free =
            provenant_confinement::disk::lane_available_bytes(state, &context.roots.home, cwd)?;
        Ok(free < floor)
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)] // Also included by the harness=false worktree driver.
    use super::*;
    #[test]
    fn session_threshold_and_distinct_volumes() -> Result<(), Box<dyn std::error::Error>> {
        let settings = provenant_config::GcSettings {
            free_floor: Some("100B".into()),
            ..Default::default()
        };
        let state = Sample {
            name: "state",
            free: 150,
            total: 1000,
        };
        let checkout = Sample {
            name: "checkout",
            free: 900,
            total: 1000,
        };
        assert!(warning(&settings, &[state, checkout])?.is_some());
        assert!(warning(&settings, &[Sample { free: 200, ..state }, checkout])?.is_none());
        let small = Sample {
            free: 60,
            total: 100,
            ..state
        };
        assert!(warning(&settings, &[small])?.is_none());
        assert!(warning(&settings, &[Sample { free: 49, ..small }])?.is_some());
        let text = warning(
            &settings,
            &[
                Sample { free: 900, ..state },
                Sample {
                    free: 100,
                    ..checkout
                },
            ],
        )?
        .ok_or("missing warning")?;
        assert!(text.contains("checkout"));
        assert!(text.len() <= 160);
        let invalid = provenant_config::GcSettings {
            free_floor: Some("bad".into()),
            ..Default::default()
        };
        assert!(warning(&invalid, &[state]).is_err());
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
    #[test]
    fn lane_pause_rereads_the_floor_each_observation() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let context = provenant_run::HolderContext {
            schema_version: 1,
            roots: provenant_run::RootSeed {
                home: temp.path().into(),
                checkout: temp.path().into(),
                state_root: temp.path().into(),
            },
            project_id: provenant_core::ProjectId::from_bytes([1; 16]),
            actor_id: provenant_core::ActorId::from_bytes([2; 16]),
            run_id: provenant_core::RunId::from_bytes([3; 16]),
            attempt_id: provenant_core::AttemptId::from_bytes([4; 16]),
            host_delivery: None,
            launch: None,
            sealed: None,
        };
        let raw = context.encode()?;
        let disk = LaneDisk::default();
        let config = temp.path().join("config.toml");
        std::fs::write(&config, "schema_version=1\n[gc]\nfree_floor='1B'\n")?;
        assert!(!disk.below_floor(&raw)?);
        std::fs::write(
            &config,
            "schema_version=1\n[gc]\nfree_floor='18446744073709551615B'\n",
        )?;
        assert!(disk.below_floor(&raw)?);
        std::fs::write(&config, "schema_version=1\n[gc]\nfree_floor='1B'\n")?;
        assert!(!disk.below_floor(&raw)?);
        Ok(())
    }
}
