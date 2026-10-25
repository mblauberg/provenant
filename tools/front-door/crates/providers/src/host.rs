//! Host-only command preparation, kept separate from attempt-owned storage.
use provenant_supervisor::{
    exec::{CaptureFilter, ExecError, ExecOptions, ExecOutcome, Executor},
    fs::{self, Directory, temporary_name},
};
use std::path::{Path, PathBuf};

/// Every command gets a new empty private cwd, regardless of the cwd supplied
/// by its caller. The root must be host-trusted configuration state, never a
/// child store or any part of an attempt's LaunchPaths.
pub struct HostProbeExecutor<'a> {
    executor: &'a dyn Executor,
    root: Directory,
    path: PathBuf,
}

impl<'a> HostProbeExecutor<'a> {
    pub fn new(executor: &'a dyn Executor, host_root: &Path) -> fs::Result<Self> {
        Ok(Self {
            executor,
            root: Directory::open_anchor(host_root)?,
            path: host_root.to_owned(),
        })
    }
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Executor for HostProbeExecutor<'_> {
    fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
        let create = || -> fs::Result<Scratch> {
            let name = temporary_name()?;
            // mkdir is exclusive: an existing entry is an error, never a cwd.
            self.root.mkdir(&name, 0o700)?;
            Ok(Scratch(self.path.join(name)))
        };
        let scratch = create().map_err(|error| ExecError::Io {
            operation: "create host probe directory",
            source: std::io::Error::other(error),
        })?;
        // Reborrow filters with the shorter cwd lifetime. Struct update syntax
        // would retain ExecOptions' original, invariant filter lifetime.
        self.executor.execute(ExecOptions {
            argv: options.argv,
            env: options.env,
            cwd: &scratch.0,
            stdin: options.stdin,
            secret_input: options.secret_input,
            stdout: options.stdout,
            stderr: options.stderr,
            stdout_decoder: options.stdout_decoder,
            stderr_decoder: options.stderr_decoder,
            stdout_filter: options
                .stdout_filter
                .map(|filter| filter as &mut dyn CaptureFilter),
            stderr_filter: options
                .stderr_filter
                .map(|filter| filter as &mut dyn CaptureFilter),
            cancel: options.cancel,
            timeout: options.timeout,
            terminate_grace: options.terminate_grace,
            reap_timeout: options.reap_timeout,
            stop_on_output_limit: options.stop_on_output_limit,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_host_commands_remove_their_private_cwd() {
        struct Fail;
        impl Executor for Fail {
            fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
                assert!(options.cwd.is_dir());
                std::fs::write(options.cwd.join("partial"), b"partial").unwrap();
                Err(ExecError::Busy)
            }
        }
        let host_root = tempfile::tempdir().unwrap();
        let executor = HostProbeExecutor::new(&Fail, host_root.path()).unwrap();
        let argv = ["provider".into(), "--version".into()];
        assert!(matches!(
            executor.execute(ExecOptions::new(
                &argv,
                Path::new("/child/scratch"),
                std::time::Duration::from_secs(1),
            )),
            Err(ExecError::Busy)
        ));
        assert_eq!(std::fs::read_dir(host_root.path()).unwrap().count(), 0);
    }
}
