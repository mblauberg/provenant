//! The writer token budget: whether an attempt's reported usage has passed it.
use provenant_records::run::UsageRecord;

/// What the reported usage says about a budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// No budget is set (0).
    Off,
    /// Usage is within the budget; `used` tokens so far.
    Within { used: u64 },
    /// Usage has passed the budget: end the attempt and attempt to commit its
    /// work so a continuation can pick it up.
    Over { used: u64, budget: u64 },
    /// The provider reported no usage. Warn; never refuse.
    Unreported,
}

/// Whether `provider` reports live token usage while running, allowing the
/// guardian to enforce a writer token budget. Codex exec reports usage after
/// each completed turn, so it can be stopped between turns but not mid-turn.
pub fn enforces(provider: &str) -> bool {
    matches!(provider, "claude" | "codex")
}

/// Fresh tokens the provider reported, summed. Cache reads do not count:
/// each call re-reads the whole context, so counting cache reads would
/// prematurely exhaust the budget. `None` when it reported none, which is
/// not the same as zero.
pub fn used(usage: &UsageRecord) -> Option<u64> {
    [
        usage.uncached_input,
        usage.cache_write,
        usage.output_excluding_reasoning,
        usage.reasoning,
    ]
    .into_iter()
    .try_fold(None, |sum: Option<u64>, part| match part {
        Some(n) => Some(Some(sum.unwrap_or(0).saturating_add(n))),
        None => Some(sum),
    })
    .flatten()
}

/// Judge `usage` against `budget` tokens. Cache reads do not count:
/// only fresh tokens count towards the budget, matching the live usage
/// the guardian stops on.
pub fn judge(usage: &UsageRecord, budget: u64) -> Verdict {
    if budget == 0 {
        return Verdict::Off;
    }
    match used(usage) {
        None => Verdict::Unreported,
        Some(used) if used > budget => Verdict::Over { used, budget },
        Some(used) => Verdict::Within { used },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A provider that reports usage in steps, or none at all.
    struct FakeProvider(Vec<Option<u64>>);
    impl FakeProvider {
        fn report(&self, step: usize) -> UsageRecord {
            UsageRecord {
                uncached_input: self.0.get(step).copied().flatten(),
                ..UsageRecord::default()
            }
        }
    }

    #[test]
    fn fresh_tokens_count_towards_the_budget_and_cache_reads_are_ignored() {
        let usage = UsageRecord {
            uncached_input: Some(100),
            cache_read: Some(400),
            cache_write: Some(50),
            output_excluding_reasoning: Some(10),
            ..UsageRecord::default()
        };
        // 100 (uncached) + 50 (cache write) + 10 (output) = 160.
        // Cache read (400) is ignored.
        assert_eq!(used(&usage), Some(160));
        assert_eq!(
            judge(&usage, 150),
            Verdict::Over {
                used: 160,
                budget: 150
            }
        );
        assert_eq!(judge(&usage, 200), Verdict::Within { used: 160 });
    }

    #[test]
    fn a_fake_provider_passing_the_budget_ends_the_attempt_and_silence_only_warns() {
        let provider = FakeProvider(vec![Some(100_000), Some(499_999), Some(500_001)]);
        assert_eq!(
            judge(&provider.report(0), 500_000),
            Verdict::Within { used: 100_000 }
        );
        assert_eq!(
            judge(&provider.report(1), 500_000),
            Verdict::Within { used: 499_999 }
        );
        assert_eq!(
            judge(&provider.report(2), 500_000),
            Verdict::Over {
                used: 500_001,
                budget: 500_000
            }
        );
        assert_eq!(judge(&provider.report(2), 0), Verdict::Off);
        assert_eq!(
            judge(&FakeProvider(vec![None]).report(0), 500_000),
            Verdict::Unreported
        );
    }

    #[test]
    fn budget_capture_applies_the_budget_for_providers_reporting_live_usage() {
        let input = crate::tests::input();
        let task = &input.request.tasks[0];
        assert_eq!(
            crate::launch::capture_plan(task, 60, 500, "codex").token_budget,
            500
        );
        assert_eq!(
            crate::launch::capture_plan(task, 60, 500, "opencode").token_budget,
            0
        );
        assert_eq!(
            crate::launch::capture_plan(task, 60, 500, "claude").token_budget,
            500
        );
    }

    #[test]
    fn budget_enforcement_requires_usage_reported_while_running() {
        assert!(enforces("claude"));
        assert!(enforces("codex"));
        assert!(!enforces("opencode"));
        assert!(!enforces("cursor"));
    }
}
