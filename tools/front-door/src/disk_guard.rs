//! Lane-scoped disk policy, composed into the running guardian only.
use provenant_supervisor::DiskPressure;
use std::{cell::RefCell, io, path::PathBuf};

#[derive(Default)]
pub(crate) struct LaneDisk {
    policy: RefCell<Option<Policy>>,
}
struct Policy {
    floor: u64,
    volumes: [PathBuf; 3],
}
impl DiskPressure for LaneDisk {
    fn below_floor(&self, raw: &[u8]) -> io::Result<bool> {
        let mut policy = self.policy.borrow_mut();
        if policy.is_none() {
            let context: provenant_run::HolderContext =
                serde_json::from_slice(raw).map_err(io::Error::other)?;
            let state = context
                .launch
                .as_ref()
                .map_or(&context.roots.state_root, |launch| {
                    &launch.configuration_root
                });
            let cwd = context
                .launch
                .as_ref()
                .map_or(&context.roots.checkout, |launch| &launch.cwd);
            let floor = provenant_config::GcSettings::read(state)?.free_floor_bytes(state)?;
            *policy = Some(Policy {
                floor,
                volumes: [
                    state.clone(),
                    cwd.clone(),
                    context
                        .roots
                        .home
                        .join("Library/Caches/provenant-lane-cargo"),
                ],
            });
        }
        let policy = policy
            .as_ref()
            .ok_or_else(|| io::Error::other("missing disk policy"))?;
        let mut free = u64::MAX;
        for volume in &policy.volumes {
            free = free.min(provenant_confinement::disk::available_bytes_at_existing(
                volume,
            )?);
        }
        Ok(free < policy.floor)
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
    let cache = home.join("Library/Caches/provenant-lane-cargo");
    let mut low = false;
    for volume in [state, cwd, cache.as_path()] {
        low |= provenant_confinement::disk::available_bytes_at_existing(volume)? < floor;
    }
    if !low {
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
