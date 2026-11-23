//! The one sourced family for CI observation retry and backoff values.
//!
//! Every reader of check state — the landing gate, `wait --pr`, `ci failures`
//! and review subject selection — pauses on the same values. They live here as
//! one family so a source names itself once and no caller keeps a private copy
//! of a backoff constant.
//!
//! Spec: docs/specs/cli.md#ci-observations

use provenant_records::provenance::{Layer, Precedence, Source, Sourced};
use std::time::Duration;

/// The layers that may set an observation retry value, lowest first. A launch
/// override wins; the compiled default is the floor.
///
/// Spec: docs/specs/state.md#every-setting-reports-its-layer
pub(crate) const RETRY_PRECEDENCE: Precedence = Precedence(&[
    Layer::Default,
    Layer::Profile,
    Layer::Host,
    Layer::Project,
    Layer::Launch,
]);

/// The interval a waiter uses when it names none of its own. It is one value
/// of this family, not a private copy in a caller.
pub(crate) const DEFAULT_POLL: Duration = Duration::from_secs(5);

fn sourced<T>(value: T, layer: Layer, origin: Option<&str>) -> Sourced<T> {
    Sourced {
        value,
        source: Source {
            layer,
            origin: origin.map(str::to_owned),
        },
    }
}

/// Retry and backoff values shared by every CI reader. Each value carries the
/// layer and place that set it, so a report can name the source of a pause.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Retry {
    /// Interval a waiter uses when it names none of its own.
    pub poll: Sourced<Duration>,
    /// First pause after a transient observation failure.
    pub first: Sourced<Duration>,
    /// Ceiling for the exponential backoff.
    pub last: Sourced<Duration>,
    /// Longest a pause runs before cancellation is checked again.
    pub slice: Sourced<Duration>,
    /// Bounded rereads of an UNKNOWN mergeability.
    pub mergeable_attempts: Sourced<u8>,
    /// Pause between those rereads.
    pub mergeable_pause: Sourced<Duration>,
    /// Bounded rereads when `--head` disagrees with the forge just after a push.
    pub head_attempts: Sourced<usize>,
    /// Pause between those head rereads.
    pub head_delay: Sourced<Duration>,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            poll: sourced(DEFAULT_POLL, Layer::Default, Some("ci.retry.poll")),
            first: sourced(
                Duration::from_secs(1),
                Layer::Default,
                Some("ci.retry.first"),
            ),
            last: sourced(
                Duration::from_secs(15),
                Layer::Default,
                Some("ci.retry.last"),
            ),
            slice: sourced(
                Duration::from_millis(50),
                Layer::Default,
                Some("ci.retry.slice"),
            ),
            mergeable_attempts: sourced(3, Layer::Default, Some("ci.retry.mergeable_attempts")),
            mergeable_pause: sourced(
                Duration::from_secs(1),
                Layer::Default,
                Some("ci.retry.mergeable_pause"),
            ),
            head_attempts: sourced(4, Layer::Default, Some("ci.retry.head_attempts")),
            head_delay: sourced(
                Duration::from_millis(250),
                Layer::Default,
                Some("ci.retry.head_delay"),
            ),
        }
    }
}

impl Retry {
    /// Merge one value's candidates through the family's precedence. The
    /// compiled default is supplied first, so there is always a winner.
    fn choose<T: Clone>(
        name: &str,
        default: T,
        candidates: impl IntoIterator<Item = Sourced<T>>,
    ) -> Sourced<T> {
        let floor = sourced(default.clone(), Layer::Default, Some(name));
        let mut all = vec![floor.clone()];
        all.extend(candidates);
        match RETRY_PRECEDENCE.merge(all).winner {
            Some(winner) => winner,
            None => floor,
        }
    }

    /// Build the family from optional overrides, each already tagged with its
    /// source. Readers that have no overrides get the compiled defaults; a
    /// test or a host layer can raise any value while keeping its source.
    #[allow(
        clippy::too_many_arguments,
        reason = "one family of independent values"
    )]
    pub(crate) fn resolve(
        poll: Option<Sourced<Duration>>,
        first: Option<Sourced<Duration>>,
        last: Option<Sourced<Duration>>,
        slice: Option<Sourced<Duration>>,
        mergeable_attempts: Option<Sourced<u8>>,
        mergeable_pause: Option<Sourced<Duration>>,
        head_attempts: Option<Sourced<usize>>,
        head_delay: Option<Sourced<Duration>>,
    ) -> Self {
        let defaults = Self::default();
        Self {
            poll: Self::choose("ci.retry.poll", defaults.poll.value, poll),
            first: Self::choose("ci.retry.first", defaults.first.value, first),
            last: Self::choose("ci.retry.last", defaults.last.value, last),
            slice: Self::choose("ci.retry.slice", defaults.slice.value, slice),
            mergeable_attempts: Self::choose(
                "ci.retry.mergeable_attempts",
                defaults.mergeable_attempts.value,
                mergeable_attempts,
            ),
            mergeable_pause: Self::choose(
                "ci.retry.mergeable_pause",
                defaults.mergeable_pause.value,
                mergeable_pause,
            ),
            head_attempts: Self::choose(
                "ci.retry.head_attempts",
                defaults.head_attempts.value,
                head_attempts,
            ),
            head_delay: Self::choose("ci.retry.head_delay", defaults.head_delay.value, head_delay),
        }
    }

    /// Merge the `ci.retry.*` knobs into the family, keeping each knob's layer
    /// and origin. Duration knobs are integer milliseconds; attempt knobs are
    /// counts. An absent or unreadable knob keeps the compiled default.
    /// Spec: docs/specs/cli.md#ci-observations
    pub(crate) fn from_knobs(
        knobs: &std::collections::BTreeMap<String, Sourced<serde_json::Value>>,
    ) -> Self {
        fn duration(
            knobs: &std::collections::BTreeMap<String, Sourced<serde_json::Value>>,
            name: &str,
        ) -> Option<Sourced<Duration>> {
            let knob = knobs.get(name)?;
            let millis = knob.value.as_u64()?;
            Some(Sourced {
                value: Duration::from_millis(millis),
                source: knob.source.clone(),
            })
        }

        fn count<T: TryFrom<u64>>(
            knobs: &std::collections::BTreeMap<String, Sourced<serde_json::Value>>,
            name: &str,
        ) -> Option<Sourced<T>> {
            let knob = knobs.get(name)?;
            let value = T::try_from(knob.value.as_u64()?).ok()?;
            Some(Sourced {
                value,
                source: knob.source.clone(),
            })
        }

        Self::resolve(
            duration(knobs, "ci.retry.poll"),
            duration(knobs, "ci.retry.first"),
            duration(knobs, "ci.retry.last"),
            duration(knobs, "ci.retry.slice"),
            count(knobs, "ci.retry.mergeable_attempts"),
            duration(knobs, "ci.retry.mergeable_pause"),
            count(knobs, "ci.retry.head_attempts"),
            duration(knobs, "ci.retry.head_delay"),
        )
    }

    /// `LAYER (origin)` for one value, for a warning that names why a pause was
    /// chosen.
    #[allow(dead_code, reason = "available to every waiter that reports a pause")]
    pub(crate) fn describe<T>(value: &Sourced<T>) -> String {
        match &value.source.origin {
            Some(origin) => format!("{:?} ({origin})", value.source.layer),
            None => format!("{:?}", value.source.layer),
        }
    }
}

#[cfg(test)]
#[allow(dead_code)] // The worktree driver includes the library without a test harness.
mod tests {
    use super::*;

    fn at(layer: Layer, value: Duration) -> Sourced<Duration> {
        sourced(value, layer, Some("test"))
    }

    /// Spec: docs/specs/state.md#every-setting-reports-its-layer
    #[test]
    fn a_launch_override_wins_and_keeps_its_source() {
        let retry = Retry::resolve(
            None,
            Some(at(Layer::Launch, Duration::from_millis(7))),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(retry.first.value, Duration::from_millis(7));
        assert_eq!(retry.first.source.layer, Layer::Launch);
        assert_eq!(retry.first.source.origin.as_deref(), Some("test"));
        assert_eq!(Retry::describe(&retry.first), "Launch (test)");
        assert_eq!(retry.last.source.layer, Layer::Default);
    }

    /// Spec: docs/specs/cli.md#ci-observations
    #[test]
    fn the_defaults_are_the_one_shared_backoff() {
        let retry = Retry::default();
        assert_eq!(retry.first.value, Duration::from_secs(1));
        assert_eq!(retry.last.value, Duration::from_secs(15));
        assert_eq!(retry.slice.value, Duration::from_millis(50));
        assert_eq!(retry.head_attempts.value, 4);
        assert_eq!(retry.head_delay.value, Duration::from_millis(250));
    }

    /// Spec: docs/specs/cli.md#ci-observations
    #[test]
    fn knobs_override_the_shared_family_and_keep_their_source() {
        let mut knobs = std::collections::BTreeMap::new();
        knobs.insert(
            "ci.retry.mergeable_attempts".to_owned(),
            sourced(
                serde_json::json!(7),
                Layer::Host,
                Some("config.toml#ci.retry.mergeable_attempts"),
            ),
        );
        knobs.insert(
            "ci.retry.head_delay".to_owned(),
            sourced(
                serde_json::json!(9),
                Layer::Project,
                Some("config.toml#ci.retry.head_delay"),
            ),
        );
        let retry = Retry::from_knobs(&knobs);
        assert_eq!(retry.mergeable_attempts.value, 7);
        assert_eq!(retry.mergeable_attempts.source.layer, Layer::Host);
        assert_eq!(retry.head_delay.value, Duration::from_millis(9));
        assert_eq!(retry.head_delay.source.layer, Layer::Project);
        assert_eq!(retry.poll.value, DEFAULT_POLL);
        assert_eq!(retry.poll.source.layer, Layer::Default);
    }
}
