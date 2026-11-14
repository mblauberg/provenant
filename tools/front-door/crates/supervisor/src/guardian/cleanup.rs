//! Census, signalling receipts and quiescence under retained custody.
use super::*;

fn live_or_full(
    observer: &dyn ProcessObserver,
    root: &ProcessIdentity,
    known: &[ProcessIdentity],
) -> (
    std::result::Result<crate::census::Census, crate::census::CensusError>,
    bool,
) {
    match observer.live_census(root, known, crate::census::MAX_CENSUS_ENTRIES) {
        Ok(Some(census))
            if census.completeness() == crate::census::CensusCompleteness::Partial
                && census.unknown_pids().is_empty() =>
        {
            (Ok(census), false)
        }
        Ok(Some(_)) | Ok(None) | Err(_) => {
            (observer.census(crate::census::MAX_CENSUS_ENTRIES), true)
        }
    }
}

impl Driver<'_> {
    /// Descriptor observations explain contention, but never grant signal or
    /// settlement authority. Share one deadline across the entire scan.
    #[allow(clippy::disallowed_methods)]
    fn warn_cleanup_holders(
        &self,
        census: Option<&crate::census::Census>,
        warnings: &mut Vec<String>,
        omitted: &mut u64,
    ) {
        use provenant_os::pid::{CwdObserver, SystemCwdObserver};
        let deadline = Instant::now() + Duration::from_millis(100);
        let resource = self
            .lock_dir
            .current_path()
            .unwrap_or_else(|_| self.lock_dir.anchored_path())
            .join("writer.lock");
        let mut found = false;
        // Explain known custody blockers before spending the diagnostic budget
        // on host-wide descriptor discovery. Their identity is already pinned.
        if let Some(census) = census {
            for fact in census.facts().filter(|fact| {
                !fact.zombie
                    && (self.tracked.contains(&fact.identity)
                        || self.reservation.provider.as_ref() == Some(&fact.identity))
            }) {
                found = true;
                push_warning(
                    warnings,
                    omitted,
                    &format!(
                        "cleanup deadline: pid {} command {:?} remains in attempt process custody; resource {} cannot be released; close the service or wait for it to exit, then run `provenant reconcile` for attempt {}; writer custody remains fenced",
                        fact.identity.pid,
                        fact.command.as_deref().unwrap_or("unknown"),
                        resource.display(),
                        self.reservation.scope.attempt,
                    ),
                );
            }
        }
        let full = self
            .observer
            .census_while(crate::census::MAX_CENSUS_ENTRIES, &|| {
                Instant::now() < deadline
            })
            .ok();
        if let Some(census) = full.as_ref().or(census) {
            for fact in census.facts() {
                if Instant::now() >= deadline {
                    break;
                }
                if fact.identity == self.guardian || fact.zombie {
                    continue;
                }
                let observed = SystemCwdObserver
                    .occupancy_while(fact.identity.pid, &|| Instant::now() < deadline);
                if observed.paths.iter().any(|path| path == &resource) {
                    found = true;
                    push_warning(
                        warnings,
                        omitted,
                        &format!(
                            "cleanup deadline: pid {} command {:?} has resource {} open; lock ownership is unproven; close the service or wait for it to exit, then run `provenant reconcile` for attempt {}",
                            fact.identity.pid,
                            fact.command.as_deref().unwrap_or("unknown"),
                            resource.display(),
                            self.reservation.scope.attempt,
                        ),
                    );
                }
            }
        }
        if !found {
            push_warning(
                warnings,
                omitted,
                &format!(
                    "cleanup deadline: resource {} or process census remains unresolved; holder pid and command unavailable; inspect open descriptors and run `provenant reconcile` for attempt {} after the holder exits; writer custody remains fenced",
                    resource.display(),
                    self.reservation.scope.attempt,
                ),
            );
        }
    }

    #[cfg(target_os = "macos")]
    pub(super) fn holder_reports_no_live_children(&self) -> bool {
        host_allows_escapee_stop(self.holder, &self.holder_context)
    }

    pub(super) fn persist_tracking(&mut self, warnings: &mut Vec<String>, omitted: &mut u64) {
        self.reservation.descendants = self.tracked.iter().take(128).cloned().collect();
        self.reservation.omitted_descendants = self.tracked.len().saturating_sub(128) as u64;
        let result = serde_json::to_vec(&self.reservation)
            .map_err(|_| CustodyError::InvalidRecord)
            .and_then(|bytes| {
                if self.last_tracking.as_ref() != Some(&bytes) {
                    let system_publisher = SystemExitPublisher;
                    Publication::require_durable(
                        self.exit_publisher
                            .unwrap_or(&system_publisher)
                            .tracking(&self.writer_dir, &bytes)?,
                    )?;
                    self.last_tracking = Some(bytes);
                }
                Ok(())
            });
        if result.is_err() {
            self.reservation.census_incomplete = true;
            if !self.journal_warning_active {
                push_warning(warnings, omitted, "descendant journal publication failed");
            }
        }
        self.journal_warning_active = result.is_err();
    }
    pub(super) fn persist_reservation(&mut self) -> bool {
        let Ok(bytes) = serde_json::to_vec(&self.reservation) else {
            return false;
        };
        let system_publisher = SystemExitPublisher;
        self.exit_publisher
            .unwrap_or(&system_publisher)
            .commit_reservation(&self.writer_dir, &bytes)
            .and_then(|publication| publication.require_durable().map_err(Into::into))
            .is_ok()
    }

    pub(super) fn record_delivery(
        &mut self,
        identity: Option<ProcessIdentity>,
        reason: machine::StopReason,
        signal: i32,
        outcome: DeliveryOutcome,
    ) {
        let signal = i64::from(signal);
        match outcome {
            DeliveryOutcome::Delivered => {
                if !self.delivered_signals.contains(&signal) {
                    self.delivered_signals.push(signal);
                }
            }
            DeliveryOutcome::Failed(failure) => {
                let prior_provider_delivery = identity.as_ref()
                    == self.reservation.provider.as_ref()
                    && failure == DeliveryFailure::NoSuchProcess
                    && self.delivered_signals.contains(&signal);
                if prior_provider_delivery {
                    return;
                }
                if self.failed_deliveries.len() < 16 {
                    self.failed_deliveries.push(FailedSignalDelivery {
                        identity,
                        reason,
                        signal,
                        failure,
                    });
                } else {
                    self.omitted_failed_deliveries =
                        self.omitted_failed_deliveries.saturating_add(1);
                }
            }
        }
    }

    /// The attempt's pending cancellation request, if it is valid for this
    /// scope and requester. Stale or unreadable requests only warn.
    pub(super) fn read_cancel(
        &self,
        warnings: &mut Vec<String>,
        omitted: &mut u64,
    ) -> Option<machine::StopReason> {
        match read_record::<CancelRequest>(&self.attempt, "cancel.json", RECORD_LIMIT) {
            Ok(request)
                if request.scope == self.reservation.scope
                    && request.requester == self.requester
                    && provenant_core::OperationId::parse(request.request_id.as_str()).is_ok() =>
            {
                Some(request.reason)
            }
            Ok(_) => {
                push_warning(warnings, omitted, "stale cancellation ignored");
                None
            }
            Err(CustodyError::Filesystem(error)) if error.is_not_found() => None,
            Err(_) => {
                push_warning(warnings, omitted, "cancel request unreadable");
                None
            }
        }
    }

    /// Ends the group of a provider that already exited, on cancellation or
    /// natural exit alike: TERM then, after the descendant grace, KILL to the
    /// provider's group while its unreaped leader still reserves the PGID.
    /// Returns once no live member remains or the bounded budget is spent;
    /// settlement is judged by the caller.
    // Process group escalation follows real elapsed time.
    #[allow(clippy::disallowed_methods)]
    fn terminate_group(
        &mut self,
        child: &mut crate::process_os::provider::ProviderGroup,
        plan: CapturePlan,
        reason: machine::StopReason,
        warnings: &mut Vec<String>,
        omitted: &mut u64,
    ) {
        let began = Instant::now();
        let grace = Duration::from_millis(plan.descendant_grace_ms);
        let group = child.id();
        let mut escalation = false;
        loop {
            // A group signal cannot exclude members, so never send one while a
            // spared or protected owner shares the group, or without a census.
            let census = self.refresh_census(child);
            // Cancellation retries a failed census; a natural exit leaves the
            // retry and its warning to the cleanup loop.
            if census.is_none() && reason != machine::StopReason::ExplicitCancellation {
                return;
            }
            if let Some(census) = census {
                let members = || {
                    census.facts().filter(|fact| {
                        fact.identity.group_id == group && fact.identity.pid != group
                    })
                };
                let shielded = members().any(|fact| {
                    self.protected.contains(&fact.identity)
                        || self
                            .spared
                            .spared_identities()
                            .any(|spared| *spared == fact.identity)
                });
                if shielded || (census.is_complete() && members().all(|fact| fact.zombie)) {
                    return;
                }
                let signal = if escalation {
                    libc::SIGKILL
                } else {
                    libc::SIGTERM
                };
                let outcome = stop_delivery_outcome(child.stop_group(escalation));
                self.record_delivery(self.reservation.provider.clone(), reason, signal, outcome);
                if outcome != DeliveryOutcome::Delivered && !escalation {
                    push_warning(warnings, omitted, "group signal not delivered");
                }
            }
            if escalation && began.elapsed() >= grace.saturating_mul(2) {
                return;
            }
            if !escalation && began.elapsed() >= grace {
                escalation = true;
                continue;
            }
            let _ = self.reactor.wait(&[], Duration::from_millis(20));
        }
    }

    pub(super) fn provider_cpu_micros(&self) -> Option<u64> {
        let expected = self.reservation.provider.as_ref()?;
        match self.observer.observe(expected.pid) {
            ProcessObservation::Present(fact) if fact.identity == *expected => fact.cpu_micros,
            ProcessObservation::Present(_)
            | ProcessObservation::Absent
            | ProcessObservation::Unknown(_) => None,
        }
    }

    pub(super) fn refresh_census(
        &mut self,
        child: &mut crate::process_os::provider::ProviderGroup,
    ) -> Option<crate::census::Census> {
        let census = match self.observer.census(crate::census::MAX_CENSUS_ENTRIES) {
            Ok(census) => census,
            Err(_) => {
                self.descendant_stop_allowed = false;
                self.escapee_stop_allowed = false;
                self.reservation.census_incomplete = true;
                return None;
            }
        };
        self.apply_census(child, census, true)
    }

    /// The scoped walk records history only. It never supplies the complete
    /// host observation required by cleanup or changes the last full result.
    pub(super) fn refresh_live_census(
        &mut self,
        child: &mut crate::process_os::provider::ProviderGroup,
    ) -> (Option<crate::census::Census>, bool) {
        let Some(root) = self.reservation.provider.as_ref() else {
            return (self.refresh_census(child), true);
        };
        let known = self
            .history
            .iter()
            .chain(self.tracked.iter())
            .chain(self.protected.iter())
            .chain(self.spared.spared_identities())
            .chain(self.reservation.descendants.iter())
            .cloned()
            .collect::<Vec<_>>();
        let (census, full) = live_or_full(self.observer, root, &known);
        match census {
            Ok(census) => (self.apply_census(child, census, full), full),
            Err(_) => {
                self.descendant_stop_allowed = false;
                self.escapee_stop_allowed = false;
                self.reservation.census_incomplete = true;
                (None, full)
            }
        }
    }

    fn apply_census(
        &mut self,
        child: &mut crate::process_os::provider::ProviderGroup,
        mut census: crate::census::Census,
        full: bool,
    ) -> Option<crate::census::Census> {
        let Some(root) = self.reservation.provider.as_ref() else {
            self.descendant_stop_allowed = false;
            self.escapee_stop_allowed = false;
            self.reservation.census_incomplete = true;
            return None;
        };
        let mut known_pids = std::collections::BTreeSet::from([root.pid, child.id()]);
        known_pids.extend(self.tracked.iter().map(|identity| identity.pid));
        known_pids.extend(self.history.iter().map(|identity| identity.pid));
        known_pids.extend(self.protected.iter().map(|identity| identity.pid));
        known_pids.extend(self.spared.spared_identities().map(|identity| identity.pid));
        known_pids.extend(
            self.reservation
                .descendants
                .iter()
                .map(|identity| identity.pid),
        );
        let mut attempted = 0_usize;
        for pid in known_pids {
            if !matches!(census.process(pid), crate::census::ProcessLookup::Unknown) {
                continue;
            }
            if attempted >= crate::census::MAX_CENSUS_ENTRIES {
                census.mark_truncated();
                break;
            }
            attempted = attempted.saturating_add(1);
            let observation = self.observer.observe_topology(pid);
            if census
                .observe_known(pid, &self.reservation.boot, observation)
                .is_err()
            {
                census.mark_truncated();
            }
        }
        self.history
            .retain(|identity| identity_still_possible(&census, identity));
        child.record_census(&census);
        self.spared.refresh(&census);
        let mut registry_unknown = self.registry.is_none();
        let mut registry_listed = false;
        if let Some(registry) = self.registry {
            match registry.registered_children() {
                Ok(children) if children.len() <= 4096 => {
                    registry_listed = true;
                    for registered in children {
                        let identity = registered.registration.identity();
                        match crate::process_os::census::prove_nested_custody(
                            self.observer,
                            &registered.registration,
                            registered.custody.handoff_file(),
                        ) {
                            Ok(proof) => {
                                if self
                                    .spared
                                    .spare_registered_owner(
                                        &census,
                                        root,
                                        &registered.registration,
                                        &proof,
                                    )
                                    .is_err()
                                {
                                    registry_unknown = true;
                                    self.protected.insert(identity.clone());
                                }
                            }
                            Err(_) => {
                                registry_unknown = true;
                                self.protected.insert(identity.clone());
                            }
                        }
                    }
                }
                _ => registry_unknown = true,
            }
        }
        for fact in census.facts() {
            if fact.identity.pid == child.id() {
                continue;
            }
            if census.ancestry(&fact.identity, root) == crate::census::Ancestry::Yes
                || fact.parent_pid == child.id()
                || fact.identity.group_id == child.id()
                || self.history.iter().any(|ancestor| {
                    census.ancestry(&fact.identity, ancestor) == crate::census::Ancestry::Yes
                })
            {
                self.tracked.insert(fact.identity.clone());
                if self.history.len() < crate::census::MAX_CENSUS_ENTRIES {
                    self.history.insert(fact.identity.clone());
                } else if !self.history.contains(&fact.identity) {
                    self.history_complete = false;
                }
            }
            if fact
                .command
                .as_ref()
                .is_some_and(|name| self.protected_commands.contains(name))
                && (self.tracked.contains(&fact.identity) || fact.identity.group_id == child.id())
            {
                self.protected.insert(fact.identity.clone());
            }
        }
        self.tracked
            .retain(|identity| identity_still_possible(&census, identity));
        self.protected
            .retain(|identity| identity_still_possible(&census, identity));
        self.descendant_stop_allowed = !registry_unknown;
        // Census establishes process history, not graph signal authority. Read
        // graph evidence only at a signalling decision, after submissions stop.
        self.escapee_stop_allowed = registry_listed || self.registry.is_none();
        if full {
            self.reservation.census_incomplete = !census.is_complete();
        }
        Some(census)
    }
    /// Live processes outside the provider and any spared owner that are still
    /// inside this attempt's sandbox. They are added to `tracked`, so cleanup cannot
    /// settle while one lives, and returned for signalling.
    #[cfg(target_os = "macos")]
    fn marked_escapees(
        &mut self,
        census: &crate::census::Census,
        provider_pid: i64,
        spared: &std::collections::BTreeSet<ProcessIdentity>,
    ) -> Vec<ProcessIdentity> {
        let Ok(name) = std::ffi::CString::new(self.reservation.scope.attempt.sandbox_lookup_name())
        else {
            return Vec::new();
        };
        let marked: Vec<_> = census
            .facts()
            .filter(|fact| {
                !fact.zombie
                    && fact.identity.pid != provider_pid
                    && fact.identity != self.guardian
                    && !spared.contains(&fact.identity)
                    // A nested owner's own descendants are its to end.
                    && !self.protected.iter().any(|owner| {
                        census.ancestry(&fact.identity, owner) != crate::census::Ancestry::No
                    })
                    && i32::try_from(fact.identity.pid)
                        .is_ok_and(|pid| matches!(crate::process_os::census::darwin_in_attempt_sandbox(pid, &name), Ok(true)))
            })
            .map(|fact| fact.identity.clone())
            .collect();
        self.tracked.extend(marked.iter().cloned());
        marked
    }

    #[cfg(target_os = "macos")]
    pub(super) fn darwin_quiescent(
        &mut self,
        child: &mut crate::process_os::provider::ProviderGroup,
        first: &crate::census::Census,
    ) -> Result<bool> {
        let Some(provider) = self.reservation.provider.clone() else {
            return Ok(false);
        };
        if !self.history_complete
            || !darwin_census_clear(first, &provider, &self.guardian, &self.history)
        {
            return Ok(false);
        }
        // Consume the dead direct provider only here, retaining attempt
        // custody and the persistent fence.
        let _ = child.cleanup(true)?;
        if !child.quiescent_children()? {
            return Ok(false);
        }
        provenant_os::sys::wait_pause(Duration::from_millis(20));
        let Some(second) = self.refresh_census(child) else {
            return Ok(false);
        };
        if !self.history_complete
            || !darwin_census_clear(&second, &provider, &self.guardian, &self.history)
        {
            return Ok(false);
        }
        // Close, never LOCK_UN: the private guardian handoff may share this
        // open file description. Providers never inherit it. A fresh lease
        // still checks custody independently of the process census.
        drop(self.writer.take());
        match try_writer_lease(&self.lock_dir, self.writer_identity) {
            Ok(lease) => {
                self.writer = Some(lease);
                self.settlement_evidence = SettlementEvidence::DarwinCensusAndCustodyLease;
                Ok(true)
            }
            Err(_) => Ok(false),
        }
    }

    /// Retire without settlement when the original lock inode has been
    /// unlinked. This never clears the fence or replaces an unresolved exit.
    /// The independent pre-opened descriptor proves that no inheritor still
    /// holds the old lock description; a replacement pathname proves nothing.
    #[cfg(target_os = "macos")]
    pub(super) fn retire_unlinked_writer(
        &mut self,
        child: &mut crate::process_os::provider::ProviderGroup,
    ) -> bool {
        let Some(probe) = self.writer_retirement_probe.as_ref() else {
            return false;
        };
        if !unlinked_original(probe, self.writer_identity) {
            return false;
        }
        let Some(first) = self.refresh_census(child) else {
            return false;
        };
        if !self.retirement_census_clear(child, &first)
            || child.cleanup(true).is_err()
            || !child.quiescent_children().unwrap_or(false)
        {
            return false;
        }
        provenant_os::sys::wait_pause(Duration::from_millis(20));
        let Some(second) = self.refresh_census(child) else {
            return false;
        };
        if !self.retirement_census_clear(child, &second) {
            return false;
        }
        let Some(probe) = self.writer_retirement_probe.as_ref() else {
            return false;
        };
        try_unlinked_writer_lease(probe, self.writer_identity, &mut self.writer)
    }

    #[cfg(target_os = "macos")]
    fn retirement_census_clear(
        &mut self,
        child: &crate::process_os::provider::ProviderGroup,
        census: &crate::census::Census,
    ) -> bool {
        let Some(provider) = self.reservation.provider.as_ref() else {
            return false;
        };
        if !self.history_complete
            || !darwin_census_clear(census, provider, &self.guardian, &self.history)
            || self
                .tracked
                .iter()
                .chain(self.protected.iter())
                .chain(self.spared.spared_identities())
                .chain(self.reservation.descendants.iter())
                .any(|identity| identity_still_possible(census, identity))
        {
            return false;
        }
        let mut spared: std::collections::BTreeSet<_> =
            self.spared.spared_identities().cloned().collect();
        spared.extend(self.protected.iter().cloned());
        self.marked_escapees(census, child.id(), &spared).is_empty()
    }

    // Process cleanup uses a real-time cap independent of provider progress.
    #[allow(clippy::disallowed_methods)]
    pub(super) fn cleanup(
        &mut self,
        child: &mut crate::process_os::provider::ProviderGroup,
        plan: CapturePlan,
        stop_reason: Option<machine::StopReason>,
        warnings: &mut Vec<String>,
        omitted: &mut u64,
    ) -> Result<bool> {
        let reason = if stop_reason == Some(machine::StopReason::ExplicitCancellation)
            || self.read_cancel(warnings, omitted)
                == Some(machine::StopReason::ExplicitCancellation)
        {
            machine::StopReason::ExplicitCancellation
        } else {
            stop_reason.unwrap_or(machine::StopReason::PostTerminalCleanup)
        };
        self.terminate_group(child, plan, reason, warnings, omitted);
        let deadline = Instant::now()
            + Duration::from_millis(
                plan.root_flush_ms
                    .saturating_add(plan.descendant_grace_ms)
                    .saturating_add(100),
            );
        let began = Instant::now();
        let mut census_warning_active = false;
        #[cfg(target_os = "macos")]
        {
            let _ = child.cleanup(true)?;
        }
        loop {
            let Some(census) = self.refresh_census(child) else {
                if !census_warning_active {
                    push_warning(warnings, omitted, "cleanup census unavailable; retrying");
                    census_warning_active = true;
                }
                if Instant::now() >= deadline {
                    self.warn_cleanup_holders(None, warnings, omitted);
                    return Ok(false);
                }
                let _ = self.reactor.wait(&[], Duration::from_millis(20));
                continue;
            };
            census_warning_active = false;
            let mut spared: std::collections::BTreeSet<_> =
                self.spared.spared_identities().cloned().collect();
            spared.extend(self.protected.iter().cloned());
            // A descendant that detached before the census saw it stays in the
            // attempt's sandbox; it is tracked here and ended below.
            #[cfg(target_os = "macos")]
            let marked = self.marked_escapees(&census, child.id(), &spared);
            // Reaping sends no signal, so an adopted zombie is reaped even when
            // descendant stops are not allowed; otherwise a fenced guardian
            // would pin it indefinitely.
            let reaped = child.reap_adopted(&census, &spared)?;
            #[cfg(target_os = "macos")]
            let mut reaped = reaped;
            #[cfg(target_os = "macos")]
            if !self.descendant_stop_allowed {
                reaped.unresolved = !self
                    .tracked
                    .iter()
                    .all(|identity| reaped.reaped.contains(identity));
            }
            for identity in reaped.reaped {
                self.tracked.remove(&identity);
                if self.reaped.len() < 16 {
                    self.reaped.push(identity);
                } else {
                    self.omitted_reaped = self.omitted_reaped.saturating_add(1);
                }
            }
            #[cfg(target_os = "macos")]
            let outstanding =
                self.tracked
                    .iter()
                    .any(|identity| match census.process(identity.pid) {
                        crate::census::ProcessLookup::Absent => false,
                        crate::census::ProcessLookup::Present(fact) => {
                            fact.identity == *identity && !fact.zombie
                        }
                        crate::census::ProcessLookup::Unknown => true,
                    });
            #[cfg(target_os = "macos")]
            let census_clear = census.is_complete() && !reaped.unresolved && !outstanding;
            #[cfg(target_os = "macos")]
            let complete = census_clear && self.darwin_quiescent(child, &census)?;
            #[cfg(not(target_os = "macos"))]
            let complete = false;
            if complete {
                let _ = child.cleanup(true)?;
                self.persist_tracking(warnings, omitted);
                return Ok(true);
            }
            // Root exit does not authorise an arbitrary PID. Descendants need
            // retained provider-ancestry proof or an attempt-sandbox mark.
            let escalation = began.elapsed() >= Duration::from_millis(plan.descendant_grace_ms);
            let delivery_reason = machine::StopReason::PostTerminalCleanup;
            let signal = if escalation {
                libc::SIGKILL
            } else {
                libc::SIGTERM
            };
            let outcome = stop_delivery_outcome(child.stop(escalation));
            // ESRCH means the provider is already gone (or already signalled),
            // which later ticks would only repeat; it is not a failed delivery.
            if outcome != DeliveryOutcome::Failed(DeliveryFailure::NoSuchProcess) {
                self.record_delivery(
                    self.reservation.provider.clone(),
                    delivery_reason,
                    signal,
                    outcome,
                );
                if outcome != DeliveryOutcome::Delivered {
                    push_warning(warnings, omitted, "post-terminal provider signal failed");
                }
            }
            #[cfg(target_os = "macos")]
            if self.holder_reports_no_live_children() {
                let mut targets = std::collections::BTreeSet::new();
                // Native descendants require a requested provider stop, not
                // merely its ordinary exit. Without a native child registry,
                // an actual host holder must affirm that no graph child remains;
                // an absent holder is not that proof.
                let requested_stop = match reason {
                    machine::StopReason::PostTerminalCleanup => false,
                    machine::StopReason::ExplicitCancellation
                    | machine::StopReason::WallDeadline
                    | machine::StopReason::ProtocolRejected
                    | machine::StopReason::SupervisorSignal
                    | machine::StopReason::TokenBudget
                    | machine::StopReason::ContextHandoff => true,
                };
                if requested_stop
                    && (self.descendant_stop_allowed
                        || (self.registry.is_none() && self.holder.is_some()))
                {
                    targets.extend(child.owned_cleanup_targets(&census, &spared));
                }
                if self.escapee_stop_allowed {
                    targets.extend(marked);
                }
                let targets: Vec<_> = targets.into_iter().collect();
                for delivery in child.stop_owned_targets(escalation, &targets) {
                    self.record_delivery(
                        Some(delivery.identity),
                        delivery_reason,
                        signal,
                        delivery.outcome,
                    );
                }
            }
            self.persist_tracking(warnings, omitted);
            if Instant::now() >= deadline {
                self.warn_cleanup_holders(Some(&census), warnings, omitted);
                return Ok(false);
            }
            let _ = self.reactor.wait(
                &[],
                Duration::from_millis(plan.descendant_grace_ms.clamp(1, 20)),
            );
        }
    }
}

/// Unknown host graph state refuses escapee signals. This is intentionally
/// separate from census: an ordinary history walk must not reopen graph state.
#[cfg(any(target_os = "macos", test))]
pub(super) fn host_allows_escapee_stop(holder: Option<&dyn Holder>, context: &[u8]) -> bool {
    holder.is_none_or(|holder| matches!(holder.has_live_children(context), Ok(false)))
}

#[cfg(any(target_os = "macos", test))]
fn unlinked_original(
    probe: &provenant_os::fs::FileHandle,
    expected: provenant_os::fs::Metadata,
) -> bool {
    probe
        .metadata()
        .is_ok_and(|actual| actual.links == 0 && actual.same_identity(&expected))
}

/// The probe was opened independently while the inode was linked. Closing our
/// inherited description and acquiring this one excludes every remaining
/// inheritor, even though no directory entry can be opened any longer.
#[cfg(any(target_os = "macos", test))]
pub(super) fn try_unlinked_writer_lease(
    probe: &provenant_os::fs::FileHandle,
    expected: provenant_os::fs::Metadata,
    writer: &mut Option<provenant_os::fs::LockGuard>,
) -> bool {
    if !unlinked_original(probe, expected) {
        return false;
    }
    drop(writer.take());
    provenant_os::sys::fs::try_flock(probe.handoff_file(), provenant_os::fs::LockMode::Exclusive)
        .is_ok()
        && unlinked_original(probe, expected)
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn try_writer_lease(
    directory: &Directory,
    expected: provenant_os::fs::Metadata,
) -> Result<provenant_os::fs::LockGuard> {
    let observation = crate::custody::writer_admission_guard(directory)?;
    let lease = directory.try_lock(
        OsStr::new("writer.lock"),
        provenant_os::fs::LockMode::Exclusive,
    )?;
    drop(observation);
    let actual = lease.metadata()?;
    if !expected.same_identity(&actual) {
        return Err(CustodyError::InvalidRecord);
    }
    Ok(lease)
}

pub(super) fn identity_still_possible(
    census: &crate::census::Census,
    identity: &ProcessIdentity,
) -> bool {
    match census.process(identity.pid) {
        crate::census::ProcessLookup::Absent => false,
        crate::census::ProcessLookup::Present(fact) => fact.identity == *identity,
        crate::census::ProcessLookup::Unknown => true,
    }
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn darwin_census_clear(
    census: &crate::census::Census,
    provider: &ProcessIdentity,
    guardian: &ProcessIdentity,
    history: &std::collections::BTreeSet<ProcessIdentity>,
) -> bool {
    if !census.is_complete()
        || identity_still_possible(census, provider)
        || history
            .iter()
            .any(|identity| identity_still_possible(census, identity))
    {
        return false;
    }
    census.facts().all(|fact| {
        // The guardian itself necessarily remains in the provider's session.
        if fact.identity == *guardian {
            return true;
        }
        if fact.identity == *provider
            || fact.identity.group_id == provider.group_id
            || fact.identity.session_id == provider.session_id
            || history.contains(&fact.identity)
        {
            return false;
        }
        // Parent edges are useful only between current, start-stamped identities.
        !history.iter().any(|ancestor| {
            census.ancestry(&fact.identity, ancestor) != crate::census::Ancestry::No
        })
    })
}

#[cfg(test)]
mod live_census_tests {
    use super::*;
    use crate::census::{Census, CensusError, ObservationIssue, ProcessObservation};
    use std::cell::Cell;

    struct GraphHolder {
        live: Option<bool>,
        reads: Cell<usize>,
    }
    impl Holder for GraphHolder {
        fn admit(&self, _: &[u8], _: &Directory) -> Result<()> {
            Ok(())
        }
        fn terminal(
            &self,
            _: &[u8],
            _: &Directory,
            _: &ExitObservation,
            _: &crate::custody::Recovery,
        ) -> Result<()> {
            Ok(())
        }
        fn has_live_children(&self, context: &[u8]) -> Result<bool> {
            assert_eq!(context, b"attempt");
            self.reads.set(self.reads.get() + 1);
            self.live.ok_or(CustodyError::InvalidRecord)
        }
    }

    #[test]
    fn escapee_signal_requires_current_clear_host_graph_evidence() {
        assert!(host_allows_escapee_stop(None, b"attempt"));
        for live in [Some(false), Some(true), None] {
            let holder = GraphHolder {
                live,
                reads: Cell::new(0),
            };
            assert_eq!(
                host_allows_escapee_stop(Some(&holder), b"attempt"),
                live == Some(false)
            );
            assert_eq!(holder.reads.get(), 1);
        }
    }

    struct Observer {
        mode: u8,
        full_calls: Cell<usize>,
    }
    impl ProcessObserver for Observer {
        fn boot_identity(&self) -> std::result::Result<String, CensusError> {
            Ok("boot".into())
        }
        fn observe(&self, _: i64) -> ProcessObservation {
            ProcessObservation::Unknown(ObservationIssue::Io)
        }
        fn census(&self, _: usize) -> std::result::Result<Census, CensusError> {
            self.full_calls.set(self.full_calls.get() + 1);
            Census::from_observations([], [], [], true, false, 10)
        }
        fn live_census(
            &self,
            _: &ProcessIdentity,
            _: &[ProcessIdentity],
            _: usize,
        ) -> std::result::Result<Option<Census>, CensusError> {
            match self.mode {
                0 => Ok(None),
                1 => Err(CensusError::InvalidBound),
                2 => Ok(Some(Census::from_observations(
                    [],
                    [(4, ObservationIssue::Io)],
                    [],
                    false,
                    false,
                    10,
                )?)),
                _ => Ok(Some(Census::from_observations(
                    [],
                    [],
                    [],
                    false,
                    false,
                    10,
                )?)),
            }
        }
    }
    #[test]
    fn live_census_falls_back_to_full_on_unavailable_overflow_or_partial_failure() {
        let root = ProcessIdentity {
            boot: "boot".into(),
            pid: 10,
            native_start: 1,
            group_id: 10,
            session_id: 1,
        };
        for mode in 0..=3 {
            let observer = Observer {
                mode,
                full_calls: Cell::new(0),
            };
            let (result, fallback) = live_or_full(&observer, &root, &[]);
            assert_eq!(fallback, mode < 3);
            assert_eq!(observer.full_calls.get(), usize::from(mode < 3));
            assert_eq!(result.unwrap().is_complete(), mode < 3);
        }
    }
}
