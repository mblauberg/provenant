//! Disk-pressure pauses are scoped to native identities below one provider.
use crate::census::{Ancestry, Census, ProcessIdentity};
use crate::custody::Scope;
use provenant_os::fs::{Directory, PublishMode};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io;

pub(crate) const PAUSE_FILE: &str = "disk-paused.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PauseJournal {
    pub schema_version: u64,
    pub scope: Scope,
    pub guardian: ProcessIdentity,
    pub identities: BTreeSet<ProcessIdentity>,
}

impl PauseJournal {
    fn persist(&self, directory: &Directory) -> io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        if bytes.len() as u64 > crate::custody::RECORD_LIMIT {
            return Err(io::Error::other("disk pause journal too large"));
        }
        directory
            .atomic_write(PAUSE_FILE.as_ref(), ".pause-", &bytes, PublishMode::Replace)
            .map_err(io::Error::other)?
            .require_durable()
            .map_err(io::Error::other)
    }
}

#[derive(Default)]
pub(crate) struct PausedBuilds {
    identities: BTreeSet<ProcessIdentity>,
    journal: Option<(Directory, Scope, ProcessIdentity)>,
}

impl PausedBuilds {
    pub(crate) fn bind(
        &mut self,
        directory: &Directory,
        scope: Scope,
        guardian: &ProcessIdentity,
    ) -> io::Result<()> {
        if self.journal.is_none() {
            self.journal = Some((
                directory.try_clone().map_err(io::Error::other)?,
                scope,
                guardian.clone(),
            ));
        }
        Ok(())
    }

    fn persist(&self) -> io::Result<()> {
        if let Some((directory, scope, guardian)) = &self.journal {
            PauseJournal {
                schema_version: 1,
                scope: *scope,
                guardian: guardian.clone(),
                identities: self.identities.clone(),
            }
            .persist(directory)?;
        }
        Ok(())
    }

    pub(crate) fn pause(
        &mut self,
        census: &Census,
        provider: &ProcessIdentity,
        spared: &BTreeSet<ProcessIdentity>,
        mut signal: impl FnMut(&ProcessIdentity, i32) -> io::Result<bool>,
    ) {
        let eligible = |identity: &ProcessIdentity| {
            identity != provider
                && census.ancestry(identity, provider) == Ancestry::Yes
                && spared.iter().all(|owner| {
                    identity != owner && census.ancestry(identity, owner) == Ancestry::No
                })
        };
        let roots: Vec<_> = census
            .facts()
            .filter(|fact| !fact.zombie && eligible(&fact.identity))
            .filter(|fact| fact.command.as_deref().is_some_and(is_build))
            .map(|fact| &fact.identity)
            .collect();
        for fact in census.facts() {
            let identity = &fact.identity;
            if fact.zombie || !eligible(identity) || self.identities.contains(identity) {
                continue;
            }
            if roots
                .iter()
                .any(|root| identity == *root || census.ancestry(identity, root) == Ancestry::Yes)
            {
                // Persist authority before STOP. A crash on either side of the
                // syscall then leaves a safe, identity-checked CONT to replay.
                self.identities.insert(identity.clone());
                if self.persist().is_err() {
                    self.identities.remove(identity);
                    continue;
                }
                if !matches!(signal(identity, libc::SIGSTOP), Ok(true)) {
                    self.identities.remove(identity);
                }
            }
        }
    }
    pub(crate) fn resume(
        &mut self,
        mut signal: impl FnMut(&ProcessIdentity, i32) -> io::Result<bool>,
    ) {
        self.identities
            .retain(|identity| signal(identity, libc::SIGCONT).is_err());
        if self.identities.is_empty() {
            if let Some((directory, _, _)) = &self.journal
                && match directory.unlink(PAUSE_FILE.as_ref()) {
                    Ok(()) => true,
                    Err(error) => error.is_not_found(),
                }
                && directory.sync().is_ok()
            {
                self.journal = None;
            }
        } else {
            let _ = self.persist();
        }
    }
}

fn is_build(command: &str) -> bool {
    matches!(
        command,
        "cargo"
            | "rustc"
            | "ld"
            | "ld64"
            | "clang"
            | "clang++"
            | "cc1"
            | "cc1plus"
            | "sccache"
            | "swift-frontend"
            | "swift-build"
            | "xcodebuild"
            | "turbo"
            | "esbuild"
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::census::ProcessFact;

    fn fact(pid: i64, parent: i64, command: &str) -> ProcessFact {
        ProcessFact {
            identity: ProcessIdentity {
                boot: "boot".into(),
                pid,
                native_start: pid as u64,
                group_id: 1,
                session_id: 1,
            },
            parent_pid: parent,
            zombie: false,
            cpu_micros: None,
            command: Some(command.into()),
        }
    }

    fn fixture() -> (Census, ProcessIdentity, BTreeSet<ProcessIdentity>) {
        let provider = fact(10, 0, "provider");
        let nested = fact(30, 10, "nested-provider");
        let protected = fact(40, 10, "protected");
        let census = Census::from_observations(
            [
                provider.clone(),
                fact(20, 10, "cargo"),
                fact(21, 20, "build-script"),
                nested.clone(),
                fact(31, 30, "rustc"),
                protected.clone(),
                fact(41, 40, "clang"),
                fact(50, 0, "cargo"),
                fact(60, 10, "cargo-helper"),
            ],
            [],
            [],
            true,
            false,
            100,
        )
        .unwrap();
        (
            census,
            provider.identity,
            BTreeSet::from([nested.identity, protected.identity]),
        )
    }

    #[test]
    fn pauses_only_owned_build_trees_and_preserves_provider_and_nested_lanes() {
        let (census, provider, spared) = fixture();
        let mut paused = PausedBuilds::default();
        let mut signals = Vec::new();
        paused.pause(&census, &provider, &spared, |identity, signal| {
            signals.push((identity.pid, signal));
            Ok(true)
        });
        assert_eq!(signals, [(20, libc::SIGSTOP), (21, libc::SIGSTOP)]);
        paused.pause(&census, &provider, &spared, |identity, signal| {
            signals.push((identity.pid, signal));
            Ok(true)
        });
        assert_eq!(
            signals.len(),
            2,
            "a paused build is not signalled every census"
        );
        paused.resume(|identity, signal| {
            signals.push((identity.pid, signal));
            Ok(true)
        });
        assert_eq!(&signals[2..], [(20, libc::SIGCONT), (21, libc::SIGCONT)]);
        assert!(paused.identities.is_empty());
    }

    #[test]
    fn every_stop_has_a_durable_identity_and_resume_clears_the_journal()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let directory = Directory::open_anchor(root.path())?;
        let (census, provider, spared) = fixture();
        let scope = Scope {
            run: provenant_core::RunId::from_bytes([1; 16]),
            attempt: provenant_core::AttemptId::from_bytes([2; 16]),
            generation: 1,
        };
        let mut paused = PausedBuilds::default();
        paused.bind(&directory, scope, &provider)?;
        let mut recorded = Vec::new();
        paused.pause(&census, &provider, &spared, |identity, signal| {
            let bytes = std::fs::read(root.path().join(PAUSE_FILE))?;
            let journal: PauseJournal = serde_json::from_slice(&bytes)?;
            assert_eq!(signal, libc::SIGSTOP);
            assert_eq!(journal.scope, scope);
            assert!(journal.identities.contains(identity));
            recorded.push(identity.pid);
            Ok(true)
        });
        assert_eq!(recorded, [20, 21]);
        paused.resume(|_, signal| {
            assert_eq!(signal, libc::SIGCONT);
            Ok(true)
        });
        assert!(!root.path().join(PAUSE_FILE).exists());
        Ok(())
    }

    #[test]
    fn failed_journal_publication_never_stops_a_build() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let directory = Directory::open_anchor(root.path())?;
        std::fs::create_dir(root.path().join(PAUSE_FILE))?;
        let (census, provider, spared) = fixture();
        let scope = Scope {
            run: provenant_core::RunId::from_bytes([1; 16]),
            attempt: provenant_core::AttemptId::from_bytes([2; 16]),
            generation: 1,
        };
        let mut paused = PausedBuilds::default();
        paused.bind(&directory, scope, &provider)?;
        let mut signalled = false;
        paused.pause(&census, &provider, &spared, |_, _| {
            signalled = true;
            Ok(true)
        });
        assert!(!signalled);
        Ok(())
    }

    #[test]
    fn failed_pause_is_retried_and_failed_resume_retains_exact_identity() {
        let (census, provider, spared) = fixture();
        let mut paused = PausedBuilds::default();
        paused.pause(&census, &provider, &spared, |_, _| {
            Err(io::Error::other("denied"))
        });
        assert!(paused.identities.is_empty());
        paused.pause(&census, &provider, &spared, |_, _| Ok(true));
        assert_eq!(paused.identities.len(), 2);
        let before = paused.identities.clone();
        paused.resume(|_, _| Err(io::Error::other("temporarily unreadable")));
        assert_eq!(paused.identities, before);
        paused.resume(|_, _| Ok(false));
        assert!(
            paused.identities.is_empty(),
            "absent or reused identities need no retry"
        );
    }

    #[test]
    fn unknown_ancestry_and_provider_build_name_do_not_grant_pause_authority() {
        let provider = fact(10, 0, "cargo");
        let incomplete = Census::from_observations(
            [provider.clone(), fact(20, 999, "rustc")],
            [],
            [],
            false,
            false,
            100,
        )
        .unwrap();
        let mut paused = PausedBuilds::default();
        let mut called = false;
        paused.pause(&incomplete, &provider.identity, &BTreeSet::new(), |_, _| {
            called = true;
            Ok(true)
        });
        assert!(!called);
    }
}
