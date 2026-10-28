//! Disk-pressure pauses are scoped to native identities below one provider.
use crate::census::{Ancestry, Census, ProcessIdentity};
use std::collections::BTreeSet;
use std::io;

#[derive(Default)]
pub(crate) struct PausedBuilds {
    identities: BTreeSet<ProcessIdentity>,
}

impl PausedBuilds {
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
                && matches!(signal(identity, libc::SIGSTOP), Ok(true))
            {
                self.identities.insert(identity.clone());
            }
        }
    }
    pub(crate) fn resume(
        &mut self,
        mut signal: impl FnMut(&ProcessIdentity, i32) -> io::Result<bool>,
    ) {
        self.identities
            .retain(|identity| signal(identity, libc::SIGCONT).is_err());
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
