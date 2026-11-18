//! Local verification a writer is told to run (`lanes.verification`).
//! Advisory: the preamble names its source. A brief line overrides the knob.
use provenant_records::{LimitSource, run::HarnessProfile};

/// The registry knob.
pub const KNOB: &str = "lanes.verification";

/// The default verification paragraph. A launch that does not override it
/// includes this text unchanged.
pub const CHECK_PARAGRAPH: &str = "Verification: use a shared warm CARGO_TARGET_DIR; if unavailable, report it and do not build cold. Run cargo check, clippy on touched crates and targeted tests for changed code, then commit and push. Leave the whole workspace suite to CI (source: default; lanes.verification).\n";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VerificationScope {
    Check,
    #[default]
    Targeted,
    Full,
}

impl VerificationScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Targeted => "targeted",
            Self::Full => "full",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "check" => Some(Self::Check),
            "targeted" => Some(Self::Targeted),
            "full" => Some(Self::Full),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedVerification {
    pub value: VerificationScope,
    pub source: &'static str,
}

/// What the preamble should say, and a stop cause when the knob and the brief disagree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerificationResolution {
    pub scope: ResolvedVerification,
    pub conflict: Option<String>,
}

impl VerificationResolution {
    pub fn resolve(profile: &HarnessProfile, brief: &str) -> Self {
        let knob = profile
            .knobs
            .get(KNOB)
            .and_then(|value| value.as_str())
            .and_then(VerificationScope::parse);
        let knob_source = provenant_config::knob_source(profile, KNOB);
        let brief_scope = brief_scope(brief);
        match (brief_scope, knob) {
            (Some(brief_value), Some(knob_value)) if brief_value != knob_value => Self {
                scope: ResolvedVerification {
                    value: brief_value,
                    source: "brief",
                },
                conflict: Some(conflict_cause(knob_value.as_str(), brief_value.as_str())),
            },
            (Some(brief_value), _) => Self {
                scope: ResolvedVerification {
                    value: brief_value,
                    source: "brief",
                },
                conflict: None,
            },
            (None, Some(value)) => Self {
                scope: ResolvedVerification {
                    value,
                    source: display_source(knob_source),
                },
                conflict: None,
            },
            (None, None) => Self {
                scope: ResolvedVerification {
                    value: VerificationScope::Targeted,
                    source: "default",
                },
                conflict: None,
            },
        }
    }

    /// The verification paragraph. Default `targeted` is [`CHECK_PARAGRAPH`].
    pub fn paragraph(&self) -> String {
        let scope = &self.scope;
        if scope.value == VerificationScope::Targeted && scope.source == "default" {
            return CHECK_PARAGRAPH.to_string();
        }
        let body = match scope.value {
            VerificationScope::Check => {
                "Verification: use a shared warm CARGO_TARGET_DIR; if unavailable, report it and do not build cold. Run cargo check on the touched crates, then commit and push."
            }
            VerificationScope::Targeted => {
                "Verification: use a shared warm CARGO_TARGET_DIR; if unavailable, report it and do not build cold. Run cargo check, clippy on touched crates and targeted tests for changed code, then commit and push. Leave the whole workspace suite to CI."
            }
            VerificationScope::Full => {
                "Verification: use a shared warm CARGO_TARGET_DIR; if unavailable, report it and do not build cold. Run the local checks the repository documents for this change, including its tests; leave the whole workspace suite to CI, then commit and push."
            }
        };
        format!("{body} (source: {}; {KNOB}).\n", scope.source)
    }
}

/// A line `Verification scope: check|targeted|full` in the chair brief.
fn brief_scope(brief: &str) -> Option<VerificationScope> {
    brief.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("Verification scope:")?;
        VerificationScope::parse(rest.trim())
    })
}

fn display_source(source: LimitSource) -> &'static str {
    match source {
        LimitSource::Launch => "brief",
        other => other.as_str(),
    }
}

/// Stop cause when guidance and the brief name different scopes. The preamble
/// follows the brief; this names both if the lane stops on the disagreement.
pub fn conflict_cause(guidance: &str, brief: &str) -> String {
    format!("verification scope conflict: lane guidance is {guidance}; the brief declares {brief}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_brief_scope_overrides_and_names_its_source() {
        let mut profile = HarnessProfile::default();
        profile.knobs.insert(KNOB.into(), "check".into());
        profile.knob_sources.insert(KNOB.into(), LimitSource::Host);
        let resolved = VerificationResolution::resolve(
            &profile,
            "Do the work.\nVerification scope: targeted\n",
        );
        assert_eq!(resolved.scope.value, VerificationScope::Targeted);
        assert_eq!(resolved.scope.source, "brief");
        // docs/specs/run.md: a knob that disagrees with the brief line is a
        // conflict; the paragraph follows the brief and the cause names both.
        let cause = resolved.conflict.as_deref().expect("conflict");
        assert!(cause.contains("lane guidance is check"), "{cause}");
        assert!(cause.contains("the brief declares targeted"), "{cause}");
        let text = resolved.paragraph();
        assert!(text.contains("source: brief"));
        assert!(text.contains("targeted tests"));
        assert!(!text.contains("at most one cheap compile check"));
    }

    #[test]
    fn the_default_paragraph_names_its_source() {
        let resolved = VerificationResolution::resolve(&HarnessProfile::default(), "implement it");
        assert_eq!(resolved.paragraph(), CHECK_PARAGRAPH);
        assert!(resolved.paragraph().contains("source: default"));
    }

    #[test]
    fn a_conflict_stop_names_the_guidance_and_the_brief() {
        let mut profile = HarnessProfile::default();
        profile.knobs.insert(KNOB.into(), "check".into());
        profile
            .knob_sources
            .insert(KNOB.into(), LimitSource::Launch);
        let resolved = VerificationResolution::resolve(&profile, "Verification scope: full\n");
        let cause = resolved.conflict.expect("conflict");
        assert!(cause.contains("lane guidance is check"));
        assert!(cause.contains("the brief declares full"));
        assert_eq!(resolved.scope.source, "brief");
        assert_eq!(resolved.scope.value, VerificationScope::Full);
    }
}
