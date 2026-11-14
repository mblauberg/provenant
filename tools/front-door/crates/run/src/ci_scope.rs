//! What CI a writer lane runs or waits for (`lanes.ci_scope`). Advisory: the
//! preamble states it and the receipt and status record its value and
//! source; it grants no CI dispatch authority and removes none.
use provenant_records::{LimitSource, run::HarnessProfile};
use serde::{Deserialize, Serialize};

/// The registry knob.
pub const KNOB: &str = "lanes.ci_scope";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiScope {
    /// Push the branch and report; the chair runs CI once for the batch.
    #[default]
    None,
    /// Wait only for the checks that cover the change.
    Targeted,
    /// Wait for the full CI on the lane's pull request.
    Full,
}

impl CiScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Targeted => "targeted",
            Self::Full => "full",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "targeted" => Some(Self::Targeted),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    fn instruction(self) -> &'static str {
        match self {
            Self::None => {
                "do not dispatch, rerun or wait for CI runs; commit, push your branch and report. The chair runs CI once for the integrated batch"
            }
            Self::Targeted => {
                "wait only for the CI checks that cover the code you changed, and do not dispatch or rerun the full suite"
            }
            Self::Full => {
                "wait for the full CI on your pull request and fix what it reports before you report"
            }
        }
    }
}

/// The effective CI scope and the layer that set it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedCiScope {
    pub value: CiScope,
    pub source: LimitSource,
}

impl ResolvedCiScope {
    /// Resolve from the sealed profile. A missing or unknown value is the
    /// registry default, `none`.
    pub fn resolve(profile: &HarnessProfile) -> Self {
        Self {
            value: profile
                .knobs
                .get(KNOB)
                .and_then(serde_json::Value::as_str)
                .and_then(CiScope::parse)
                .unwrap_or_default(),
            source: crate::limits::knob_source(profile, KNOB),
        }
    }

    /// The receipt and status field.
    pub fn receipt(self) -> serde_json::Value {
        serde_json::json!({
            "value": self.value.as_str(),
            "source": self.source.as_str(),
            "key": KNOB,
        })
    }

    /// One preamble line.
    pub fn preamble(self) -> String {
        format!(
            "CI scope: {} (source: {}; {KNOB}): {}.\n",
            self.value.as_str(),
            self.source.as_str(),
            self.value.instruction()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scope_and_its_source_come_from_the_sealed_profile() {
        let mut profile = HarnessProfile::default();
        let unset = ResolvedCiScope::resolve(&profile);
        assert_eq!(
            (unset.value, unset.source),
            (CiScope::None, LimitSource::Default)
        );
        profile.knobs.insert(KNOB.into(), "full".into());
        profile.extra.insert(
            provenant_config::KNOB_SOURCES.into(),
            serde_json::json!({ KNOB: "launch" }),
        );
        let set = ResolvedCiScope::resolve(&profile);
        assert_eq!(
            (set.value, set.source),
            (CiScope::Full, LimitSource::Launch)
        );
        assert_eq!(set.receipt()["value"], "full");
        assert_eq!(set.receipt()["source"], "launch");
        assert!(set.preamble().contains("CI scope: full (source: launch"));
    }
}
