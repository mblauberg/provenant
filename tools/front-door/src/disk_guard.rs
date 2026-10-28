//! Lane-scoped disk policy, composed into the running guardian only.
use provenant_supervisor::DiskPressure;
use std::io;

/// Cheap, best-effort session advisory. Never walks the cache or state trees.
pub(crate) fn session_warning(
    state: &std::path::Path,
    home: &std::path::Path,
    _cwd: &std::path::Path,
) -> Option<String> {
    fn observe(state: &std::path::Path, home: &std::path::Path) -> io::Result<Option<String>> {
        use provenant_os::fs::Directory;
        fn existing(path: &std::path::Path) -> io::Result<Directory> {
            let mut path = path;
            loop {
                match Directory::open_anchor(path) {
                    Ok(directory) => return Ok(directory),
                    Err(error) if error.is_not_found() => {
                        path = path.parent().ok_or_else(|| io::Error::other(error))?
                    }
                    Err(error) => return Err(io::Error::other(error)),
                }
            }
        }
        let settings = provenant_config::GcSettings::read(state)?;
        let state_dir = existing(state)?;
        let cache_dir = existing(&home.join("Library/Caches/provenant-lane-cargo"))?;
        let (mut free, total) = state_dir.filesystem_bytes()?;
        if state_dir.metadata().map_err(io::Error::other)?.device
            != cache_dir.metadata().map_err(io::Error::other)?.device
        {
            free = free.min(cache_dir.filesystem_bytes()?.0);
        }
        let threshold = settings.free_floor_for_total(total)?.saturating_mul(2);
        Ok((free < threshold).then(|| format!("Disk pressure: {free} bytes free, warning below {threshold} bytes; run `provenant disk report` or `provenant gc`.")))
    }
    observe(state, home).ok().flatten()
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

/// One bounded pressure pass after settlement. No fetches or worktree removal.
pub(crate) fn settlement_gc(context: &provenant_run::HolderContext) -> io::Result<()> {
    let Some(launch) = &context.launch else {
        return Ok(());
    };
    collect_if_low(
        &launch.configuration_root,
        &context.roots.home,
        &launch.cwd,
        &std::env::current_exe()?,
    )
}
fn collect_if_low(
    state: &std::path::Path,
    home: &std::path::Path,
    cwd: &std::path::Path,
    program: &std::path::Path,
) -> io::Result<()> {
    use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
    let settings = provenant_config::GcSettings::read(state)?;
    let floor = settings.free_floor_bytes(state)?;
    if provenant_confinement::disk::lane_available_bytes(state, home, cwd)? >= floor {
        return Ok(());
    }
    let argv = vec![
        program.as_os_str().to_owned(),
        "--state-dir".into(),
        state.into(),
        "--project".into(),
        cwd.into(),
        "gc".into(),
        "--pressure".into(),
        "--apply".into(),
    ];
    let mut options = ExecOptions::new(&argv, home, std::time::Duration::from_secs(5));
    options.stdin = Some(b"");
    options.stdout = StreamPolicy::Discard;
    options.stderr = StreamPolicy::Capture { limit: 4096 };
    let output = SystemExecutor::new(1)
        .map_err(io::Error::other)?
        .execute(options)
        .map_err(io::Error::other)?;
    if output.timed_out {
        // Detached remnants retain their ownership proof for the daily backstop.
        return Ok(());
    }
    if output.termination != Termination::Exit(0) {
        return Err(io::Error::other(format!(
            "settlement GC failed: {}; run `provenant gc --apply --json`",
            String::from_utf8_lossy(&output.stderr.bytes)
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)] // Also included by the harness=false worktree driver.
    use super::*;
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
        std::fs::write(&config, "[gc]\nfree_floor='1B'\n")?;
        assert!(!disk.below_floor(&raw)?);
        std::fs::write(&config, "[gc]\nfree_floor='18446744073709551615B'\n")?;
        assert!(disk.below_floor(&raw)?);
        std::fs::write(&config, "[gc]\nfree_floor='1B'\n")?;
        assert!(!disk.below_floor(&raw)?);
        Ok(())
    }

    #[test]
    fn settlement_collects_below_the_shared_floor_only() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let state = temp.path().join("state");
        std::fs::create_dir(&state)?;
        let program = temp.path().join("gc-fixture");
        std::fs::write(
            &program,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$2/event.args\"\n",
        )?;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))?;
        std::fs::write(state.join("config.toml"), "[gc]\nfree_floor='1B'\n")?;
        collect_if_low(&state, temp.path(), temp.path(), &program)?;
        assert!(!state.join("event.args").exists());
        std::fs::write(
            state.join("config.toml"),
            "[gc]\nschedule=false\nfree_floor='18446744073709551615B'\n",
        )?;
        collect_if_low(&state, temp.path(), temp.path(), &program)?;
        assert!(
            state.join("event.args").exists(),
            "low-space settlement must launch bounded GC"
        );
        let args = std::fs::read_to_string(state.join("event.args"))?;
        assert!(args.contains("gc\n--pressure\n--apply\n"), "{args}");
        assert!(!args.contains("--scheduled"));
        Ok(())
    }
}
