//! Child-process construction for build jobs.
//!
//! Spawning and reaping happen inside `provenant_os`, the custody boundary for
//! process creation. The builder surface matches the `std::process::Command`
//! options that the build modules use, so their explicit environment and
//! working directory stay visible at each call site.

use provenant_os::exec::{
    ExecError, ExecOptions, Executor, MAX_CAPTURE_LIMIT, StreamPolicy, SystemExecutor, Termination,
};
use std::{
    ffi::{OsStr, OsString},
    io,
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{ExitStatus, Output},
    time::Duration,
};

/// Lifetime for a Git or prepare subprocess. The OS executor requires a finite
/// timeout; this bound matches the maximum lane lifetime so ordinary commands
/// keep effectively the same semantics as a plain `std::process::Command`.
const PROCESS_TIMEOUT: Duration = Duration::from_secs(provenant_core::MAX_LANE_RUNTIME_SECONDS);

/// A child-process specification executed through the OS custody boundary.
pub(crate) struct Command {
    program: OsString,
    args: Vec<OsString>,
    env: Option<Vec<(OsString, OsString)>>,
    cwd: Option<PathBuf>,
    timeout: Duration,
}

impl Command {
    pub(crate) fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_owned(),
            args: Vec::new(),
            env: None,
            cwd: None,
            timeout: PROCESS_TIMEOUT,
        }
    }

    pub(crate) fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    pub(crate) fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_owned()));
        self
    }

    pub(crate) fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        let (key, value) = (key.as_ref(), value.as_ref());
        let env = self
            .env
            .get_or_insert_with(|| std::env::vars_os().collect());
        env.retain(|(name, _)| name != key);
        env.push((key.to_owned(), value.to_owned()));
        self
    }

    pub(crate) fn envs<I, K, V>(&mut self, vars: I) -> &mut Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        for (key, value) in vars {
            self.env(key, value);
        }
        self
    }

    pub(crate) fn env_clear(&mut self) -> &mut Self {
        self.env = Some(Vec::new());
        self
    }

    pub(crate) fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        let key = key.as_ref();
        self.env
            .get_or_insert_with(|| std::env::vars_os().collect())
            .retain(|(name, _)| name != key);
        self
    }

    pub(crate) fn current_dir(&mut self, dir: impl AsRef<Path>) -> &mut Self {
        self.cwd = Some(dir.as_ref().to_owned());
        self
    }

    /// Run the child and collect its bounded output.
    pub(crate) fn output(&self) -> io::Result<Output> {
        let mut argv = Vec::with_capacity(self.args.len() + 1);
        argv.push(self.program.clone());
        argv.extend(self.args.iter().cloned());
        let cwd = match &self.cwd {
            Some(cwd) => cwd.clone(),
            None => std::env::current_dir()?,
        };
        let executor = SystemExecutor::new(4).map_err(exec_error)?;
        let mut options = ExecOptions::new(&argv, &cwd, self.timeout);
        if let Some(env) = self.env.as_deref() {
            options.env = Some(env);
        }
        options.stdout = StreamPolicy::Capture {
            limit: MAX_CAPTURE_LIMIT,
        };
        options.stderr = StreamPolicy::Capture {
            limit: MAX_CAPTURE_LIMIT,
        };
        let outcome = executor.execute(options).map_err(exec_error)?;
        let status = match outcome.termination {
            Termination::Exit(code) => {
                ExitStatus::from_raw((i32::try_from(code).unwrap_or(0) & 0xff) << 8)
            }
            Termination::Signal(signal) => {
                ExitStatus::from_raw(i32::try_from(signal).unwrap_or(0) & 0x7f)
            }
        };
        Ok(Output {
            status,
            stdout: outcome.stdout.bytes,
            stderr: outcome.stderr.bytes,
        })
    }

    /// Run the child and return its exit status, discarding collected output.
    pub(crate) fn status(&self) -> io::Result<ExitStatus> {
        self.output().map(|output| output.status)
    }
}

fn exec_error(error: ExecError) -> io::Error {
    io::Error::other(error.to_string())
}
