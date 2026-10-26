//! Provider custody is unavailable outside macOS. Utility execution and
//! process observation remain separate operating-system services.
use crate::census::{Census, ProcessIdentity};
use crate::exec::Termination;
use std::collections::BTreeSet;
use std::convert::Infallible;
use std::io;
use std::marker::PhantomData;
use std::os::fd::BorrowedFd;
use std::process::{ChildStderr, ChildStdin, ChildStdout};
use std::time::Duration;

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "process custody is supported only on macOS",
    )
}

pub(crate) fn ensure_supported() -> io::Result<()> {
    Err(unsupported())
}

pub(crate) struct Watch<'a>(PhantomData<BorrowedFd<'a>>);

#[derive(Default)]
pub(crate) struct ReactorEvent {
    pub(crate) supervisor_signal: bool,
}

pub(crate) struct GuardianReactor;
impl GuardianReactor {
    pub(crate) fn install() -> io::Result<Self> {
        Err(unsupported())
    }

    pub(crate) fn wait(
        &mut self,
        _watches: &[Watch<'_>],
        _timeout: Duration,
    ) -> io::Result<ReactorEvent> {
        Err(unsupported())
    }
}

pub(crate) struct AdoptedCleanup {
    pub(crate) reaped: Vec<ProcessIdentity>,
}

pub(crate) struct ProviderGroup {
    pub(crate) stdin: Option<ChildStdin>,
    pub(crate) stdout: Option<ChildStdout>,
    pub(crate) stderr: Option<ChildStderr>,
    // No custody owner can be constructed on an unsupported platform.
    never: Infallible,
}
impl ProviderGroup {
    pub(crate) fn spawn(
        _spec: super::raw_exec::SpawnSpec<'_>,
        _lease: &crate::fs::LockGuard,
        _boot: &str,
    ) -> io::Result<Self> {
        Err(unsupported())
    }

    pub(crate) fn id(&self) -> i64 {
        match self.never {}
    }

    pub(crate) fn identity(&self) -> Option<&ProcessIdentity> {
        match self.never {}
    }

    pub(crate) fn watches(&self) -> Vec<Watch<'_>> {
        match self.never {}
    }

    pub(crate) fn observe(&mut self) -> io::Result<Option<Termination>> {
        Err(unsupported())
    }

    pub(crate) fn stop(&mut self, _escalation: bool) -> io::Result<bool> {
        Err(unsupported())
    }

    pub(crate) fn stop_group(&mut self, _escalation: bool) -> io::Result<bool> {
        Err(unsupported())
    }

    pub(crate) fn record_census(&mut self, _census: &Census) {
        match self.never {}
    }

    pub(crate) fn reap_adopted(
        &mut self,
        _census: &Census,
        _spared: &BTreeSet<ProcessIdentity>,
    ) -> io::Result<AdoptedCleanup> {
        Err(unsupported())
    }

    pub(crate) fn cleanup(&mut self, _complete: bool) -> io::Result<Option<Termination>> {
        Err(unsupported())
    }

    pub(crate) fn finish_fenced(self) -> Result<Termination, Box<(Self, io::Error)>> {
        Err(Box::new((self, unsupported())))
    }
}

pub(crate) fn signal_identity(_identity: &ProcessIdentity, _escalation: bool) -> bool {
    false
}

pub(crate) fn any_pid_alive(pids: &[i64]) -> bool {
    // No absence proof is available from this backend.
    !pids.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custody_is_unsupported_without_installing_a_reactor() -> io::Result<()> {
        let error = ensure_supported()
            .err()
            .ok_or_else(|| io::Error::other("custody accepted"))?;
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(error.to_string().contains("macOS"));
        let error = GuardianReactor::install()
            .err()
            .ok_or_else(|| io::Error::other("reactor installed"))?;
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        Ok(())
    }

    #[test]
    fn custody_refuses_spawn_before_inspecting_the_command()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let directory = crate::fs::Directory::open_anchor(root.path())?;
        let lease = directory.try_lock(
            std::ffi::OsStr::new("writer.lock"),
            crate::fs::LockMode::Exclusive,
        )?;
        let error = ProviderGroup::spawn(
            super::super::raw_exec::SpawnSpec {
                argv: &[],
                env: None,
                cwd: root.path(),
                input: false,
                secret_input: None,
                capture_stdout: false,
                capture_stderr: false,
            },
            &lease,
            "boot",
        )
        .err()
        .ok_or_else(|| io::Error::other("provider spawned"))?;
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(error.to_string().contains("macOS"));
        Ok(())
    }
}
