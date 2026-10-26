//! Census, signalling receipts and quiescence under retained custody.
use super::*;

impl Driver<'_> {
    // Cleanup phases use bounded real-time windows around OS process events.
    #[allow(clippy::disallowed_methods)]
    pub(super) fn persist_tracking(&mut self, warnings: &mut Vec<String>, omitted: &mut u64) {
        self.reservation.descendants = self.tracked.iter().take(128).cloned().collect();
        self.reservation.omitted_descendants = self.tracked.len().saturating_sub(128) as u64;
        let result = serde_json::to_vec(&self.reservation)
            .map_err(|_| CustodyError::InvalidRecord)
            .and_then(|bytes| {
                if self.last_tracking.as_ref() != Some(&bytes) {
                    require_durable(write_record(
                        &self.writer_dir,
                        "reservation.json",
                        &self.reservation,
                        PublishMode::Replace,
                    )?)?;
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
            .and_then(require_durable)
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
        child: &mut crate::sys::provider::ProviderGroup,
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
        child: &mut crate::sys::provider::ProviderGroup,
    ) -> Option<crate::census::Census> {
        let mut census = match self.observer.census(crate::census::MAX_CENSUS_ENTRIES) {
            Ok(census) => census,
            Err(_) => {
                self.descendant_stop_allowed = false;
                self.escapee_stop_allowed = false;
                self.reservation.census_incomplete = true;
                return None;
            }
        };
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
            let observation = self.observer.observe(pid);
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
                        match crate::sys::census::prove_nested_custody(
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
        self.escapee_stop_allowed = registry_listed
            || (self.registry.is_none()
                && self
                    .nested_children
                    .is_some_and(|store| store.has_no_nested_runs(&self.holder_context)));
        self.reservation.census_incomplete = !census.is_complete();
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
                        .is_ok_and(|pid| crate::sys::census::darwin_in_attempt_sandbox(pid, &name))
            })
            .map(|fact| fact.identity.clone())
            .collect();
        self.tracked.extend(marked.iter().cloned());
        marked
    }

    #[cfg(target_os = "macos")]
    pub(super) fn darwin_quiescent(
        &mut self,
        child: &mut crate::sys::provider::ProviderGroup,
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
        // Darwin lacks Linux's subreaper child list. Consume the dead direct
        // provider only here, retaining attempt custody and the persistent fence.
        let _ = child.cleanup(true)?;
        if !child.quiescent_children()? {
            return Ok(false);
        }
        crate::sys::wait_pause(Duration::from_millis(20));
        let Some(second) = self.refresh_census(child) else {
            return Ok(false);
        };
        if !self.history_complete
            || !darwin_census_clear(&second, &provider, &self.guardian, &self.history)
        {
            return Ok(false);
        }
        // Close, never LOCK_UN: descendants share this open file description.
        // A fresh nonblocking flock succeeds only after every inheritor closes.
        drop(self.writer.take());
        match try_writer_lease(&self.writer_dir, self.writer_identity) {
            Ok(lease) => {
                self.writer = Some(lease);
                self.settlement_evidence = SettlementEvidence::DarwinCensusAndCustodyLease;
                Ok(true)
            }
            Err(_) => Ok(false),
        }
    }

    // Process cleanup uses a real-time cap independent of provider progress.
    #[allow(clippy::disallowed_methods)]
    pub(super) fn cleanup(
        &mut self,
        child: &mut crate::sys::provider::ProviderGroup,
        plan: CapturePlan,
        cancelled: bool,
        warnings: &mut Vec<String>,
        omitted: &mut u64,
    ) -> Result<bool> {
        let reason = if cancelled
            || self.read_cancel(warnings, omitted)
                == Some(machine::StopReason::ExplicitCancellation)
        {
            machine::StopReason::ExplicitCancellation
        } else {
            machine::StopReason::PostTerminalCleanup
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
            let mut reaped = child.reap_adopted(&census, &spared)?;
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
            let census_clear = census.is_complete() && !reaped.unresolved && !outstanding;
            #[cfg(target_os = "linux")]
            let complete = census_clear && child.quiescent_children().unwrap_or(false);
            #[cfg(target_os = "macos")]
            let complete = census_clear && self.darwin_quiescent(child, &census)?;
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            let complete = false;
            if complete {
                #[cfg(target_os = "linux")]
                {
                    self.settlement_evidence = SettlementEvidence::LinuxKernelChildren;
                }
                let _ = child.cleanup(true)?;
                self.persist_tracking(warnings, omitted);
                return Ok(true);
            }
            // Root exit does not authorise an arbitrary PID. Only the pinned
            // provider and kernel-confirmed adopted children are targets.
            let escalation = began.elapsed() >= Duration::from_millis(plan.descendant_grace_ms);
            let reason = machine::StopReason::PostTerminalCleanup;
            let signal = if escalation {
                libc::SIGKILL
            } else {
                libc::SIGTERM
            };
            let outcome = stop_delivery_outcome(child.stop(escalation));
            // ESRCH means the provider is already gone (or already signalled),
            // which later ticks would only repeat; it is not a failed delivery.
            if outcome != DeliveryOutcome::Failed(DeliveryFailure::NoSuchProcess) {
                self.record_delivery(self.reservation.provider.clone(), reason, signal, outcome);
                if outcome != DeliveryOutcome::Delivered {
                    push_warning(warnings, omitted, "post-terminal provider signal failed");
                }
            }
            #[cfg(target_os = "linux")]
            if self.descendant_stop_allowed {
                match child.stop_adopted(escalation, &census, &spared) {
                    Ok(deliveries) => {
                        for delivery in deliveries {
                            self.record_delivery(
                                Some(delivery.identity),
                                reason,
                                signal,
                                delivery.outcome,
                            );
                        }
                    }
                    Err(_) => push_warning(warnings, omitted, "adopted-child signal failed"),
                }
            }
            #[cfg(target_os = "macos")]
            if self.escapee_stop_allowed {
                for delivery in child.stop_marked(escalation, &marked) {
                    self.record_delivery(Some(delivery.identity), reason, signal, delivery.outcome);
                }
            }
            self.persist_tracking(warnings, omitted);
            if Instant::now() >= deadline {
                return Ok(false);
            }
            let _ = self.reactor.wait(
                &[],
                Duration::from_millis(plan.descendant_grace_ms.clamp(1, 20)),
            );
        }
    }
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn try_writer_lease(
    directory: &Directory,
    expected: crate::fs::Metadata,
) -> Result<crate::fs::LockGuard> {
    let lease = directory.try_lock(OsStr::new("writer.lock"), crate::fs::LockMode::Exclusive)?;
    let actual = crate::fs::Metadata::from_std(lease.handoff_file().metadata()?);
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
