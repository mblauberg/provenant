//! The limits of one lane, resolved once. Enforcement, the lane preamble and
//! `status` all read the record this builds, so they cannot disagree.
use provenant_confinement::run_config::RunConfig;
use provenant_records::{LaneLimits, Limit, LimitSource, LimitStrength, run::TaskRequest};
use std::path::Path;

/// Resolve `task`'s limits for a lane on `provider`. `runtime_cap` is the
/// provider's host cap on the wall clock.
pub fn lane_limits(
    task: &TaskRequest,
    state_root: &Path,
    project: &str,
    provider: &str,
    runtime_cap: Option<u64>,
) -> LaneLimits {
    let writer = task.writer.is_some();
    // A provider that reports no usage while it runs cannot be stopped on
    // it: the budget is then reported to the agent, not enforced.
    let live = crate::token_budget::enforces(provider);
    let nonzero = |limit: Limit| (limit.value > 0).then_some(limit);
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
    let context_handoff = writer
        .then(|| task.profile.knob_u64("lanes.context_handoff_tokens"))
        .flatten()
        .and_then(|tokens| {
            nonzero(Limit::new(
                tokens,
                LimitSource::Template,
                LimitStrength::Default,
            ))
        });
    let timeout = task.provider_timeout_seconds;
    let deadline_seconds = match runtime_cap {
        Some(cap) if cap < timeout => {
            Some(Limit::new(cap, LimitSource::Host, LimitStrength::Ceiling))
        }
        _ => nonzero(Limit::new(
            timeout,
            LimitSource::Launch,
            LimitStrength::Default,
        )),
    };
    LaneLimits {
        token_budget,
        context_handoff,
        deadline_seconds,
        ..LaneLimits::default()
    }
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

    fn writer() -> provenant_records::run::WriterRequest {
        provenant_records::run::WriterRequest {
            extra: Default::default(),
            project: "/p".into(),
            worktree: "/p/lane".into(),
        }
    }

    fn state(config: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), config).unwrap();
        dir
    }

    #[test]
    fn each_limit_reports_the_winning_source() {
        let state = state(
            "[domains.run]\nwriter_token_budget = 700\n[domains.run.projects.p]\nwriter_token_budget = 900\n",
        );
        let mut task = crate::tests::input().request.tasks[0].clone();
        task.writer = Some(writer());
        task.profile
            .knobs
            .insert("lanes.context_handoff_tokens".into(), 200_000.into());
        task.provider_timeout_seconds = 3600;
        let limits = lane_limits(&task, state.path(), "p", "claude", Some(600));
        let source = |limit: &Option<Limit>| limit.as_ref().map(|l| (l.value, l.source));
        assert_eq!(
            source(&limits.token_budget),
            Some((900, LimitSource::Project))
        );
        assert_eq!(
            source(&limits.context_handoff),
            Some((200_000, LimitSource::Template))
        );
        assert_eq!(
            source(&limits.deadline_seconds),
            Some((600, LimitSource::Host))
        );
        let host_only = lane_limits(&task, state.path(), "other", "claude", None);
        assert_eq!(
            source(&host_only.token_budget),
            Some((700, LimitSource::Host))
        );
        assert_eq!(
            source(&host_only.deadline_seconds),
            Some((3600, LimitSource::Launch))
        );
        let bare = lane_limits(
            &task,
            tempfile::tempdir().unwrap().path(),
            "p",
            "claude",
            None,
        );
        assert_eq!(
            source(&bare.token_budget),
            Some((500_000, LimitSource::Default))
        );
    }

    #[test]
    fn the_preamble_and_the_capture_plan_read_the_same_numbers() {
        let state = state("[domains.run]\nwriter_token_budget = 777\n");
        let mut task = crate::tests::input().request.tasks[0].clone();
        task.writer = Some(writer());
        let limits = lane_limits(&task, state.path(), "p", "claude", None);
        let capture = crate::launch::capture_plan(&task, &limits);
        assert_eq!(capture.token_budget, 777);
        assert!(limits.preamble().contains("after 777 fresh tokens"));
        // A provider that reports no live usage is told, not stopped.
        let codexless = lane_limits(&task, state.path(), "p", "opencode", None);
        assert_eq!(
            crate::launch::capture_plan(&task, &codexless).token_budget,
            0
        );
        assert!(!codexless.preamble().contains("fresh tokens"));
    }
}
