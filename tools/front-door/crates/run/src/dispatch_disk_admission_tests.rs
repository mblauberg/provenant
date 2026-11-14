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
            "schema_version=1\n[gc]\nfree_floor='100B'\n[domains.fabric.lanes]\nmax_lanes=1\nmax_writer_lanes=1\nstart_spacing_ms=600000\n",
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
fn ordinary_disk_floor_never_refuses_admission() -> TestResult {
    for strength in [None, Some("refuse"), Some("off")] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let policy = strength.map_or_else(String::new, |strength| {
            format!("[knobs]\n'policy.ADMISSION_DISK'='{strength}'\n")
        });
        std::fs::write(
            store.writer_registry.join("config.toml"),
            format!("schema_version=1\n{policy}[gc]\nfree_floor='100B'\n"),
        )?;
        let id = starting(&store, &lane(80, true), &call)?;
        let attempt = store.attempt_dir(id, false)?;
        let mut facts = HostFacts::new(100);
        facts.disk = 99;
        let running = store.admit(id, &attempt, &facts, &clock, &cancel)?;
        assert!(matches!(running.state, AttemptState::Running { .. }));
    }
    Ok(())
}
