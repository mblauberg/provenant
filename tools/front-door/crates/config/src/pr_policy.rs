//! The lane pull-request policy (`lanes.pr`). A sourced knob: every effective
//! value names the layer that set it, and the provider's `gh` tool enforces it
//! before forwarding a command. The deliver template can select `never`.
use provenant_records::{Layer, run::HarnessProfile};
use serde::{Deserialize, Serialize};

use crate::knob_source;

/// The registry knob.
pub const KNOB: &str = "lanes.pr";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrPolicy {
    /// Refuse every pull-request write from the lane.
    Never,
    /// Permit creation only as a draft: the `gh` wrapper adds `--draft` when
    /// it is missing. Drafts cannot be promoted, merged or reopened by the lane.
    Draft,
    /// Forward pull-request writes unchanged.
    #[default]
    Allowed,
}

impl PrPolicy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Draft => "draft",
            Self::Allowed => "allowed",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "never" => Some(Self::Never),
            "draft" => Some(Self::Draft),
            "allowed" => Some(Self::Allowed),
            _ => None,
        }
    }

    /// The instruction carried in the writer preamble. It names the behaviour
    /// the lane is allowed to perform, not just the knob value.
    pub const fn instruction(self) -> &'static str {
        match self {
            Self::Never => {
                "do not open, edit or close pull requests and do not write to the forge API; the chair publishes after settlement"
            }
            Self::Draft => {
                "open pull requests only as drafts; the gh wrapper adds --draft, permits edit, close, comment and review, and refuses other PR writes and forge writes"
            }
            Self::Allowed => {
                "you may open, edit and close pull requests with gh; use a filled pull request template and `gh pr create --body-file FILE`"
            }
        }
    }
}

/// The effective policy and the layer that set it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedPrPolicy {
    pub value: PrPolicy,
    pub source: Layer,
}

impl ResolvedPrPolicy {
    /// Resolve from the sealed profile. Missing or unknown values fall back to
    /// the registry default, `allowed`.
    pub fn resolve(profile: &HarnessProfile) -> Self {
        Self {
            value: profile
                .knobs
                .get(KNOB)
                .and_then(serde_json::Value::as_str)
                .and_then(PrPolicy::parse)
                .unwrap_or_default(),
            source: knob_source(profile, KNOB),
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

    /// The preamble line, naming the value and its source.
    pub fn preamble(self) -> String {
        format!(
            "Pull request policy: {} (source: {}; {KNOB}). {}.\n",
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
    fn the_policy_defaults_to_allowed_and_reports_the_layer_that_set_it() {
        let mut profile = HarnessProfile::default();
        let unset = ResolvedPrPolicy::resolve(&profile);
        assert_eq!(
            (unset.value, unset.source),
            (PrPolicy::Allowed, Layer::Default)
        );
        assert_eq!(unset.receipt()["value"], "allowed");
        assert_eq!(unset.receipt()["source"], "default");

        profile.knobs.insert(KNOB.into(), "draft".into());
        profile.knob_sources.insert(KNOB.into(), Layer::Launch);
        let set = ResolvedPrPolicy::resolve(&profile);
        assert_eq!((set.value, set.source), (PrPolicy::Draft, Layer::Launch));
        assert_eq!(set.receipt()["value"], "draft");
        assert_eq!(set.receipt()["source"], "launch");

        profile.knobs.insert(KNOB.into(), "unknown".into());
        let unknown = ResolvedPrPolicy::resolve(&profile);
        assert_eq!(unknown.value, PrPolicy::Allowed);

        profile.knobs.insert(KNOB.into(), "never".into());
        assert_eq!(ResolvedPrPolicy::resolve(&profile).value, PrPolicy::Never);
    }
}
