use super::*;

#[test]
fn resource_pressure_and_busy_caps_never_queue_default_admission() -> TestResult {
    struct Pressured<'a>(&'a HostFacts);
    impl Resources for Pressured<'_> {
        fn guardian_identity(&self) -> Option<provenant_supervisor::census::ProcessIdentity> {
            self.0.guardian_identity()
        }
        fn observe_process(&self, pid: i64) -> provenant_supervisor::census::ProcessObservation {
            self.0.observe_process(pid)
        }
        fn memory(&self) -> std::io::Result<MemorySample> {
            self.0.memory()
        }
        fn process_budget(&self) -> std::io::Result<provenant_os::sys::resources::ProcessBudget> {
            self.0.process_budget()
        }
        fn disk(&self, attempt: &Directory) -> std::io::Result<u64> {
            self.0.disk(attempt)
        }
        fn pressure(
            &self,
        ) -> std::io::Result<Option<provenant_os::sys::resources::MemoryPressure>> {
            Ok(Some(provenant_os::sys::resources::MemoryPressure::Critical))
        }
    }
    for writer in [false, true] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        std::fs::write(
            store.writer_registry.join("config.toml"),
            "schema_version=1\n[gc]\nfree_floor='100B'\ncritical_free='1B'\n[domains.fabric.lanes]\nmax_lanes=1\nmax_writer_lanes=1\nstart_spacing_ms=600000\n",
        )?;
        let mut facts = HostFacts::new(1);
        facts.disk = 99;
        facts.process_used = facts.process_limit;
        for seed in 80..83 {
            let id = starting(&store, &lane(seed, writer), &call)?;
            let attempt = store.attempt_dir(id, false)?;
            let running = store.admit(id, &attempt, &Pressured(&facts), &clock, &cancel)?;
            assert!(matches!(running.state, AttemptState::Running { .. }));
        }
    }
    Ok(())
}

#[test]
fn admission_uses_only_the_critical_threshold_for_refusal() -> TestResult {
    for writer in [false, true] {
        for free in [49, 50, 99, 100] {
            let temp = tempfile::tempdir()?;
            let (clock, cancel) = (clock(), CancelToken::new());
            let call = call(&clock, &cancel);
            let store = RunStore::open(&context(temp.path())?, &call)?;
            std::fs::write(
                store.writer_registry.join("config.toml"),
                "schema_version=1\n[gc]\nfree_floor='100B'\ncritical_free='50B'\n",
            )?;
            let id = starting(&store, &lane(80, writer), &call)?;
            let attempt = store.attempt_dir(id, false)?;
            let mut facts = HostFacts::new(100);
            facts.disk = free;
            let result = store.admit(id, &attempt, &facts, &clock, &cancel);
            if free < 50 {
                assert!(matches!(
                    result,
                    Err(RunError::Admission(AdmissionError::Disk { floor: 50, .. }))
                ));
            } else {
                assert!(matches!(result?.state, AttemptState::Running { .. }));
            }
        }
    }
    Ok(())
}
