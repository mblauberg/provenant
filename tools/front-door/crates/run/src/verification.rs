//! Local verification a writer is told to run (`lanes.verification`).
//! Advisory: the preamble states it. The default `check` wording is the
//! historical paragraph, with no source, so an unset launch stays byte-identical
//! on that paragraph. A brief line overrides the knob.
use provenant_records::{LimitSource, run::HarnessProfile};

/// The registry knob.
pub const KNOB: &str = "lanes.verification";

/// The default verification paragraph. A launch that does not override it
/// includes this text unchanged.
pub const CHECK_PARAGRAPH: &str = "Verification: where the repository has CI, CI runs the tests and lints on pull requests and the integrated branch, not on every push; the CI scope line below says what CI, if any, you wait for. Run at most one cheap compile check (`cargo check` on the one package you changed), then commit and push; do not run test suites, clippy or nextest locally, and do not restart a build that was stopped. The repository's test tiers (T0 to T3) describe what CI and a configured host run; they are not a checklist for you. A `cargo` command the host refuses because CI runs it is final: do not look for another way to run it.\n";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VerificationScope {
    #[default]
    Check,
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
                    value: VerificationScope::Check,
                    source: "default",
                },
                conflict: None,
            },
        }
    }

    /// The verification paragraph. Default `check` is [`CHECK_PARAGRAPH`].
    pub fn paragraph(&self) -> String {
        let scope = &self.scope;
        if scope.value == VerificationScope::Check && scope.source == "default" {
            return CHECK_PARAGRAPH.to_string();
        }
        let body = match scope.value {
            VerificationScope::Check => {
                "Verification: run at most one cheap compile check (`cargo check` on the one package you changed), then commit and push. Do not run test suites, clippy or nextest locally."
            }
            VerificationScope::Targeted => {
                "Verification: run the tests that cover the code you changed, then commit and push. Do not run the full local suite unless this scope says so."
            }
            VerificationScope::Full => {
                "Verification: run the local checks the repository documents for this change, including its tests, then commit and push."
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
        assert!(text.contains("tests that cover"));
        assert!(!text.contains("at most one cheap compile check"));
    }

    #[test]
    fn the_default_paragraph_is_the_historical_wording() {
        let resolved = VerificationResolution::resolve(&HarnessProfile::default(), "implement it");
        assert_eq!(resolved.paragraph(), CHECK_PARAGRAPH);
        assert!(!resolved.paragraph().contains("source:"));
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
