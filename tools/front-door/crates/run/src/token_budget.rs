//! The writer token budget: whether an attempt's reported usage has passed it.
use provenant_records::run::UsageRecord;

/// What the reported usage says about a budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// No budget is set (0).
    Off,
    /// Usage is within the budget; `used` tokens so far.
    Within { used: u64 },
    /// Usage has passed the budget: end the attempt with its work committed
    /// and pushed so a continuation can pick it up.
    Over { used: u64, budget: u64 },
    /// The provider reported no usage. Warn; never refuse.
    Unreported,
}

/// Every token class the provider reported, summed, cached input included.
/// `None` when it reported none, which is not the same as zero.
pub fn used(usage: &UsageRecord) -> Option<u64> {
    [
        usage.uncached_input,
        usage.cache_read,
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

/// Judge `usage` against `budget` tokens. Cached input counts, as the live
/// figure the guardian stops on does.
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
    fn cached_input_counts_towards_the_budget() {
        let usage = UsageRecord {
            uncached_input: Some(100),
            cache_read: Some(400),
            cache_write: Some(50),
            output_excluding_reasoning: Some(10),
            ..UsageRecord::default()
        };
        assert_eq!(used(&usage), Some(560));
        assert_eq!(
            judge(&usage, 500),
            Verdict::Over {
                used: 560,
                budget: 500
            }
        );
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
}
