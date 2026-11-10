//! The limits of one lane, resolved once. Enforcement, the lane preamble and
//! `status` all read the record this builds, so they cannot disagree.
use provenant_confinement::run_config::RunConfig;
use provenant_records::{LaneLimits, Limit, LimitSource, LimitStrength, run::TaskRequest};
use std::path::Path;

/// The task setting that carries `retry --extend` raises to the next launch.
pub const EXTENSIONS: &str = "limit_extensions";

/// Host settings that bound one lane.
#[derive(Clone, Copy, Debug, Default)]
pub struct HostCaps {
    /// The provider's cap on the wall clock.
    pub runtime_seconds: Option<u64>,
    /// Model policy's automatic continuations after a stop.
    pub continuations: Option<u8>,
}

/// Resolve `task`'s limits for a lane on `provider`, once, at preparation.
/// Everything after reads this record.
pub fn lane_limits(
    task: &TaskRequest,
    state_root: &Path,
    project: &str,
    provider: &str,
    caps: HostCaps,
) -> LaneLimits {
    let writer = task.writer.is_some();
    // A provider that reports no usage while it runs cannot be stopped on
    // it: the budget is then reported to the agent, not enforced.
    let live = crate::token_budget::enforces(provider);
    let nonzero = |limit: Limit| (limit.value > 0).then_some(limit);
    let knob = |name: &str, source| {
        task.profile
            .knob_u64(name)
            .and_then(|value| nonzero(Limit::new(value, source, LimitStrength::Default)))
    };
    let token_budget = writer
        .then(|| {
            RunConfig::read(state_root)
                .unwrap_or_default()
                .writer_token_budget_for(project)
        })
        .and_then(nonzero)
        .map(|mut limit| {
            if !live {
                limit.strength = LimitStrength::Advisory;
            }
            limit
        });
    let timeout = task.provider_timeout_seconds;
    let deadline_seconds = match caps.runtime_seconds {
        Some(cap) if cap < timeout => {
            Some(Limit::new(cap, LimitSource::Host, LimitStrength::Ceiling))
        }
        _ => nonzero(Limit::new(
            timeout,
            LimitSource::Launch,
            LimitStrength::Default,
        )),
    };
    let mut limits = LaneLimits {
        token_budget,
        context_handoff: writer
            .then(|| knob("lanes.context_handoff_tokens", LimitSource::Template))
            .flatten(),
        deadline_seconds,
        continuations: writer
            .then(|| {
                caps.continuations.and_then(|n| {
                    nonzero(Limit::new(
                        n.into(),
                        LimitSource::Host,
                        LimitStrength::Default,
                    ))
                })
            })
            .flatten(),
        repair_cycles: knob("lanes.max_repair_cycles", LimitSource::Template),
    };
    // Raises the owner asked for with `retry --extend`; the command refused a
    // ceiling already, and one that arrives here anyway stays as it is.
    if let Some(raises) = task.extra.get(EXTENSIONS).and_then(|v| v.as_object()) {
        for (name, value) in raises {
            if let Some(value) = value.as_u64() {
                let _ = limits.extend(name, value);
            }
        }
    }
    limits
}

/// Limits with only a deadline, for tests that need an allowance.
#[cfg(test)]
pub(crate) fn deadline_only(seconds: u64) -> LaneLimits {
    LaneLimits {
        deadline_seconds: (seconds > 0)
            .then(|| Limit::new(seconds, LimitSource::Launch, LimitStrength::Default)),
        ..LaneLimits::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer_task() -> TaskRequest {
        let mut task = crate::tests::input().request.tasks[0].clone();
        task.writer = Some(provenant_records::run::WriterRequest {
            extra: Default::default(),
            project: "/p".into(),
            worktree: "/p/lane".into(),
        });
        task
    }

    fn state(config: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            format!("schema_version = 1\n{config}"),
        )
        .unwrap();
        dir
    }

    fn found(limit: &Option<Limit>) -> Option<(u64, LimitSource)> {
        limit.as_ref().map(|l| (l.value, l.source))
    }

    /// Acceptance 1: each limit reports the source that won.
    #[test]
    fn each_limit_reports_the_winning_source() {
        let state = state(
            "[domains.run]\nwriter_token_budget = 700\n[domains.run.projects.p]\nwriter_token_budget = 900\n",
        );
        let mut task = writer_task();
        task.profile
            .knobs
            .insert("lanes.context_handoff_tokens".into(), 200_000.into());
        task.provider_timeout_seconds = 3600;
        let caps = HostCaps {
            runtime_seconds: Some(600),
            continuations: Some(2),
        };
        let limits = lane_limits(&task, state.path(), "p", "claude", caps);
        assert_eq!(
            found(&limits.token_budget),
            Some((900, LimitSource::Project))
        );
        assert_eq!(
            found(&limits.context_handoff),
            Some((200_000, LimitSource::Template))
        );
        assert_eq!(
            found(&limits.deadline_seconds),
            Some((600, LimitSource::Host))
        );
        assert_eq!(found(&limits.continuations), Some((2, LimitSource::Host)));
        let host_only = lane_limits(&task, state.path(), "other", "claude", HostCaps::default());
        assert_eq!(
            found(&host_only.token_budget),
            Some((700, LimitSource::Host))
        );
        assert_eq!(
            found(&host_only.deadline_seconds),
            Some((3600, LimitSource::Launch))
        );
        let bare = tempfile::tempdir().unwrap();
        let bare = lane_limits(&task, bare.path(), "p", "claude", HostCaps::default());
        assert_eq!(
            found(&bare.token_budget),
            Some((500_000, LimitSource::Default))
        );
    }

    /// Acceptance 2: the preamble and the capture plan read one record.
    #[test]
    fn the_preamble_and_the_capture_plan_read_the_same_numbers() {
        let state = state("[domains.run]\nwriter_token_budget = 777\n");
        let task = writer_task();
        let limits = lane_limits(&task, state.path(), "p", "claude", HostCaps::default());
        assert_eq!(
            crate::launch::capture_plan(&task, &limits).token_budget,
            777
        );
        assert!(limits.preamble().contains("after 777 fresh tokens"));
        // A provider that reports no live usage is told, not stopped.
        let blind = lane_limits(&task, state.path(), "p", "opencode", HostCaps::default());
        assert_eq!(crate::launch::capture_plan(&task, &blind).token_budget, 0);
        assert!(!blind.preamble().contains("fresh tokens"));
    }

    /// Acceptance 3: `retry --extend` raises a default and not a ceiling.
    #[test]
    fn an_extension_raises_a_default_and_never_a_ceiling() {
        let state = state("[domains.run]\nwriter_token_budget = 700\n");
        let mut task = writer_task();
        task.provider_timeout_seconds = 3600;
        task.extra.insert(
            EXTENSIONS.into(),
            serde_json::json!({"token_budget": 5000, "deadline_seconds": 9000}),
        );
        let caps = HostCaps {
            runtime_seconds: Some(600),
            ..HostCaps::default()
        };
        let limits = lane_limits(&task, state.path(), "p", "claude", caps);
        assert_eq!(
            found(&limits.token_budget),
            Some((5000, LimitSource::Launch))
        );
        assert_eq!(
            found(&limits.deadline_seconds),
            Some((600, LimitSource::Host))
        );
        assert!(limits.preamble().contains("after 5000 fresh tokens"));
    }
}
