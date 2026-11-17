//! Settlement snapshots of the shared handoff, independent of provider history.
use crate::{
    artifacts::{read_sealed, seal},
    *,
};
use provenant_os::fs::Directory;
use provenant_records::{
    AttemptFile,
    handoff::{HANDOFF_BYTES, Handoff, HandoffError, HandoffResult},
    run::{PathScope, WriteSource},
};
use std::{
    ffi::OsStr,
    io::Write,
    path::{Component, Path},
};

pub(crate) fn guidance() -> &'static str {
    "Work until the brief is finished. Do not end your turn because a slice is done, a commit is a natural checkpoint or the work is large; start the next item. Ending your turn ends your work, so a report of what remains is not progress. End your final answer with exactly one fenced handoff block, with nothing after it. Use schema_version 2 and status done, blocked or stopped_early; include note, remaining[], decisions[] and verify[] (verification performed and results). When a criterion is wrong, add spec_concerns entries with criterion, claim, evidence, proposed_change and status (as_written, diverged or stopped); disclose any divergence. A write scope names the expected files, not a fence: when the fix needs another file, edit it minimally and list each such path in decisions; only a strict WRITE_SCOPE lane policy refuses those paths at settlement. Use blocked only for a real boundary such as credentials, another repository or shared Git history. Use done only when the brief is finished. Use blocked when you need an owner decision, lack a capability required by the brief, or need an external change you cannot make yourself. These cases must not use stopped_early. Use stopped_early only when you cannot carry on in this session, such as running out of context, and never to report work you could do now. The JSON is at most 12288 bytes; note and each array entry are 1..4096 bytes, each array at most 64 strings. Example:\n```handoff\n{\"schema_version\":2,\"status\":\"done\",\"note\":\"what was done\",\"remaining\":[],\"decisions\":[],\"verify\":[\"check and result\"]}\n```"
}

impl RunStore {
    /// Read the host's immutable settlement snapshot, never provider history.
    pub fn handoff(&self, record: &AttemptRecord) -> Result<HandoffResult> {
        let attempt = self.attempt_dir(record.attempt_id, false)?;
        match read_snapshot(&attempt)? {
            Some(handoff) => with_scope_report(&attempt, handoff),
            None => Ok(Ok(None)),
        }
    }

    pub(crate) fn capture_handoff(
        &self,
        attempt: &Directory,
        record: &AttemptRecord,
        decoded: &Decoded,
    ) -> Result<provenant_records::handoff::HandoffParse> {
        let parsed = decoded.handoff_report();
        if let Some(repeated) =
            provenant_records::handoff::Handoff::repeated_blocks(&decoded.result)
            && parsed.handoff.as_ref().is_ok_and(Option::is_some)
        {
            // Accepted either way: the lane did its work and declared it.
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: attempt {}: {}",
                record.attempt_id,
                repeated.warning()
            );
        }
        if let Some(fallback) = provenant_records::handoff::Handoff::json_fence_fallback(
            &decoded.result,
            &decoded.turns,
        ) {
            // Accepted: the answer ended in a handoff-shaped json block.
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: attempt {}: {}",
                record.attempt_id,
                fallback.warning()
            );
        }
        if let Some(handoff) = read_snapshot(attempt)? {
            let handoff = with_scope_report(attempt, handoff)?;
            return Ok(provenant_records::handoff::HandoffParse {
                trailing_verdict_tolerated: parsed.trailing_verdict_tolerated
                    && handoff.as_ref().is_ok_and(Option::is_some),
                handoff,
            });
        }
        let interrupted = !decoded.complete
            || matches!(
                decoded.outcome,
                Outcome::QuotaLimited
                    | Outcome::ModelUnavailable
                    | Outcome::Failed
                    | Outcome::TimedOut
                    | Outcome::Lost
            );
        let handoff = match parsed.handoff {
            Ok(None) if interrupted => self
                .progress_handoff(attempt, record)
                .unwrap_or_else(|_| Err(HandoffError::new("$", "progress handoff unreadable"))),
            // A malformed final declaration is evidence even on interruption.
            // Never substitute an older progress declaration for it.
            handoff => handoff,
        };
        let handoff = sanitise_handoff_result(handoff);
        if handoff != Ok(None) {
            seal(
                attempt,
                AttemptFile::Handoff.name(),
                &serde_json::to_vec(&handoff)
                    .map_err(|_| RunError::Validation("handoff encoding"))?,
                HANDOFF_RECORD_BYTES as u64,
            )?;
        }
        let trailing_verdict_tolerated =
            parsed.trailing_verdict_tolerated && handoff.as_ref().is_ok_and(Option::is_some);
        Ok(provenant_records::handoff::HandoffParse {
            handoff,
            trailing_verdict_tolerated,
        })
    }

    fn progress_handoff(
        &self,
        attempt: &Directory,
        record: &AttemptRecord,
    ) -> Result<HandoffResult> {
        let expected = self
            .context
            .state_root
            .as_path()
            .join("attempts")
            .join(record.attempt_id.to_string())
            .join("provider-state");
        let mut grants = record
            .write_grants
            .iter()
            .filter(|grant| grant.source == WriteSource::PrivateState);
        let Some(grant) = grants.next() else {
            return Ok(Ok(None));
        };
        if grants.next().is_some() || grant.scope != PathScope::Subtree {
            return Ok(Ok(None));
        }
        // The applied grant pins the launched candidate. Never scan other
        // candidates, follow paths from model text or open provider credentials.
        let granted = grant.path.to_path_buf()?;
        let relative = match granted.strip_prefix(&expected) {
            Ok(relative) => relative,
            Err(_) => return Ok(Ok(None)),
        };
        let mut parts = relative.components();
        let Some(Component::Normal(candidate)) = parts.next() else {
            return Ok(Ok(None));
        };
        if parts.next().is_some() {
            return Ok(Ok(None));
        }
        let state = match attempt.open_verified_directory(OsStr::new("provider-state")) {
            Ok(state) => state,
            Err(error) if error.is_not_found() => return Ok(Ok(None)),
            Err(error) => return Err(error.into()),
        };
        let candidate = match state.open_verified_directory(candidate) {
            Ok(candidate) => candidate,
            Err(error) if error.is_not_found() => return Ok(Ok(None)),
            Err(error) => return Err(error.into()),
        };
        let Some(bytes) = read_sealed(&candidate, "handoff.json", HANDOFF_BYTES as u64)? else {
            return Ok(Ok(None));
        };
        Ok(Handoff::from_json(&bytes).map(Some))
    }
}

/// Redact decoded handoff values before a graph consumes local or peer output.
pub fn sanitise_handoff(handoff: &Handoff) -> Result<Handoff> {
    transform_handoff(handoff, |text| {
        let safe = provenant_confinement::redact::known_shapes(text.as_bytes())
            .map_err(|_| RunError::Validation("handoff redaction failed"))?;
        String::from_utf8(safe).map_err(|_| RunError::Validation("handoff UTF-8"))
    })
}

/// Attach the sealed write-scope evidence to the in-memory handoff view. The
/// original `handoff.json` is an immutable provider snapshot, so derived scope
/// facts are never resealed over it. Oversized full reports fall back to a
/// pointer to the artifact; if that also exceeds the handoff bound, return the
/// validation error so the scope evidence is never silently omitted.
/// A sealed declaration with the host's scope evidence attached; malformed or
/// absent outcomes pass through unchanged.
fn with_scope_report(attempt: &Directory, handoff: HandoffResult) -> Result<HandoffResult> {
    Ok(match handoff {
        Ok(Some(handoff)) => Ok(Some(attach_scope_report(attempt, &handoff)?)),
        other => other,
    })
}

pub(crate) fn attach_scope_report(attempt: &Directory, handoff: &Handoff) -> Result<Handoff> {
    let Some(raw) = read_sealed(
        attempt,
        "policy-write-scope.json",
        provenant_records::run::TERMINAL_RECORD_LIMIT,
    )?
    else {
        return Ok(handoff.clone());
    };
    let report: provenant_records::handoff::WriteScopeReport = crate::db::decode(&raw)?;
    let mut full = handoff.clone();
    full.write_scope = Some(report.clone());
    if let Ok(safe) = sanitise_handoff(&full) {
        return Ok(safe);
    }

    let mut compact = handoff.clone();
    compact.write_scope = Some(report.compact());
    sanitise_handoff(&compact)
}

fn walk_handoff_value(
    value: &mut serde_json::Value,
    transform: &mut impl FnMut(&str) -> Result<String>,
) -> Result<()> {
    match value {
        serde_json::Value::String(text) => *text = transform(text)?,
        serde_json::Value::Array(items) => {
            for item in items {
                walk_handoff_value(item, transform)?;
            }
        }
        serde_json::Value::Object(fields) => {
            for (key, mut value) in std::mem::take(fields) {
                let key = transform(&key)?;
                walk_handoff_value(&mut value, transform)?;
                if fields.insert(key, value).is_some() {
                    return Err(RunError::Validation("handoff field collision"));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// Transform decoded values and object keys, including nested forward fields,
/// then revalidate both field and total bounds after any replacement growth.
pub(crate) fn transform_handoff(
    handoff: &Handoff,
    mut transform: impl FnMut(&str) -> Result<String>,
) -> Result<Handoff> {
    let mut value =
        serde_json::to_value(handoff).map_err(|_| RunError::Validation("handoff encoding"))?;
    walk_handoff_value(&mut value, &mut transform)?;
    let bytes = serde_json::to_vec(&value).map_err(|_| RunError::Validation("handoff encoding"))?;
    Handoff::from_json(&bytes).map_err(RunError::Handoff)
}

// The envelope includes a typed error and a retained partial object, in
// addition to the bounded provider payload. This is not the input byte cap.
const HANDOFF_RECORD_BYTES: usize = HANDOFF_BYTES * 4;

fn read_snapshot(attempt: &Directory) -> Result<Option<HandoffResult>> {
    read_sealed(
        attempt,
        AttemptFile::Handoff.name(),
        HANDOFF_RECORD_BYTES as u64,
    )?
    .map(|bytes| {
        let handoff: HandoffResult = serde_json::from_slice(&bytes)
            .map_err(|_| RunError::Validation("handoff snapshot unreadable"))?;
        Ok(match handoff {
            Ok(Some(handoff)) => Handoff::from_json(
                &serde_json::to_vec(&handoff)
                    .map_err(|_| RunError::Validation("handoff encoding"))?,
            )
            .map(Some),
            other => other,
        })
    })
    .transpose()
}

/// Errors and retained fields are provider output too: redact their keys and
/// values before persistence or disclosure, just as for a complete handoff.
pub fn sanitise_handoff_result(handoff: HandoffResult) -> HandoffResult {
    match handoff {
        Ok(Some(handoff)) => sanitise_handoff(&handoff)
            .map(Some)
            .map_err(|error| match error {
                RunError::Handoff(error) => error,
                _ => HandoffError::new("$", "handoff redaction failed"),
            }),
        Ok(None) => Ok(None),
        Err(error) => {
            let mut value = serde_json::json!(error);
            let safe = walk_handoff_value(&mut value, &mut |text: &str| {
                let safe = provenant_confinement::redact::known_shapes(text.as_bytes())
                    .map_err(|_| RunError::Validation("handoff error redaction failed"))?;
                String::from_utf8(safe).map_err(|_| RunError::Validation("handoff error UTF-8"))
            });
            if safe.is_err() {
                return Err(HandoffError::new("$", "handoff error redaction failed"));
            }
            Err(serde_json::from_value(value)
                .unwrap_or_else(|_| HandoffError::new("$", "handoff error redaction failed")))
        }
    }
}

/// The verification and slot-wait rules every writer lane is told. A writer's `cargo` commands
/// that compile share the host's build cap, and a lane that
/// stopped to report the wait cost a whole task (#2484).
pub const BUILD_SLOT_GUIDANCE: &str = "Verification: where the repository has CI, CI runs the tests and lints on pull requests and the integrated branch, not on every push; the CI scope line below says what CI, if any, you wait for. Run at most one cheap compile check (`cargo check` on the one package you changed), then commit and push; do not run test suites, clippy or nextest locally, and do not restart a build that was stopped. The repository's test tiers (T0 to T3) describe what CI and a configured host run; they are not a checklist for you. A `cargo` command the host refuses because CI runs it is final: do not look for another way to run it.\nBuild slots: compiling `cargo` commands share a host-wide cap (source: default; domains.fabric.lanes.max_concurrent_builds), derived from cores and memory. Busy or unavailable slots wait a bounded time, then warn and the build proceeds. 0 disables the cap. Cargo manages its own build locks.\n";

/// Host build cap stated in the preamble.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuildCap {
    pub slots: u64,
    pub source: &'static str,
}

/// Optional preamble overrides. Default keeps the historical verification paragraph.
#[derive(Clone, Debug, Default)]
pub struct PreambleExtras {
    pub verification: Option<crate::VerificationResolution>,
    pub build_cap: Option<BuildCap>,
}

fn slot_guidance(cap: Option<BuildCap>) -> String {
    match cap {
        Some(cap) => format!(
            "Build slots: compiling `cargo` commands share a host-wide cap of {} (source: {}; domains.fabric.lanes.max_concurrent_builds). Busy or unavailable slots wait a bounded time, then warn and the build proceeds. 0 disables the cap. Cargo manages its own build locks.\n",
            cap.slots, cap.source
        ),
        None => BUILD_SLOT_GUIDANCE
            .split_once('\n')
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_default(),
    }
}

/// The reading rule every lane is told. Tool output stays in a lane's context and is
/// paid for again on every later turn, so whole-file and large-range reads dominate its cost.
pub const READING_GUIDANCE: &str = "Reading: locate before you read. Search with line numbers (`rg -n`), then read a bounded range of the lines you need; avoid whole-file `cat` or `sed` reads and unbounded `git diff` or `rg` output. Everything you print stays in your context and is paid for again on every later turn.\n";

/// The text placed before a lane's task: model policy, then for writers
/// (`ci_scope` is `Some`) the build-slot rule, the CI scope and the gh
/// budget, then the handoff file and the reading rule.
pub fn lane_preamble(
    model_guidance: &str,
    limits: &provenant_records::LaneLimits,
    ci_scope: Option<crate::ResolvedCiScope>,
    handoff: Option<&Path>,
) -> String {
    lane_preamble_with(
        model_guidance,
        limits,
        ci_scope,
        handoff,
        &PreambleExtras::default(),
    )
}

fn lane_preamble_with(
    model_guidance: &str,
    limits: &provenant_records::LaneLimits,
    ci_scope: Option<crate::ResolvedCiScope>,
    handoff: Option<&Path>,
    extras: &PreambleExtras,
) -> String {
    let writer = ci_scope
        .map(|scope| {
            let budget = limits
                .gh_invocations
                .as_ref()
                .map(|limit| {
                    format!(
                        "GitHub calls: gh commands through your gh wrapper are counted, with a warning after {} (source: {}; lanes.gh_budget). They spend the user's API quota: batch reads, do not poll, and prefer one gh api graphql query over many calls. The warning never stops a command.\n",
                        limit.value,
                        limit.source.as_str()
                    )
                })
                .unwrap_or_default();
            let verification = extras
                .verification
                .as_ref()
                .map(crate::VerificationResolution::paragraph)
                .unwrap_or_else(|| crate::verification::CHECK_PARAGRAPH.to_string());
            format!(
                "{verification}{}{}{budget}",
                slot_guidance(extras.build_cap),
                scope.preamble()
            )
        })
        .unwrap_or_default();
    format!(
        "{model_guidance}\n{}{writer}{}{READING_GUIDANCE}",
        limits.preamble(),
        handoff.map(handoff_progress_guidance).unwrap_or_default()
    )
}

/// Effective guards are rendered by the same table used by enforcement.
pub fn lane_preamble_with_policy(
    model_guidance: &str,
    limits: &provenant_records::LaneLimits,
    ci_scope: Option<crate::ResolvedCiScope>,
    handoff: Option<&Path>,
    policy: &provenant_records::policy::Policy,
    extras: &PreambleExtras,
) -> String {
    format!(
        "{}Launch policy (code, boundary, strength, source, knob):\n{}",
        lane_preamble_with(model_guidance, limits, ci_scope, handoff, extras),
        policy.render()
    )
}

pub fn handoff_progress_guidance(path: &Path) -> String {
    format!(
        "Progress handoff JSON: {}. Update this file throughout the task by writing a temporary file beside it and renaming it into place (hard links are denied), and repeat its current value in the final handoff fence. A failed update never blocks the work: carry on and report the value in the fence. It is lane state, never a repository file or a commit.\n",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{call, clock, context, exit, input, publication, seal_observation};
    use provenant_records::AbsolutePath;
    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn guidance_treats_write_scope_as_advisory_outside_strict_policy() {
        let text = guidance();
        assert!(text.contains("not a fence"));
        assert!(text.contains("list each such path in decisions"));
        assert!(text.contains("strict WRITE_SCOPE lane policy"));
        assert!(!text.contains("keep it inside the approved write scope"));
    }

    #[test]
    fn writer_preamble_states_advisory_build_policy_and_read_only_does_not() {
        let handoff = Path::new("/state/handoff.json");
        let writer = lane_preamble(
            "Model policy.",
            &Default::default(),
            Some(crate::ResolvedCiScope::resolve(&Default::default())),
            Some(handoff),
        );
        assert!(writer.contains(crate::verification::CHECK_PARAGRAPH));
        assert!(writer.contains("source: default"));
        assert!(writer.contains("Build slots:"));
        assert!(writer.contains("CI scope:"));
        assert!(writer.contains(&format!("Progress handoff JSON: {}", handoff.display())));
        let reader = lane_preamble("Model policy.", &Default::default(), None, Some(handoff));
        assert!(!reader.contains("Build slots"));
        assert!(!reader.contains("CI scope:"));
        assert!(reader.contains("Progress handoff JSON:"));
        assert!(
            !lane_preamble("Model policy.", &Default::default(), None, None)
                .contains("handoff.json")
        );
    }

    fn snapshot(status: &str) -> Vec<u8> {
        serde_json::json!({"schema_version":2,"status":status,"note":"Parser committed; renderer remains.","remaining":["Finish renderer"],"decisions":["Keep the parser API"],"verify":["Parser tests passed"]}).to_string().into_bytes()
    }
    fn decoded() -> Decoded {
        Decoded {
            terminal_cause: None,
            outcome: Outcome::QuotaLimited,
            result: String::new(),
            complete: false,
            observed_route: None,
            usage: Default::default(),
            session: None,
            reason: None,
            partial_signals: vec![],
            turns: Vec::new(),
        }
    }
    fn grant(record: &mut AttemptRecord, candidate: &Path) -> TestResult {
        record.write_grants = vec![WriteGrantRecord {
            extra: Default::default(),
            source: WriteSource::PrivateState,
            path: AbsolutePath::new(candidate)?,
            scope: PathScope::Subtree,
        }];
        Ok(())
    }

    #[test]
    fn scope_report_falls_back_to_its_artifact_without_replacing_snapshot() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let record = store.record(input.attempt_id, &call)?;
        let attempt = store.attempt_dir(record.attempt_id, false)?;
        let raw = snapshot("done");
        let original = Handoff::from_json(&raw).map_err(std::io::Error::other)?;
        seal(
            &attempt,
            AttemptFile::Handoff.name(),
            &serde_json::to_vec(&Ok::<_, HandoffError>(Some(original.clone())))?,
            HANDOFF_RECORD_BYTES as u64,
        )?;
        let mut paths = Vec::new();
        for index in 0..HANDOFF_BYTES / 32 {
            let relative = provenant_core::RelativePath::parse(format!(
                "outside/{index}/{}",
                "x".repeat(128)
            ))?;
            paths.push(provenant_records::PathWire::from_relative(&relative)?);
        }
        let path_count = paths.len();
        let report = provenant_records::handoff::WriteScopeReport::Full {
            schema_version: 1,
            code: "WRITE_SCOPE".into(),
            base: "1".repeat(40),
            head: "2".repeat(40),
            strength: provenant_records::policy::Strength::Warn,
            source: provenant_records::policy::Source::Project,
            out_of_scope_paths: paths,
        };
        seal(
            &attempt,
            "policy-write-scope.json",
            &crate::db::bytes(&report)?,
            provenant_records::run::TERMINAL_RECORD_LIMIT,
        )?;

        let attached = attach_scope_report(&attempt, &original)?;
        assert!(matches!(
            attached.write_scope,
            Some(provenant_records::handoff::WriteScopeReport::Compact {
                out_of_scope_count,
                paths_omitted_from_handoff: true,
                ..
            }) if out_of_scope_count == path_count
        ));
        assert_eq!(read_snapshot(&attempt)?, Some(Ok(Some(original))));
        assert_eq!(store.handoff(&record)??, Some(attached));
        Ok(())
    }

    #[test]
    fn read_only_plan_progress_is_collected_at_settlement() -> TestResult {
        use provenant_confinement::{
            ConfinedPlan, PlanRequest,
            permissions::{AccessMode, NetworkAccess, PathGrant, PlanPermissions, ProviderProfile},
            plan::{CodexPolicy, ExtensionPolicy},
        };
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let mut record = store.record(input.attempt_id, &call)?;
        assert!(input.request.task.access.write_roots.is_empty());
        let attempt = store.attempt_dir(record.attempt_id, false)?;
        let state = crate::artifacts::subdir(&attempt, OsStr::new("provider-state"), true)?;
        let candidate = crate::artifacts::subdir(&state, OsStr::new("candidate-0"), true)?;
        let base = store
            .context
            .state_root
            .as_path()
            .join(format!("attempts/{}", record.attempt_id));
        let subtree = |path| PathGrant {
            path,
            scope: PathScope::Subtree,
        };
        let plan = ConfinedPlan::new(PlanRequest {
            scope: provenant_core::WorkScope::Project(store.context.project_id),
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
            permissions: PlanPermissions {
                credential_roots: provenant_core::CredentialRoots::for_home(
                    &temp.path().join("home"),
                ),
                policy: None,
                read_only: None,
                mode: AccessMode::ReadOnly,
                scratch: subtree(base.join("tmp/candidate-0")),
                private_state: Some(subtree(base.join("provider-state/candidate-0"))),
                read_roots: vec![],
                protected_roots: vec![],
                secret_roots: vec![],
                selected_authentication: None,
                include_continuation: false,
                write_roots: vec![],
                network: NetworkAccess::Provider,
                nested_confinement: false,
            },
        })?;
        let profile = ProviderProfile {
            spec: Default::default(),
            observations: Default::default(),
        };
        record.write_grants = provenant_confinement::write_grants(plan.permissions(), &profile)
            .into_iter()
            .map(|grant| {
                Ok(WriteGrantRecord {
                    extra: Default::default(),
                    source: grant.source,
                    path: AbsolutePath::new(&grant.path)?,
                    scope: grant.scope,
                })
            })
            .collect::<std::result::Result<Vec<_>, Box<dyn std::error::Error>>>()?;
        let bytes = snapshot("stopped_early");
        seal(&candidate, "handoff.json", &bytes, HANDOFF_BYTES as u64)?;
        let expected = Handoff::from_json(&bytes)?;
        assert_eq!(
            store.progress_handoff(&attempt, &record)??,
            Some(expected.clone())
        );
        assert_eq!(
            store
                .capture_handoff(&attempt, &record, &decoded())?
                .handoff?,
            Some(expected.clone())
        );
        assert_eq!(store.handoff(&record)??, Some(expected));
        Ok(())
    }

    /// Provider boundary for exercising holder recovery when this host cannot
    /// launch nested confinement. No launch or Git settlement is simulated.
    struct RecoveringAccounts;
    impl Providers for RecoveringAccounts {
        fn prepare(
            &self,
            _: &TaskRequest,
            _: &str,
            _: &LaunchPaths,
            _: Option<&AttemptRecord>,
        ) -> std::result::Result<PreparedLaunch, Box<crate::PrepareRefusal>> {
            Err(crate::fixture_refusal(
                "holder-only fixture does not launch",
            ))
        }
        fn decode(
            &self,
            route: &RouteRecord,
            _: &[u8],
            _: &provenant_supervisor::custody::ExitObservation,
            _: &Path,
        ) -> std::result::Result<Decoded, String> {
            let mut result = decoded();
            match route.account_key.as_deref() {
                // The provider died before declaring anything: an absent final
                // declaration, so settlement falls back to granted progress. A
                // truncated fence is malformed evidence instead.
                Some("fixture-one") => result.result = "Working on the renderer".into(),
                Some("fixture-two") => {
                    result.outcome = Outcome::Succeeded;
                    result.complete = true;
                    result.result = "Recovered progress and completed the renderer\n```handoff\n{\"schema_version\":2,\"status\":\"done\",\"remaining\":[],\"decisions\":[],\"verify\":[],\"note\":\"finished\"}\n```".into();
                    result.observed_route = Some(route.clone());
                }
                _ => return Err("unexpected fixture account".into()),
            }
            Ok(result)
        }
    }

    #[test]
    fn provider_death_retains_sanitised_progress() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let mut input = input();
        let worktree = temp.path().join("retained-worktree");
        std::fs::create_dir(&worktree)?;
        std::fs::write(
            worktree.join("partial.txt"),
            "parser implementation survives",
        )?;
        input.request.task.access.write_roots.push(worktree.clone());
        store.create(&input, &call)?;
        seal_observation(&store, &input, &exit())?;
        let id = input.attempt_id;
        let mut old = store.record(id, &call)?;
        let attempt = store.attempt_dir(id, false)?;
        let root = crate::artifacts::subdir(&attempt, OsStr::new("provider-state"), true)?;
        let candidate = crate::artifacts::subdir(&root, OsStr::new("candidate-1"), true)?;
        let secret_shape = "sk-abcdefghijklmnopqrstuvwx";
        let mut progress: serde_json::Value = serde_json::from_slice(&snapshot("stopped_early"))?;
        progress["note"] = serde_json::json!(format!(
            "Parser committed; renderer remains.\n{secret_shape}"
        ));
        for field in ["remaining", "decisions", "verify"] {
            progress[field]
                .as_array_mut()
                .ok_or("progress array")?
                .extend([
                    serde_json::json!(format!("\n{secret_shape}")),
                    serde_json::json!(format!("\t{secret_shape}")),
                ]);
        }
        progress["future"] = serde_json::json!({"nested":[format!("\n{secret_shape}"), {"token":format!("\t{secret_shape}")} ]});
        seal(
            &candidate,
            "handoff.json",
            &serde_json::to_vec(&progress)?,
            HANDOFF_BYTES as u64,
        )?;
        let candidate_path = store
            .context
            .state_root
            .as_path()
            .join(format!("attempts/{id}/provider-state/candidate-1"));
        grant(&mut old, &candidate_path)?;
        let route = RouteRecord {
            account_label: Some("one".into()),
            account_key: Some("fixture-one".into()),
            ..crate::tests::route()
        };
        old.resolved_route = Some(route.clone());
        old.applied_route = Some(route);
        old.provider_launched = Some(true);
        old.state = AttemptState::Running {
            extra: Default::default(),
            child_wait: None,
        };
        store.db.execute(
            "UPDATE attempts SET record=?1 WHERE id=?2",
            rusqlite::params![crate::db::bytes(&old)?, id.to_string()],
        )?;
        let terminal = store.finish(id, &attempt, &exit(), &RecoveringAccounts, &call)?;
        assert!(matches!(
            terminal.state,
            AttemptState::Terminal {
                outcome: Outcome::QuotaLimited,
                ..
            }
        ));
        let handoff = store.handoff(&terminal)??.ok_or("missing snapshot")?;
        assert_eq!(
            handoff.remaining,
            ["Finish renderer", "\n[REDACTED]", "\t[REDACTED]"]
        );
        assert_eq!(
            handoff.extra["future"]["nested"][1]["token"],
            "\t[REDACTED]"
        );
        assert!(!handoff.fenced().contains(secret_shape));
        assert!(handoff.fenced().contains("[REDACTED]"));
        // Progress survives temporary cleanup and provider history expiry.
        crate::artifacts::remove_entry_tree(&attempt, OsStr::new("provider-state"))?;
        assert_eq!(store.handoff(&old)??, Some(handoff.clone()));
        assert_eq!(
            std::fs::read_to_string(worktree.join("partial.txt"))?,
            "parser implementation survives"
        );
        Ok(())
    }
    #[test]
    fn final_handoff_redacts_decoded_strings_and_revalidates_bounds() -> TestResult {
        for prefix in ["\n", "\t"] {
            for field in ["note", "remaining", "decisions", "verify", "future"] {
                let temp = tempfile::tempdir()?;
                let clock = clock();
                let cancel = provenant_core::CancelToken::new();
                let call = call(&clock, &cancel);
                let store = RunStore::open(&context(temp.path())?, &call)?;
                let input = input();
                store.create(&input, &call)?;
                let record = store.record(input.attempt_id, &call)?;
                let attempt = store.attempt_dir(record.attempt_id, false)?;
                let secret = format!("{prefix}sk-abcdefghijklmnopqrstuvwx");
                let mut value: serde_json::Value = serde_json::from_slice(&snapshot("done"))?;
                value[field] = match field {
                    "note" => serde_json::json!(secret),
                    "future" => {
                        serde_json::json!({secret.clone():[secret.clone(),{"value":secret}]})
                    }
                    _ => serde_json::json!([secret]),
                };
                let mut result = decoded();
                result.complete = true;
                result.outcome = Outcome::Succeeded;
                result.result = Handoff::from_json(&serde_json::to_vec(&value)?)?.fenced();
                let safe = store
                    .capture_handoff(&attempt, &record, &result)?
                    .handoff?
                    .ok_or("final snapshot")?;
                assert!(!safe.fenced().contains("sk-abcdefghijklmnopqrstuvwx"));
                assert!(safe.fenced().contains("[REDACTED]"));
                assert_eq!(store.handoff(&record)??, Some(safe));
            }
        }
        // Redacting short URL credentials expands the note past its byte cap.
        let url = "\nhttps://a:b@h";
        let note = format!(
            "{}{url}",
            "x".repeat(provenant_records::handoff::NOTE_BYTES - url.len())
        );
        let mut handoff = Handoff::from_json(&snapshot("done"))?;
        handoff.note = note;
        let redact = |text: &str| {
            let safe = provenant_confinement::redact::known_shapes(text.as_bytes())
                .map_err(|_| RunError::Validation("redaction"))?;
            String::from_utf8(safe).map_err(|_| RunError::Validation("UTF-8"))
        };
        let invalid = transform_handoff(&handoff, redact);
        assert!(matches!(invalid, Err(RunError::Handoff(_))));
        let mut value: serde_json::Value = serde_json::from_slice(&snapshot("done"))?;
        value["future"] = serde_json::json!({"url":url,"padding":""});
        let padding = HANDOFF_BYTES - serde_json::to_vec(&value)?.len();
        value["future"]["padding"] = serde_json::json!("x".repeat(padding));
        let handoff = Handoff::from_json(&serde_json::to_vec(&value)?)?;
        assert!(matches!(
            transform_handoff(&handoff, redact),
            Err(RunError::Handoff(_))
        ));
        Ok(())
    }

    // Spec: docs/specs/run-settlement.md#settlement
    #[test]
    fn final_handoff_records_the_single_trailing_verdict_tolerance() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let record = store.record(input.attempt_id, &call)?;
        let attempt = store.attempt_dir(record.attempt_id, false)?;
        let mut result = decoded();
        result.complete = true;
        result.outcome = Outcome::Succeeded;
        result.result = format!(
            "{}\nVERDICT: PASS\n",
            Handoff::from_json(&snapshot("done"))?.fenced()
        );

        let captured = store.capture_handoff(&attempt, &record, &result)?;
        assert!(captured.trailing_verdict_tolerated);
        assert!(captured.handoff.as_ref().is_ok_and(Option::is_some));
        assert_eq!(store.handoff(&record)?, captured.handoff);
        Ok(())
    }

    #[test]
    fn oversized_handoff_brief_does_not_block_settlement() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let mut input = input();
        input.prompt = "x".repeat(RECORD_LIMIT as usize - 64);
        input.request.task.prompt_digest =
            provenant_records::CanonicalV1::file_digest(input.prompt.as_bytes());
        store.create(&input, &call)?;
        seal_observation(&store, &input, &exit())?;
        let id = input.attempt_id;
        let old = store.record(id, &call)?;
        let attempt = store.attempt_dir(id, false)?;
        let mut result = decoded();
        result.result = Handoff::from_json(&snapshot("stopped_early"))?.fenced();
        store.capture_handoff(&attempt, &old, &result)?.handoff?;
        store
            .db
            .execute("DELETE FROM admission WHERE attempt=?1", [id.to_string()])?;
        let mut publication = publication();
        publication.outcome = Outcome::QuotaLimited;
        let terminal = store.publish_terminal(id, 1, &publication, &exit(), &call)?;
        assert!(matches!(
            terminal.state,
            AttemptState::Terminal {
                outcome: Outcome::QuotaLimited,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn final_answer_precedes_progress_and_bad_progress_never_guesses_completion() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let mut record = store.record(input.attempt_id, &call)?;
        let attempt = store.attempt_dir(record.attempt_id, false)?;
        let state = crate::artifacts::subdir(&attempt, OsStr::new("provider-state"), true)?;
        let candidate = crate::artifacts::subdir(&state, OsStr::new("candidate-0"), true)?;
        let base = store
            .context
            .state_root
            .as_path()
            .join(format!("attempts/{}/provider-state", record.attempt_id));
        grant(&mut record, &base.join("candidate-0"))?;
        assert!(
            store
                .capture_handoff(&attempt, &record, &decoded())?
                .handoff?
                .is_none()
        );
        seal(
            &candidate,
            "handoff.json",
            &snapshot("stopped_early"),
            HANDOFF_BYTES as u64,
        )?;
        // Another candidate's progress is not the applied candidate's evidence.
        grant(&mut record, &base.join("candidate-1"))?;
        assert!(
            store
                .capture_handoff(&attempt, &record, &decoded())?
                .handoff?
                .is_none()
        );
        grant(&mut record, &base.join("candidate-0"))?;
        let mut result = decoded();
        result.outcome = Outcome::Succeeded;
        result.complete = true;
        result.result = "Completed prose without a declaration".into();
        assert!(
            store
                .capture_handoff(&attempt, &record, &result)?
                .handoff?
                .is_none()
        );
        let done = Handoff::from_json(&snapshot("done"))?.fenced();
        let example = format!("````markdown\n{done}\n````");
        result.result = example.clone();
        assert!(
            store
                .capture_handoff(&attempt, &record, &result)?
                .handoff?
                .is_none()
        );
        result.result = format!("{example}\n{done}");
        assert!(
            !store
                .capture_handoff(&attempt, &record, &result)?
                .handoff?
                .ok_or("missing final")?
                .unfinished()
        );
        Ok(())
    }

    #[test]
    fn unsafe_or_oversize_progress_is_a_typed_error_without_following_links() -> TestResult {
        use std::os::unix::fs::symlink;
        for link in [true, false] {
            let temp = tempfile::tempdir()?;
            let clock = clock();
            let cancel = provenant_core::CancelToken::new();
            let call = call(&clock, &cancel);
            let store = RunStore::open(&context(temp.path())?, &call)?;
            let input = input();
            store.create(&input, &call)?;
            let mut record = store.record(input.attempt_id, &call)?;
            let attempt = store.attempt_dir(record.attempt_id, false)?;
            let state = crate::artifacts::subdir(&attempt, OsStr::new("provider-state"), true)?;
            let candidate = crate::artifacts::subdir(&state, OsStr::new("candidate-0"), true)?;
            let base = store
                .context
                .state_root
                .as_path()
                .join(format!("attempts/{}/provider-state", record.attempt_id));
            // An ungranted location is absent evidence, even if it has a file.
            grant(&mut record, temp.path())?;
            assert_eq!(
                store
                    .capture_handoff(&attempt, &record, &decoded())?
                    .handoff,
                Ok(None)
            );
            grant(&mut record, &base.join("candidate-0"))?;
            if link {
                let outside = temp.path().join("outside.json");
                std::fs::write(&outside, snapshot("stopped_early"))?;
                symlink(&outside, base.join("candidate-0/handoff.json"))?;
            } else {
                seal(
                    &candidate,
                    "handoff.json",
                    &vec![b'x'; HANDOFF_BYTES + 1],
                    (HANDOFF_BYTES + 1) as u64,
                )?;
            }
            let result = store.capture_handoff(&attempt, &record, &decoded())?;
            assert!(result.handoff.is_err());
            assert_eq!(store.handoff(&record)?, result.handoff);
        }
        Ok(())
    }

    #[test]
    fn malformed_final_retains_fields_and_never_uses_stale_progress() -> TestResult {
        for complete in [true, false] {
            let temp = tempfile::tempdir()?;
            let clock = clock();
            let cancel = provenant_core::CancelToken::new();
            let call = call(&clock, &cancel);
            let store = RunStore::open(&context(temp.path())?, &call)?;
            let input = input();
            store.create(&input, &call)?;
            let mut record = store.record(input.attempt_id, &call)?;
            let attempt = store.attempt_dir(record.attempt_id, false)?;
            let state = crate::artifacts::subdir(&attempt, OsStr::new("provider-state"), true)?;
            let candidate = crate::artifacts::subdir(&state, OsStr::new("candidate-0"), true)?;
            let base = store
                .context
                .state_root
                .as_path()
                .join(format!("attempts/{}/provider-state", record.attempt_id));
            grant(&mut record, &base.join("candidate-0"))?;
            seal(
                &candidate,
                "handoff.json",
                &snapshot("done"),
                HANDOFF_BYTES as u64,
            )?;
            let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
            let value = serde_json::json!({"schema_version":2,"status":"done","note":format!("New note {secret}"),"remaining":["Unfinished work"],"decisions":[],"verify":[false]});
            let mut result = decoded();
            result.complete = complete;
            result.outcome = Outcome::Succeeded;
            result.result = format!("```handoff\n{value}\n```");
            let captured = store.capture_handoff(&attempt, &record, &result)?;
            let error = captured.handoff.as_ref().unwrap_err();
            assert_eq!(error.dropped[0].field, "verify[0]");
            assert_eq!(
                error.retained["remaining"],
                serde_json::json!(["Unfinished work"])
            );
            assert!(!serde_json::to_string(error)?.contains(secret));
            assert_eq!(store.handoff(&record)?, captured.handoff);
            result.result = Handoff::from_json(&snapshot("done"))?.fenced();
            assert_eq!(store.capture_handoff(&attempt, &record, &result)?, captured);
        }
        Ok(())
    }
}
