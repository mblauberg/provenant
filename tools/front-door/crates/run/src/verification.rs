//! Verification guidance in a writer preamble (`lanes.verification`). Lanes
//! format only; the paragraph names what the chair's integrated check and CI
//! will run. Advisory: the preamble names its source. A brief line overrides
//! the knob.
use provenant_records::{
    Layer,
    command_result::SettingRef,
    provenance::Source,
    run::HarnessProfile,
    warning::{self, VERIFICATION_BRIEF_OVERRIDES, Warning},
};

/// The registry knob.
pub const KNOB: &str = "lanes.verification";

/// The default verification paragraph. A launch that does not override it
/// includes this text unchanged.
pub const CHECK_PARAGRAPH: &str = "Verification: format only. Run `cargo fmt` (or the repository's formatter) on the files you touched; do not compile, seed or admit a build. The chair's integrated check compiles and lints the touched crates and their tests, and CI runs the full suite. Report what you formatted (source: default; lanes.verification).\n";

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

/// What the preamble should say. A brief line wins over the knob and is
/// reported with `VERIFICATION_BRIEF_OVERRIDES`; there is no stop cause.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerificationResolution {
    pub scope: ResolvedVerification,
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
        let scope = match (brief_scope, knob) {
            (Some(brief_value), Some(knob_value)) if brief_value != knob_value => {
                brief_override_warning(brief_value, knob_value, knob_source);
                ResolvedVerification {
                    value: brief_value,
                    source: "brief",
                }
            }
            (Some(brief_value), _) => ResolvedVerification {
                value: brief_value,
                source: "brief",
            },
            (None, Some(value)) => ResolvedVerification {
                value,
                source: display_source(knob_source),
            },
            (None, None) => ResolvedVerification {
                value: VerificationScope::Targeted,
                source: "default",
            },
        };
        Self { scope }
    }

    /// The verification paragraph. Default `targeted` is [`CHECK_PARAGRAPH`].
    pub fn paragraph(&self) -> String {
        let scope = &self.scope;
        if scope.value == VerificationScope::Targeted && scope.source == "default" {
            return CHECK_PARAGRAPH.to_string();
        }
        let body = match scope.value {
            VerificationScope::Check => {
                "Verification: format only. Run the repository's formatter on the files you touched; do not compile, seed or admit a build. The chair's integrated check compiles and lints the touched crates, and CI runs the full suite. Then commit and push."
            }
            VerificationScope::Targeted => {
                "Verification: format only. Run the repository's formatter on the files you touched; do not compile, seed or admit a build. The chair's integrated check compiles and lints the touched crates and their tests, and CI runs the full suite. Then commit and push."
            }
            VerificationScope::Full => {
                "Verification: format only. Run the repository's formatter on the files you touched; do not compile, seed or admit a build. The chair's integrated check compiles and lints the whole workspace, and CI runs the full suite. Then commit and push."
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

fn display_source(source: Layer) -> &'static str {
    match source {
        Layer::Launch => "brief",
        other => other.as_str(),
    }
}

/// The brief wins: report the override as a typed warning that names both
/// scopes and the losing setting, with a next step to keep the knob in charge.
fn brief_override_warning(brief: VerificationScope, knob: VerificationScope, layer: Layer) {
    warning::warn(
        Warning::new(
            VERIFICATION_BRIEF_OVERRIDES,
            format!(
                "brief verification scope {} overrides {KNOB}={} from {}",
                brief.as_str(),
                knob.as_str(),
                layer.as_str()
            ),
            Source {
                layer: Layer::Launch,
                origin: Some("brief#Verification scope".to_owned()),
            },
        )
        .subject(KNOB)
        .setting(SettingRef {
            key: KNOB.to_owned(),
            value: knob.as_str().to_owned(),
            layer,
            origin: format!("{}#{KNOB}", layer.as_str()),
        })
        .repair(format!(
            "remove the brief line to use {KNOB}={} from {}",
            knob.as_str(),
            layer.as_str()
        )),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_brief_scope_overrides_and_names_its_source() {
        let mut profile = HarnessProfile::default();
        profile.knobs.insert(KNOB.into(), "check".into());
        profile.knob_sources.insert(KNOB.into(), Layer::Host);
        let (resolved, warnings) = warning::capture(|| {
            VerificationResolution::resolve(
                &profile,
                "Do the work.\nVerification scope: targeted\n",
            )
        });
        assert_eq!(resolved.scope.value, VerificationScope::Targeted);
        assert_eq!(resolved.scope.source, "brief");
        // The brief wins; the disagreement is a typed warning that names both
        // scopes and the losing setting, not a stop cause on the resolution.
        let warning = warnings.first().expect("override warning");
        assert_eq!(warning.code.as_ref(), VERIFICATION_BRIEF_OVERRIDES.as_str());
        assert!(warning.message.contains("check"), "{}", warning.message);
        assert!(warning.message.contains("targeted"), "{}", warning.message);
        let setting = warning.setting.as_ref().expect("losing setting");
        assert_eq!(setting.key, KNOB);
        assert_eq!(setting.value, "check");
        assert_eq!(setting.layer, Layer::Host);
        assert!(!warning.next.is_empty());
        let text = resolved.paragraph();
        assert!(text.contains("source: brief"));
        assert!(text.contains("touched crates and their tests"));
        assert!(text.contains("format only"));
        assert!(!text.contains("cargo check"));
        assert!(!text.contains("cargo build"));
        assert!(!text.contains("seeded warm"));
        assert!(!text.contains("at most one cheap compile check"));
    }

    #[test]
    fn the_default_paragraph_names_its_source() {
        let resolved = VerificationResolution::resolve(&HarnessProfile::default(), "implement it");
        assert_eq!(resolved.paragraph(), CHECK_PARAGRAPH);
        assert!(resolved.paragraph().contains("source: default"));
        assert!(resolved.paragraph().contains("format only"));
        assert!(!resolved.paragraph().contains("cargo check"));
    }

    // Spec: docs/specs/run.md#build-strategy
    #[test]
    fn every_scope_tells_the_lane_to_format_and_never_to_build() {
        for (scope, source) in [
            (VerificationScope::Check, "check"),
            (VerificationScope::Targeted, "targeted"),
            (VerificationScope::Full, "full"),
        ] {
            let mut profile = HarnessProfile::default();
            profile.knobs.insert(KNOB.into(), source.into());
            profile.knob_sources.insert(KNOB.into(), Layer::Host);
            let text = VerificationResolution::resolve(&profile, "implement it").paragraph();
            assert!(text.contains("format only"), "{scope:?}: {text}");
            assert!(text.contains("source: host"), "{scope:?}: {text}");
            assert!(!text.contains("cargo check"), "{scope:?}: {text}");
            assert!(!text.contains("cargo build"), "{scope:?}: {text}");
            assert!(!text.contains("CARGO_TARGET_DIR"), "{scope:?}: {text}");
            assert!(!text.contains("seeded warm"), "{scope:?}: {text}");
        }
    }

    #[test]
    fn a_brief_override_warns_and_names_the_guidance_and_the_brief() {
        let mut profile = HarnessProfile::default();
        profile.knobs.insert(KNOB.into(), "check".into());
        profile.knob_sources.insert(KNOB.into(), Layer::Launch);
        let (resolved, warnings) = warning::capture(|| {
            VerificationResolution::resolve(&profile, "Verification scope: full\n")
        });
        let warning = warnings.first().expect("override warning");
        assert_eq!(warning.code.as_ref(), VERIFICATION_BRIEF_OVERRIDES.as_str());
        assert!(warning.message.contains("check"), "{}", warning.message);
        assert!(warning.message.contains("full"), "{}", warning.message);
        assert_eq!(resolved.scope.source, "brief");
        assert_eq!(resolved.scope.value, VerificationScope::Full);
    }

    #[test]
    fn an_agreeing_or_absent_brief_emits_no_override_warning() {
        let mut profile = HarnessProfile::default();
        profile.knobs.insert(KNOB.into(), "full".into());
        profile.knob_sources.insert(KNOB.into(), Layer::Project);
        let (_, warnings) = warning::capture(|| {
            VerificationResolution::resolve(&profile, "Verification scope: full\n")
        });
        assert!(warnings.is_empty(), "{warnings:?}");
        let (_, warnings) =
            warning::capture(|| VerificationResolution::resolve(&profile, "implement it"));
        assert!(warnings.is_empty(), "{warnings:?}");
    }
}
