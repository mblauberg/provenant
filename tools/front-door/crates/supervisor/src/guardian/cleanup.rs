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
    /// settlement authority. Share one deadline across the entire scan. The
    /// scan runs once per guardian: retained custody reaches a cleanup
    /// deadline every escalation, and the holders rarely change between them.
    #[allow(clippy::disallowed_methods)]
    fn warn_cleanup_holders(
        &mut self,
        census: Option<&crate::census::Census>,
        warnings: &mut Vec<String>,
        omitted: &mut u64,
    ) {
        use provenant_os::pid::{CwdObserver, SystemCwdObserver};
        if std::mem::replace(&mut self.cleanup_holders_warned, true) {
            return;
        }
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
        match read_stop_intent(&self.attempt, self.reservation.scope) {
            Ok(StopIntentState::Requested(request)) if request.requester == self.requester => {
                Some(request.reason)
            }
            Ok(StopIntentState::Absent) => None,
            Ok(_) => {
                push_warning(warnings, omitted, "stale or invalid stop intent ignored");
                None
            }
            Err(_) => {
                push_warning(warnings, omitted, "stop intent unreadable");
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
            if let Some(census) = census
                && self
                    .signal_group(child, &census, reason, escalation, warnings, omitted)
                    .is_none()
            {
                return;
            }
            if escalation && began.elapsed() >= grace.saturating_mul(2) {
                return;
            }
            if !escalation && began.elapsed() >= grace {
                escalation = true;
                continue;
            }
            self.reactor.pause(Duration::from_millis(20));
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
        let spared = self.cleanup_spared();
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
                self.reactor.pause(Duration::from_millis(20));
                continue;
            };
            census_warning_active = false;
            let spared = self.cleanup_spared();
            let (complete, marked) = self.observe_settlement(child, &census, &spared)?;
            if complete {
                let _ = child.cleanup(true)?;
                self.persist_tracking(warnings, omitted);
                return Ok(true);
            }
            let escalation = began.elapsed() >= Duration::from_millis(plan.descendant_grace_ms);
            self.stop_pass(
                child, &census, &spared, marked, escalation, warnings, omitted,
            );
            self.persist_tracking(warnings, omitted);
            if Instant::now() >= deadline {
                self.warn_cleanup_holders(Some(&census), warnings, omitted);
                return Ok(false);
            }
            self.reactor
                .pause(Duration::from_millis(plan.descendant_grace_ms.clamp(1, 20)));
        }
    }

    /// The group signal for one census: `None` when a spared or protected
    /// owner shares the group (a group signal cannot exclude it) or no live
    /// member remains; otherwise the live members signalled, if delivered.
    fn signal_group(
        &mut self,
        child: &mut crate::process_os::provider::ProviderGroup,
        census: &crate::census::Census,
        reason: machine::StopReason,
        escalation: bool,
        warnings: &mut Vec<String>,
        omitted: &mut u64,
    ) -> Option<Vec<ProcessIdentity>> {
        let group = child.id();
        let members = || {
            census
                .facts()
                .filter(move |fact| fact.identity.group_id == group && fact.identity.pid != group)
        };
        let shielded = members().any(|fact| {
            self.protected.contains(&fact.identity)
                || self
                    .spared
                    .spared_identities()
                    .any(|spared| *spared == fact.identity)
        });
        if shielded || (census.is_complete() && members().all(|fact| fact.zombie)) {
            return None;
        }
        let signal = if escalation {
            libc::SIGKILL
        } else {
            libc::SIGTERM
        };
        let outcome = stop_delivery_outcome(child.stop_group(escalation));
        self.record_delivery(self.reservation.provider.clone(), reason, signal, outcome);
        if outcome != DeliveryOutcome::Delivered {
            if !escalation {
                push_warning(warnings, omitted, "group signal not delivered");
            }
            return Some(Vec::new());
        }
        Some(
            members()
                .filter(|fact| !fact.zombie)
                .map(|fact| fact.identity.clone())
                .collect(),
        )
    }

    /// One signalling pass over one census: the provider, then (on macOS)
    /// recorded descendants and sandbox-marked escapees the host allows.
    /// Returns the live identities that were signalled.
    #[allow(clippy::too_many_arguments)]
    fn stop_pass(
        &mut self,
        child: &mut crate::process_os::provider::ProviderGroup,
        census: &crate::census::Census,
        spared: &std::collections::BTreeSet<ProcessIdentity>,
        marked: Vec<ProcessIdentity>,
        escalation: bool,
        warnings: &mut Vec<String>,
        omitted: &mut u64,
    ) -> Vec<ProcessIdentity> {
        let mut signalled = Vec::new();
        // Root exit does not authorise an arbitrary PID. Descendants need
        // retained provider-ancestry proof or an attempt-sandbox mark.
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
        if outcome == DeliveryOutcome::Delivered
            && let Some(provider) = self.reservation.provider.as_ref()
            && matches!(
                census.process(provider.pid),
                crate::census::ProcessLookup::Present(fact)
                    if fact.identity == *provider && !fact.zombie
            )
        {
            signalled.push(provider.clone());
        }
        #[cfg(target_os = "macos")]
        {
            let mut candidates = std::collections::BTreeSet::new();
            // The lane's own tree ends with it, whatever ended the provider
            // (#3545). Only recorded descendants with retained provider
            // ancestry and sandbox-marked escapees are candidates, and each
            // signal re-observes the exact identity. Without a native child
            // registry, an actual host holder must affirm that no graph
            // child remains; an absent holder is not that proof.
            let owned = child.owned_cleanup_targets(census, spared);
            let descendants_allowed =
                self.descendant_stop_allowed || (self.registry.is_none() && self.holder.is_some());
            // A live candidate this pass may not signal can settle only by its
            // own exit. Only actual candidates count: a withheld permission
            // with nothing to stop refuses nothing.
            let mut refused = (!descendants_allowed && !owned.is_empty())
                || (!self.escapee_stop_allowed && !marked.is_empty());
            if descendants_allowed {
                candidates.extend(owned);
            }
            if self.escapee_stop_allowed {
                candidates.extend(marked);
            }
            let had_candidates = !candidates.is_empty();
            let targets = host_authorised_targets(self.holder, &self.holder_context, candidates);
            refused |= had_candidates && targets.is_empty();
            self.stop_refused = refused;
            for delivery in child.stop_owned_targets(escalation, &targets) {
                if delivery.outcome == DeliveryOutcome::Delivered {
                    self.stopped_descendants.insert(delivery.identity.clone());
                    signalled.push(delivery.identity.clone());
                }
                self.record_delivery(
                    Some(delivery.identity),
                    delivery_reason,
                    signal,
                    delivery.outcome,
                );
            }
            self.note_stopped_descendants(warnings, omitted);
        }
        #[cfg(not(target_os = "macos"))]
        {
            drop(marked);
            let _ = spared;
        }
        signalled
    }

    /// Retained custody's repeated stop. Unlike `cleanup`, it never polls:
    /// one census, then TERM and a wait on exit watches for exactly the
    /// signalled processes, then KILL and another, then one last census. With
    /// nothing to signal it returns after the first census.
    pub(super) fn retained_stop(
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
        #[cfg(target_os = "macos")]
        {
            let _ = child.cleanup(true)?;
        }
        retained_stop_flow(&mut DriverPasses {
            driver: self,
            child,
            reason,
            grace: Duration::from_millis(plan.descendant_grace_ms.max(1)),
            warnings,
            omitted,
            last: None,
        })
    }

    /// Block on exit watches until every identity is gone or `grace` ends.
    /// Each wake checks only these identities, never a host census. The
    /// reactor keeps every edge it drains for the retained loop, including
    /// process events: one may be the exit of an identity outside
    /// `identities` that blocks settlement. A supervisor signal ends the wait.
    #[allow(clippy::disallowed_methods)]
    fn await_exits(&mut self, identities: &[ProcessIdentity], grace: Duration) {
        let pids = identities.iter().map(|identity| identity.pid).collect();
        let mut watched =
            matches!(self.reactor.watch_processes(&pids), Ok(watches) if watches.failures == 0);
        let deadline = Instant::now() + grace;
        loop {
            if !identities.iter().any(|identity| self.still_alive(identity)) {
                return;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return;
            }
            // Without a watch on every identity, recheck at the walk rate.
            let wait = if watched {
                remaining
            } else {
                remaining.min(Duration::from_millis(CENSUS_INTERVAL_MS))
            };
            match self.reactor.wait(&[], wait) {
                Ok(_) => {
                    watched &= !self.reactor.pending().process_overflow;
                    if self.reactor.signalled() {
                        return;
                    }
                }
                Err(_) => {
                    provenant_os::sys::wait_pause(remaining);
                    return;
                }
            }
        }
    }

    fn still_alive(&self, identity: &ProcessIdentity) -> bool {
        match self.observer.observe(identity.pid) {
            crate::census::ProcessObservation::Present(fact) => {
                fact.identity == *identity && !fact.zombie
            }
            crate::census::ProcessObservation::Absent => false,
            crate::census::ProcessObservation::Unknown(_) => true,
        }
    }

    /// One summary warning, refreshed in place, for every descendant this
    /// guardian signalled; per-process warnings would crowd out the rest.
    #[cfg(target_os = "macos")]
    fn note_stopped_descendants(&self, warnings: &mut Vec<String>, omitted: &mut u64) {
        if self.stopped_descendants.is_empty() {
            return;
        }
        let label = format!(
            "killed {} surviving descendants",
            self.stopped_descendants.len()
        );
        match warnings.iter_mut().find(|warning| {
            warning.starts_with("killed ") && warning.ends_with(" surviving descendants")
        }) {
            Some(warning) => *warning = label,
            None => push_warning(warnings, omitted, &label),
        }
    }

    /// Spared and protected owners: never signalled, never reaped as ours.
    fn cleanup_spared(&self) -> std::collections::BTreeSet<ProcessIdentity> {
        let mut spared: std::collections::BTreeSet<_> =
            self.spared.spared_identities().cloned().collect();
        spared.extend(self.protected.iter().cloned());
        spared
    }

    /// One signal-free settlement observation over a complete-or-not census:
    /// track sandboxed escapees, reap adopted zombies and prove quiescence.
    /// Returns whether the group settled and the escapees marked this pass.
    fn observe_settlement(
        &mut self,
        child: &mut crate::process_os::provider::ProviderGroup,
        census: &crate::census::Census,
        spared: &std::collections::BTreeSet<ProcessIdentity>,
    ) -> Result<(bool, Vec<ProcessIdentity>)> {
        // A descendant that detached before the census saw it stays in the
        // attempt's sandbox; it is tracked here and ended by the caller.
        #[cfg(target_os = "macos")]
        let marked = self.marked_escapees(census, child.id(), spared);
        #[cfg(not(target_os = "macos"))]
        let marked = Vec::new();
        // Reaping sends no signal, so an adopted zombie is reaped even when
        // descendant stops are not allowed; otherwise a fenced guardian
        // would pin it indefinitely.
        let reaped = child.reap_adopted(census, spared)?;
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
        let outstanding = self
            .tracked
            .iter()
            .any(|identity| match census.process(identity.pid) {
                crate::census::ProcessLookup::Absent => false,
                crate::census::ProcessLookup::Present(fact) => {
                    fact.identity == *identity && !fact.zombie
                }
                crate::census::ProcessLookup::Unknown => true,
            });
        #[cfg(target_os = "macos")]
        let complete = census.is_complete()
            && !reaped.unresolved
            && !outstanding
            && self.darwin_quiescent(child, census)?;
        #[cfg(not(target_os = "macos"))]
        let complete = false;
        Ok((complete, marked))
    }

    /// Retained custody's check between stop passes: one full census and the
    /// same settlement proof as cleanup. It sends no signal and never reads
    /// host graph evidence, so an idle retained guardian costs one census.
    pub(super) fn settle_without_signals(
        &mut self,
        child: &mut crate::process_os::provider::ProviderGroup,
        warnings: &mut Vec<String>,
        omitted: &mut u64,
    ) -> Result<bool> {
        let Some(census) = self.refresh_census(child) else {
            return Ok(false);
        };
        let spared = self.cleanup_spared();
        let (complete, _) = self.observe_settlement(child, &census, &spared)?;
        if complete {
            let _ = child.cleanup(true)?;
        }
        self.persist_tracking(warnings, omitted);
        Ok(complete)
    }

    /// Exit and fork filters for every identity that can still block
    /// settlement. Returns whether all of them are watched (dead identities
    /// need no filter) and whether one was found gone at registration, an
    /// exit no event will report.
    pub(super) fn watch_retained_processes(&mut self) -> (bool, bool) {
        let pids: std::collections::BTreeSet<i64> = self
            .tracked
            .iter()
            .chain(self.history.iter())
            .chain(self.protected.iter())
            .chain(self.spared.spared_identities())
            .map(|identity| identity.pid)
            .collect();
        match self.reactor.watch_processes(&pids) {
            Ok(watches) => (watches.failures == 0, watches.gone),
            Err(_) => (false, false),
        }
    }
}

/// What one retained stop look saw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Look {
    Unavailable,
    Settled,
    Pending,
}

/// The steps of retained custody's repeated stop. Each look is one census.
pub(super) trait RetainedStopPasses {
    fn look(&mut self) -> Result<Look>;
    /// Signal what the last pending look authorised; the live identities signalled.
    fn signal(&mut self, escalation: bool) -> Vec<ProcessIdentity>;
    /// Block until every identity is gone or the grace ends.
    fn await_exits(&mut self, identities: &[ProcessIdentity]);
}

/// TERM, wait, KILL, wait, look: at most three censuses per stop, one when
/// nothing may be signalled. The 20 ms cleanup poll ran about 130 (#3560).
pub(super) fn retained_stop_flow(passes: &mut dyn RetainedStopPasses) -> Result<bool> {
    for escalation in [false, true] {
        match passes.look()? {
            Look::Unavailable => return Ok(false),
            Look::Settled => return Ok(true),
            Look::Pending => {}
        }
        let signalled = passes.signal(escalation);
        if signalled.is_empty() {
            return Ok(false);
        }
        passes.await_exits(&signalled);
    }
    Ok(passes.look()? == Look::Settled)
}

struct DriverPasses<'p, 'a> {
    driver: &'p mut Driver<'a>,
    child: &'p mut crate::process_os::provider::ProviderGroup,
    reason: machine::StopReason,
    grace: Duration,
    warnings: &'p mut Vec<String>,
    omitted: &'p mut u64,
    last: Option<(
        crate::census::Census,
        Vec<ProcessIdentity>,
        std::collections::BTreeSet<ProcessIdentity>,
    )>,
}

impl RetainedStopPasses for DriverPasses<'_, '_> {
    fn look(&mut self) -> Result<Look> {
        let Some(census) = self.driver.refresh_census(self.child) else {
            return Ok(Look::Unavailable);
        };
        let spared = self.driver.cleanup_spared();
        let (complete, marked) = self
            .driver
            .observe_settlement(self.child, &census, &spared)?;
        if complete {
            let _ = self.child.cleanup(true)?;
            self.driver.persist_tracking(self.warnings, self.omitted);
            return Ok(Look::Settled);
        }
        self.last = Some((census, marked, spared));
        Ok(Look::Pending)
    }

    fn signal(&mut self, escalation: bool) -> Vec<ProcessIdentity> {
        let Some((census, marked, spared)) = self.last.take() else {
            return Vec::new();
        };
        let mut signalled = self
            .driver
            .signal_group(
                self.child,
                &census,
                self.reason,
                escalation,
                self.warnings,
                self.omitted,
            )
            .unwrap_or_default();
        for identity in self.driver.stop_pass(
            self.child,
            &census,
            &spared,
            marked,
            escalation,
            self.warnings,
            self.omitted,
        ) {
            if !signalled.contains(&identity) {
                signalled.push(identity);
            }
        }
        self.driver.persist_tracking(self.warnings, self.omitted);
        signalled
    }

    fn await_exits(&mut self, identities: &[ProcessIdentity]) {
        self.driver.await_exits(identities, self.grace);
    }
}

/// Unknown host graph state refuses escapee signals. This is intentionally
/// separate from census: an ordinary history walk must not reopen graph state.
#[cfg(any(target_os = "macos", test))]
pub(super) fn host_allows_escapee_stop(holder: Option<&dyn Holder>, context: &[u8]) -> bool {
    holder.is_none_or(|holder| matches!(holder.has_live_children(context), Ok(false)))
}

/// Graph evidence is read only at a signalling decision that has candidates,
/// so a cleanup pass with nothing to signal never reopens host graph state.
/// Each read decodes every pending graph checkpoint; at the 20 ms cleanup
/// tick that decode dominated retained guardians (#3560).
#[cfg(any(target_os = "macos", test))]
pub(super) fn host_authorised_targets(
    holder: Option<&dyn Holder>,
    context: &[u8],
    candidates: std::collections::BTreeSet<ProcessIdentity>,
) -> Vec<ProcessIdentity> {
    if candidates.is_empty() || !host_allows_escapee_stop(holder, context) {
        return Vec::new();
    }
    candidates.into_iter().collect()
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

    #[test]
    fn cleanup_reads_graph_evidence_only_for_a_signalling_decision() {
        let candidate = ProcessIdentity {
            boot: "boot".into(),
            pid: 20,
            native_start: 2,
            group_id: 20,
            session_id: 20,
        };
        for live in [Some(false), Some(true), None] {
            let holder = GraphHolder {
                live,
                reads: Cell::new(0),
            };
            // Nothing to signal: retained cleanup passes never decode graphs.
            for _ in 0..100 {
                assert!(
                    host_authorised_targets(Some(&holder), b"attempt", Default::default())
                        .is_empty()
                );
            }
            assert_eq!(holder.reads.get(), 0);
            let targets = host_authorised_targets(
                Some(&holder),
                b"attempt",
                std::collections::BTreeSet::from([candidate.clone()]),
            );
            assert_eq!(holder.reads.get(), 1);
            assert_eq!(targets.is_empty(), live != Some(false));
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
