//! Recovery of an attempt whose guardian is gone but whose published exit still
//! names unresolved descendants.
//!
//! Nothing here signals by bare PID. Candidates are only what the attempt
//! recorded or marked: the PIDs and identities its exit named, the provider and
//! descendants its reservation recorded, and live processes carrying the
//! attempt's own sandbox mark. A candidate is ended only when it is proven to belong to the
//! attempt: its recorded identity (boot, PID and native start time) still
//! matches the live process, or the platform reports it inside the attempt's
//! sandbox. Process group, session, ancestry and working directory grant
//! nothing. A PID that is gone, or now names an unrelated process, needs
//! nothing. A named PID that cannot be proven is left alone and named in the
//! settled exit.
use super::*;

/// What recovery did with each process the unresolved exit named.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OrphanReport {
    /// Proven attempt processes that were alive and are now gone.
    pub stopped: Vec<ProcessIdentity>,
    /// Named PIDs that could not be proven to belong to the attempt. They were
    /// never signalled.
    pub unproven: Vec<i64>,
    /// Proven processes that outlived TERM and KILL. While any remain the
    /// exit stays unresolved and the fence holds.
    pub survivors: Vec<ProcessIdentity>,
}

/// The two platform facts recovery needs beyond observation.
pub(crate) trait OrphanProbe {
    /// Whether this live PID carries the attempt's own sandbox mark.
    fn in_attempt_sandbox(&self, pid: i64, scope: &Scope) -> bool;
    /// Deliver TERM (or KILL) to exactly this identity; false when nothing was sent.
    fn signal(&self, identity: &ProcessIdentity, escalation: bool) -> bool;
}

pub(super) struct SystemProbe;
impl OrphanProbe for SystemProbe {
    #[cfg(target_os = "macos")]
    fn in_attempt_sandbox(&self, pid: i64, scope: &Scope) -> bool {
        let Ok(name) = std::ffi::CString::new(scope.attempt.sandbox_lookup_name()) else {
            return false;
        };
        i32::try_from(pid).is_ok_and(|pid| {
            matches!(
                crate::process_os::census::darwin_in_attempt_sandbox(pid, &name),
                Ok(true)
            )
        })
    }
    #[cfg(not(target_os = "macos"))]
    fn in_attempt_sandbox(&self, _: i64, _: &Scope) -> bool {
        false
    }
    fn signal(&self, identity: &ProcessIdentity, escalation: bool) -> bool {
        crate::process_os::provider::signal_identity(identity, escalation)
    }
}

/// The part of the unresolved exit recovery reads.
#[derive(Deserialize)]
struct UnresolvedExit {
    scope: Scope,
    unresolved_cleanup: bool,
    #[serde(default)]
    unresolved_pids: Vec<i64>,
    #[serde(default)]
    unresolved_identities: Vec<ProcessIdentity>,
}

enum Standing {
    /// Absent, a zombie, or the PID now names another process.
    Gone,
    Proven(ProcessIdentity),
    Unproven,
}

fn alive(observer: &dyn ProcessObserver, identity: &ProcessIdentity) -> bool {
    match observer.observe(identity.pid) {
        ProcessObservation::Absent => false,
        ProcessObservation::Present(fact) => fact.identity == *identity && !fact.zombie,
        ProcessObservation::Unknown(_) => true,
    }
}

impl Recovery {
    /// With the guardian gone, end the descendants its unresolved exit named and
    /// settle that exit. Needs writer exclusion (held by `self`) and the
    /// attempt directory holding `process-exit.json`.
    ///
    /// Settles the exit and reservation unless a proven process survives KILL
    /// for `patience` per round. Unproven PIDs do not block settlement: they
    /// are named in the exit as `LeftoverNotProven` for the user to act on.
    pub fn end_orphans(&mut self, attempt: &Directory, patience: Duration) -> Result<OrphanReport> {
        self.end_orphans_with(attempt, &SystemProcessObserver, &SystemProbe, patience)
    }

    // Orphan escalation and reaping use bounded real-time windows.
    #[allow(clippy::disallowed_methods)]
    pub(crate) fn end_orphans_with(
        &mut self,
        attempt: &Directory,
        observer: &dyn ProcessObserver,
        probe: &dyn OrphanProbe,
        patience: Duration,
    ) -> Result<OrphanReport> {
        let bytes = attempt.read_bounded(OsStr::new("process-exit.json"), EXIT_LIMIT)?;
        let mut document: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| CustodyError::InvalidRecord)?;
        let exit: UnresolvedExit =
            serde_json::from_value(document.clone()).map_err(|_| CustodyError::InvalidRecord)?;
        if exit.scope != self.reservation.scope || !exit.unresolved_cleanup {
            return Err(CustodyError::Fenced);
        }
        let report = self.end_candidates(&exit, observer, probe, patience)?;
        if !report.survivors.is_empty() {
            return Ok(report);
        }

        // Resolve the reservation first, so a crash in between leaves only the
        // conservative unresolved exit visible, as the guardian orders it.
        let mut resolved = self.reservation.clone();
        resolved.unresolved_cleanup = false;
        resolved.census_incomplete = false;
        resolved.omitted_descendants = 0;
        resolved.descendants.clear();
        resolved.validate()?;
        Publication::require_durable(write_record(
            &self.directory,
            "reservation.json",
            &resolved,
            PublishMode::Replace,
        )?)?;
        self.reservation = resolved;

        let evidence = if report.unproven.is_empty() {
            SettlementEvidence::OrphansEnded
        } else {
            SettlementEvidence::LeftoverNotProven
        };
        let fields = document
            .as_object_mut()
            .ok_or(CustodyError::InvalidRecord)?;
        fields.insert("unresolved_cleanup".into(), false.into());
        fields.insert("unresolved_pids".into(), report.unproven.clone().into());
        fields.insert("unresolved_identities".into(), Vec::<u8>::new().into());
        fields.insert(
            "settlement_evidence".into(),
            serde_json::to_value(evidence).map_err(|_| CustodyError::InvalidRecord)?,
        );
        let settled = bounded_exit(document, report.stopped.len())?;
        Publication::require_durable(attempt.atomic_write(
            OsStr::new("process-exit.json"),
            ".exit-",
            &settled,
            PublishMode::Replace,
        )?)?;
        Ok(report)
    }

    /// End a dead lane's recorded process tree when the guardian was killed
    /// before it could publish a process exit. Without the published identity
    /// its absence cannot be proven, so this is fenced in that case. Writer
    /// exclusion and guardian absence are rechecked before any signal, and
    /// only recorded identities or sandbox-marked escapees are candidates.
    /// No exit is invented: the caller settles the attempt through its normal
    /// loss proof once the tree is gone.
    pub fn end_recorded_orphans(
        &mut self,
        observer: &dyn ProcessObserver,
        patience: Duration,
    ) -> Result<OrphanReport> {
        self.end_recorded_orphans_with(observer, &SystemProbe, patience)
    }

    #[allow(clippy::disallowed_methods)]
    pub(crate) fn end_recorded_orphans_with(
        &mut self,
        observer: &dyn ProcessObserver,
        probe: &dyn OrphanProbe,
        patience: Duration,
    ) -> Result<OrphanReport> {
        if self.reservation.guardian.is_none() {
            return Err(CustodyError::Fenced);
        }
        let exit = UnresolvedExit {
            scope: self.reservation.scope,
            unresolved_cleanup: false,
            unresolved_pids: Vec::new(),
            unresolved_identities: Vec::new(),
        };
        self.end_candidates(&exit, observer, probe, patience)
    }

    /// The proof-gated signal loop shared by an unresolved exit and a dead
    /// lane whose guardian never published one.
    #[allow(clippy::disallowed_methods)]
    fn end_candidates(
        &self,
        exit: &UnresolvedExit,
        observer: &dyn ProcessObserver,
        probe: &dyn OrphanProbe,
        patience: Duration,
    ) -> Result<OrphanReport> {
        self.reservation.validate()?;
        let same_boot =
            observer.boot_identity().map_err(|_| CustodyError::Fenced)? == self.reservation.boot;
        if same_boot && self.guardian_present(observer)? {
            return Err(CustodyError::Fenced);
        }
        let mut report = OrphanReport::default();
        // A new boot ended every process of the old one.
        let mut proven = if same_boot {
            self.candidates(exit, observer, probe, &mut report.unproven)
        } else {
            Vec::new()
        };
        let mut live = proven.clone();
        for escalation in [false, true] {
            live.retain(|identity| alive(observer, identity));
            if live.is_empty() {
                break;
            }
            for identity in &live {
                probe.signal(identity, escalation);
            }
            let deadline = Instant::now() + patience;
            loop {
                live.retain(|identity| alive(observer, identity));
                if live.is_empty() || Instant::now() >= deadline {
                    break;
                }
                provenant_os::sys::wait_pause(Duration::from_millis(20));
            }
        }
        proven.retain(|identity| !live.contains(identity));
        report.stopped = proven;
        report.survivors = live;
        Ok(report)
    }

    fn guardian_present(&self, observer: &dyn ProcessObserver) -> Result<bool> {
        let Some(guardian) = &self.reservation.guardian else {
            return Ok(false);
        };
        match observer.observe(guardian.pid) {
            ProcessObservation::Present(fact) => Ok(fact.identity == *guardian && !fact.zombie),
            ProcessObservation::Absent => Ok(false),
            ProcessObservation::Unknown(_) => Err(CustodyError::Fenced),
        }
    }

    /// Proven attempt processes among the recorded and sandbox-marked
    /// candidates. Named PIDs that cannot be proven go to `unproven`. The
    /// census and sandbox probe run only during recovery.
    fn candidates(
        &self,
        exit: &UnresolvedExit,
        observer: &dyn ProcessObserver,
        probe: &dyn OrphanProbe,
        unproven: &mut Vec<i64>,
    ) -> Vec<ProcessIdentity> {
        let mut named = std::collections::BTreeSet::new();
        named.extend(exit.unresolved_pids.iter().copied());
        named.extend(
            exit.unresolved_identities
                .iter()
                .chain(self.reservation.provider.iter())
                .chain(self.reservation.descendants.iter())
                .filter(|identity| identity.boot == self.reservation.boot)
                .map(|identity| identity.pid),
        );
        let mut proven = Vec::new();
        for &pid in &named {
            match self.stand(pid, exit, observer, probe) {
                Standing::Gone => {}
                Standing::Proven(identity) => proven.push(identity),
                Standing::Unproven if exit.unresolved_pids.contains(&pid) => unproven.push(pid),
                Standing::Unproven => {}
            }
        }
        // Escapees the guardian never recorded still carry the sandbox mark.
        if let Ok(census) = observer.census(crate::census::MAX_CENSUS_ENTRIES) {
            for fact in census.facts() {
                if fact.zombie
                    || named.contains(&fact.identity.pid)
                    || self.reservation.guardian.as_ref() == Some(&fact.identity)
                {
                    continue;
                }
                if probe.in_attempt_sandbox(fact.identity.pid, &self.reservation.scope) {
                    proven.push(fact.identity.clone());
                }
            }
        }
        proven
    }

    fn stand(
        &self,
        pid: i64,
        exit: &UnresolvedExit,
        observer: &dyn ProcessObserver,
        probe: &dyn OrphanProbe,
    ) -> Standing {
        let recorded = exit
            .unresolved_identities
            .iter()
            .chain(self.reservation.provider.iter())
            .chain(self.reservation.descendants.iter())
            .find(|identity| identity.pid == pid && identity.boot == self.reservation.boot);
        match (observer.observe(pid), recorded) {
            (ProcessObservation::Absent, _) => Standing::Gone,
            (ProcessObservation::Unknown(_), _) => Standing::Unproven,
            (ProcessObservation::Present(fact), _) if fact.zombie => Standing::Gone,
            (ProcessObservation::Present(fact), Some(identity)) => {
                if fact.identity == *identity {
                    Standing::Proven(fact.identity)
                } else {
                    // The recorded process is gone and the PID was reused.
                    Standing::Gone
                }
            }
            (ProcessObservation::Present(fact), None) => {
                if probe.in_attempt_sandbox(pid, &self.reservation.scope) {
                    Standing::Proven(fact.identity)
                } else {
                    Standing::Unproven
                }
            }
        }
    }
}

/// The settled exit with one summary warning for `stopped` processes, kept
/// within the warning cap and `EXIT_LIMIT` the guardian's `bounded_bytes`
/// applies; an entry that does not fit is counted in `omitted_warnings`.
fn bounded_exit(mut document: serde_json::Value, stopped: usize) -> Result<Vec<u8>> {
    let fields = document
        .as_object_mut()
        .ok_or(CustodyError::InvalidRecord)?;
    let omit = |fields: &mut serde_json::Map<String, serde_json::Value>| {
        let omitted = fields
            .get("omitted_warnings")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        fields.insert("omitted_warnings".into(), omitted.saturating_add(1).into());
    };
    let mut added = false;
    if stopped > 0 {
        let warnings = fields
            .entry("warnings")
            .or_insert_with(|| serde_json::Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or(CustodyError::InvalidRecord)?;
        if warnings.len() < 24 {
            warnings.push(format!("killed {stopped} surviving processes").into());
            added = true;
        } else {
            omit(fields);
        }
    }
    let bytes = serde_json::to_vec(&document).map_err(|_| CustodyError::InvalidRecord)?;
    if bytes.len() as u64 <= EXIT_LIMIT {
        return Ok(bytes);
    }
    if added {
        let fields = document
            .as_object_mut()
            .ok_or(CustodyError::InvalidRecord)?;
        if let Some(warnings) = fields
            .get_mut("warnings")
            .and_then(serde_json::Value::as_array_mut)
        {
            warnings.pop();
        }
        omit(fields);
        let bytes = serde_json::to_vec(&document).map_err(|_| CustodyError::InvalidRecord)?;
        if bytes.len() as u64 <= EXIT_LIMIT {
            return Ok(bytes);
        }
    }
    Err(CustodyError::InvalidRecord)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::census::{Census, CensusError, ProcessFact};
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::error::Error;

    type TestResult = std::result::Result<(), Box<dyn Error>>;
    const BOOT: &str = "fixture-recorded-boot-v1";

    fn scope() -> Scope {
        Scope {
            attempt: provenant_core::AttemptId::from_bytes([2; 16]),
            generation: 1,
        }
    }

    fn identity(pid: i64, start: u64) -> ProcessIdentity {
        ProcessIdentity {
            boot: BOOT.into(),
            pid,
            native_start: start,
            group_id: pid,
            session_id: pid,
        }
    }

    /// A fake host. TERM ends every process except those marked stubborn.
    /// `continuing` processes carry another attempt's sandbox mark; `in_worktree`
    /// processes merely have their working directory in the lane worktree,
    /// which recovery never reads.
    #[derive(Default)]
    struct World {
        live: RefCell<BTreeMap<i64, ProcessIdentity>>,
        sandboxed: Vec<i64>,
        continuing: Vec<i64>,
        in_worktree: Vec<i64>,
        stubborn: Vec<i64>,
        signals: RefCell<Vec<(i64, bool)>>,
    }

    fn successor() -> Scope {
        Scope {
            attempt: provenant_core::AttemptId::from_bytes([3; 16]),
            generation: 1,
        }
    }
    impl ProcessObserver for World {
        fn boot_identity(&self) -> std::result::Result<String, CensusError> {
            Ok(BOOT.into())
        }
        fn observe(&self, pid: i64) -> ProcessObservation {
            match self.live.borrow().get(&pid) {
                Some(identity) => ProcessObservation::Present(ProcessFact {
                    identity: identity.clone(),
                    parent_pid: 1,
                    zombie: false,
                    cpu_micros: None,
                    command: None,
                }),
                None => ProcessObservation::Absent,
            }
        }
        fn census(&self, bound: usize) -> std::result::Result<Census, CensusError> {
            let facts: Vec<_> = self
                .live
                .borrow()
                .values()
                .map(|identity| ProcessFact {
                    identity: identity.clone(),
                    parent_pid: 1,
                    zombie: false,
                    cpu_micros: None,
                    command: None,
                })
                .collect();
            Census::from_observations(facts, [], [], true, false, bound)
        }
    }
    impl OrphanProbe for World {
        fn in_attempt_sandbox(&self, pid: i64, asked: &Scope) -> bool {
            if *asked == successor() {
                return self.continuing.contains(&pid);
            }
            *asked == scope() && self.sandboxed.contains(&pid)
        }
        fn signal(&self, target: &ProcessIdentity, escalation: bool) -> bool {
            if self.live.borrow().get(&target.pid) != Some(target) {
                return false;
            }
            self.signals.borrow_mut().push((target.pid, escalation));
            if escalation || !self.stubborn.contains(&target.pid) {
                self.live.borrow_mut().remove(&target.pid);
            }
            true
        }
    }

    struct Fixture {
        _root: tempfile::TempDir,
        recovery: Recovery,
        attempt: Directory,
        path: std::path::PathBuf,
    }

    /// A spawned attempt whose guardian (pid 100) is gone and whose published
    /// exit is unresolved for `named` PIDs, `recorded` of which carry identities.
    fn fixture(
        named: &[i64],
        recorded: &[ProcessIdentity],
    ) -> std::result::Result<Fixture, Box<dyn Error>> {
        let root = tempfile::tempdir()?;
        let clock = provenant_os::clock::SystemClock::new();
        let lease = crate::fixture::acquire_writer(
            Directory::open_anchor(root.path())?,
            scope(),
            WaitBudget::cli(clock.mono_now(), 1).map_err(|_| CustodyError::InvalidRecord)?,
            &CancelToken::new(),
            &clock,
        )?;
        let mut recovery = lease.into_recovery()?;
        let reservation = &mut recovery.reservation;
        reservation.launch_state = LaunchState::Spawned;
        reservation.attempt_device = Some(1);
        reservation.attempt_inode = Some(1);
        reservation.guardian = Some(identity(100, 1));
        reservation.provider = Some(ProcessIdentity {
            session_id: 100,
            ..identity(101, 2)
        });
        reservation.unresolved_cleanup = true;
        reservation.census_incomplete = true;
        reservation.descendants = recorded.to_vec();
        let scope = reservation.scope;
        let exit = serde_json::json!({
            "schema_version": 1,
            "scope": scope,
            "termination": null,
            "unresolved_cleanup": true,
            "unresolved_pids": named,
            "settlement_evidence": "unresolved",
            "warnings": ["kept"],
        });
        let path = root.path().to_path_buf();
        std::fs::write(path.join("process-exit.json"), serde_json::to_vec(&exit)?)?;
        let attempt = Directory::open_anchor(root.path())?;
        Ok(Fixture {
            _root: root,
            recovery,
            attempt,
            path,
        })
    }

    /// A spawned attempt whose guardian was killed before it could publish an
    /// exit. Only the durable reservation names the tree it started.
    fn recorded_fixture(
        recorded: &[ProcessIdentity],
    ) -> std::result::Result<Fixture, Box<dyn Error>> {
        let root = tempfile::tempdir()?;
        let clock = provenant_os::clock::SystemClock::new();
        let lease = crate::fixture::acquire_writer(
            Directory::open_anchor(root.path())?,
            scope(),
            WaitBudget::cli(clock.mono_now(), 1).map_err(|_| CustodyError::InvalidRecord)?,
            &CancelToken::new(),
            &clock,
        )?;
        let mut recovery = lease.into_recovery()?;
        let reservation = &mut recovery.reservation;
        reservation.launch_state = LaunchState::Spawned;
        reservation.attempt_device = Some(1);
        reservation.attempt_inode = Some(1);
        reservation.guardian = Some(identity(100, 1));
        reservation.provider = Some(ProcessIdentity {
            session_id: 100,
            ..identity(101, 2)
        });
        reservation.census_incomplete = false;
        reservation.descendants = recorded.to_vec();
        let path = root.path().to_path_buf();
        let attempt = Directory::open_anchor(root.path())?;
        Ok(Fixture {
            _root: root,
            recovery,
            attempt,
            path,
        })
    }

    fn settled(f: &Fixture) -> std::result::Result<serde_json::Value, Box<dyn Error>> {
        Ok(serde_json::from_slice(&std::fs::read(
            f.path.join("process-exit.json"),
        )?)?)
    }

    fn run(f: &mut Fixture, world: &World) -> Result<OrphanReport> {
        f.recovery
            .end_orphans_with(&f.attempt, world, world, Duration::ZERO)
    }

    #[test]
    fn a_proven_orphan_is_stopped_and_the_exit_settles() -> TestResult {
        let tail = identity(84322, 7);
        let mut f = fixture(&[84322], std::slice::from_ref(&tail))?;
        let world = World::default();
        world.live.borrow_mut().insert(84322, tail.clone());
        let report = run(&mut f, &world)?;
        assert_eq!(report.stopped, vec![tail]);
        assert!(report.unproven.is_empty() && report.survivors.is_empty());
        assert_eq!(world.signals.borrow().as_slice(), [(84322, false)]);
        let exit = settled(&f)?;
        assert_eq!(exit["unresolved_cleanup"], false);
        assert_eq!(exit["settlement_evidence"], "orphans_ended");
        assert_eq!(exit["warnings"][0], "kept");
        assert!(!f.recovery.reservation.unresolved_cleanup);
        assert_eq!(
            f.recovery.observe_with(&world),
            crate::custody::RecoveryState::Absent
        );
        Ok(())
    }

    #[test]
    fn an_orphan_that_ignores_term_is_killed() -> TestResult {
        let tail = identity(9, 3);
        let mut f = fixture(&[9], std::slice::from_ref(&tail))?;
        let world = World {
            stubborn: vec![9],
            ..World::default()
        };
        world.live.borrow_mut().insert(9, tail.clone());
        assert_eq!(run(&mut f, &world)?.stopped, vec![tail]);
        assert_eq!(world.signals.borrow().as_slice(), [(9, false), (9, true)]);
        Ok(())
    }

    #[test]
    fn a_reused_pid_is_left_alone_and_the_exit_settles() -> TestResult {
        let mut f = fixture(&[84322], &[identity(84322, 7)])?;
        let world = World::default();
        let stranger = identity(84322, 99);
        world.live.borrow_mut().insert(84322, stranger);
        let report = run(&mut f, &world)?;
        assert_eq!(report, OrphanReport::default());
        assert!(world.signals.borrow().is_empty());
        assert_eq!(settled(&f)?["settlement_evidence"], "orphans_ended");
        Ok(())
    }

    #[test]
    fn an_unprovable_pid_is_never_signalled_and_is_named() -> TestResult {
        let mut f = fixture(&[4242, 5], &[])?;
        let world = World::default();
        world.live.borrow_mut().insert(4242, identity(4242, 1));
        let report = run(&mut f, &world)?;
        assert_eq!(report.unproven, vec![4242]);
        assert!(world.signals.borrow().is_empty());
        let exit = settled(&f)?;
        assert_eq!(exit["settlement_evidence"], "leftover_not_proven");
        assert_eq!(exit["unresolved_pids"], serde_json::json!([4242]));
        assert_eq!(exit["unresolved_cleanup"], false);
        Ok(())
    }

    #[test]
    fn an_unrecorded_pid_in_the_attempt_sandbox_is_proven() -> TestResult {
        let mut f = fixture(&[4242], &[])?;
        let world = World {
            sandboxed: vec![4242],
            ..World::default()
        };
        world.live.borrow_mut().insert(4242, identity(4242, 1));
        assert_eq!(run(&mut f, &world)?.stopped, vec![identity(4242, 1)]);
        Ok(())
    }

    #[test]
    fn a_survivor_keeps_the_exit_unresolved() -> TestResult {
        let tail = identity(9, 3);
        let mut f = fixture(&[9], std::slice::from_ref(&tail))?;
        let world = World::default();
        world.live.borrow_mut().insert(9, tail.clone());
        // A process that survives KILL: the fake never removes it.
        struct Immortal(World);
        impl ProcessObserver for Immortal {
            fn boot_identity(&self) -> std::result::Result<String, CensusError> {
                self.0.boot_identity()
            }
            fn observe(&self, pid: i64) -> ProcessObservation {
                self.0.observe(pid)
            }
            fn census(&self, bound: usize) -> std::result::Result<Census, CensusError> {
                self.0.census(bound)
            }
        }
        impl OrphanProbe for Immortal {
            fn in_attempt_sandbox(&self, _: i64, _: &Scope) -> bool {
                false
            }
            fn signal(&self, _: &ProcessIdentity, _: bool) -> bool {
                true
            }
        }
        let immortal = Immortal(world);
        let report =
            f.recovery
                .end_orphans_with(&f.attempt, &immortal, &immortal, Duration::ZERO)?;
        assert_eq!(report.survivors, vec![tail]);
        assert_eq!(settled(&f)?["unresolved_cleanup"], true);
        assert!(f.recovery.reservation.unresolved_cleanup);
        Ok(())
    }

    #[test]
    fn a_recorded_descendant_the_exit_did_not_name_is_reaped() -> TestResult {
        // The exit names 16 identities; the reservation records more.
        let tail = identity(84400, 7);
        let mut f = fixture(&[], std::slice::from_ref(&tail))?;
        let world = World::default();
        world.live.borrow_mut().insert(84400, tail.clone());
        let report = run(&mut f, &world)?;
        assert_eq!(report.stopped, vec![tail]);
        assert_eq!(world.signals.borrow().as_slice(), [(84400, false)]);
        let exit = settled(&f)?;
        assert_eq!(exit["settlement_evidence"], "orphans_ended");
        assert_eq!(
            exit["warnings"],
            serde_json::json!(["kept", "killed 1 surviving processes"])
        );
        Ok(())
    }

    #[test]
    fn a_sandboxed_escapee_found_only_by_census_is_reaped() -> TestResult {
        let mut f = fixture(&[], &[])?;
        let world = World {
            sandboxed: vec![5150],
            ..World::default()
        };
        world.live.borrow_mut().insert(5150, identity(5150, 4));
        assert_eq!(run(&mut f, &world)?.stopped, vec![identity(5150, 4)]);
        assert_eq!(world.signals.borrow().as_slice(), [(5150, false)]);
        Ok(())
    }

    #[test]
    fn a_reused_recorded_pid_is_left_alone() -> TestResult {
        let mut f = fixture(&[], &[identity(84401, 7)])?;
        let world = World::default();
        world.live.borrow_mut().insert(84401, identity(84401, 8));
        assert_eq!(run(&mut f, &world)?, OrphanReport::default());
        assert!(world.signals.borrow().is_empty());
        assert_eq!(settled(&f)?["warnings"], serde_json::json!(["kept"]));
        Ok(())
    }

    #[test]
    fn an_unrecorded_process_in_the_worktree_is_untouched() -> TestResult {
        let mut f = fixture(&[], &[])?;
        // Same group and session as the provider, cwd in the worktree: none of
        // it grants signal authority.
        let shell = ProcessIdentity {
            group_id: 101,
            session_id: 100,
            ..identity(6000, 1)
        };
        let world = World {
            in_worktree: vec![6000],
            ..World::default()
        };
        world.live.borrow_mut().insert(6000, shell);
        assert!(world.in_worktree.contains(&6000));
        assert_eq!(run(&mut f, &world)?, OrphanReport::default());
        assert!(world.signals.borrow().is_empty());
        assert_eq!(settled(&f)?["settlement_evidence"], "orphans_ended");
        Ok(())
    }

    #[test]
    fn a_continuing_attempt_is_untouched() -> TestResult {
        let mut f = fixture(&[], &[])?;
        let world = World {
            continuing: vec![7000],
            ..World::default()
        };
        world.live.borrow_mut().insert(7000, identity(7000, 1));
        assert!(world.in_attempt_sandbox(7000, &successor()));
        assert_eq!(run(&mut f, &world)?, OrphanReport::default());
        assert!(world.signals.borrow().is_empty());
        Ok(())
    }

    #[test]
    fn many_reaped_processes_leave_one_bounded_warning() -> TestResult {
        let mut f = fixture(&[], &[])?;
        let pids: Vec<i64> = (20_000..20_200).collect();
        let world = World {
            sandboxed: pids.clone(),
            ..World::default()
        };
        for &pid in &pids {
            world.live.borrow_mut().insert(pid, identity(pid, 1));
        }
        assert_eq!(run(&mut f, &world)?.stopped.len(), 200);
        let bytes = std::fs::read(f.path.join("process-exit.json"))?;
        assert!(bytes.len() as u64 <= EXIT_LIMIT);
        let exit: serde_json::Value = serde_json::from_slice(&bytes)?;
        assert_eq!(
            exit["warnings"],
            serde_json::json!(["kept", "killed 200 surviving processes"])
        );
        Ok(())
    }

    #[test]
    fn a_live_guardian_is_never_recovered_over() -> TestResult {
        let mut f = fixture(&[9], &[identity(9, 3)])?;
        let world = World::default();
        world.live.borrow_mut().insert(100, identity(100, 1));
        assert!(matches!(run(&mut f, &world), Err(CustodyError::Fenced)));
        assert_eq!(settled(&f)?["unresolved_cleanup"], true);
        Ok(())
    }

    #[test]
    fn a_dead_lane_without_an_exit_has_its_recorded_tree_reaped() -> TestResult {
        let tail = identity(84322, 7);
        let mut f = recorded_fixture(std::slice::from_ref(&tail))?;
        let world = World::default();
        world.live.borrow_mut().insert(84322, tail.clone());
        let report = f
            .recovery
            .end_recorded_orphans_with(&world, &world, Duration::ZERO)?;
        assert_eq!(report.stopped, vec![tail.clone()]);
        assert!(report.unproven.is_empty() && report.survivors.is_empty());
        assert_eq!(world.signals.borrow().as_slice(), [(84322, false)]);
        // No exit is invented and the reservation stays for the normal proof.
        assert!(!f.path.join("process-exit.json").exists());
        assert_eq!(f.recovery.reservation.descendants, vec![tail]);
        Ok(())
    }

    #[test]
    fn a_dead_lane_without_an_exit_reaps_its_recorded_provider() -> TestResult {
        let mut f = recorded_fixture(&[])?;
        let provider = f
            .recovery
            .reservation
            .provider
            .clone()
            .ok_or("missing provider")?;
        let world = World::default();
        world
            .live
            .borrow_mut()
            .insert(provider.pid, provider.clone());
        let report = f
            .recovery
            .end_recorded_orphans_with(&world, &world, Duration::ZERO)?;
        assert_eq!(report.stopped, vec![provider.clone()]);
        assert!(report.unproven.is_empty() && report.survivors.is_empty());
        assert_eq!(world.signals.borrow().as_slice(), [(provider.pid, false)]);
        assert!(!f.path.join("process-exit.json").exists());
        Ok(())
    }

    #[test]
    fn a_live_guardian_fences_reaping_without_an_exit() -> TestResult {
        let tail = identity(9, 3);
        let mut f = recorded_fixture(std::slice::from_ref(&tail))?;
        let world = World::default();
        world.live.borrow_mut().insert(9, tail);
        world.live.borrow_mut().insert(100, identity(100, 1));
        assert!(matches!(
            f.recovery
                .end_recorded_orphans_with(&world, &world, Duration::ZERO),
            Err(CustodyError::Fenced)
        ));
        assert!(world.signals.borrow().is_empty());
        Ok(())
    }

    #[test]
    fn a_survivor_without_an_exit_is_reported_and_the_fence_holds() -> TestResult {
        let tail = identity(9, 3);
        let mut f = recorded_fixture(std::slice::from_ref(&tail))?;
        let world = World::default();
        world.live.borrow_mut().insert(9, tail.clone());
        // A process that outlives both TERM and KILL: no exit is written and
        // the caller keeps the attempt fenced, reporting the surviving PID.
        struct Survives(World);
        impl ProcessObserver for Survives {
            fn boot_identity(&self) -> std::result::Result<String, CensusError> {
                self.0.boot_identity()
            }
            fn observe(&self, pid: i64) -> ProcessObservation {
                self.0.observe(pid)
            }
            fn census(&self, bound: usize) -> std::result::Result<Census, CensusError> {
                self.0.census(bound)
            }
        }
        impl OrphanProbe for Survives {
            fn in_attempt_sandbox(&self, _: i64, _: &Scope) -> bool {
                false
            }
            fn signal(&self, _: &ProcessIdentity, _: bool) -> bool {
                true
            }
        }
        let survives = Survives(world);
        let report = f
            .recovery
            .end_recorded_orphans_with(&survives, &survives, Duration::ZERO)?;
        assert_eq!(report.survivors, vec![tail]);
        assert!(report.stopped.is_empty());
        assert!(!f.path.join("process-exit.json").exists());
        Ok(())
    }
}
