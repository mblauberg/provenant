use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanOutput {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub fallback_reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultOutput {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub digest: String,
    pub excerpt: String,
    pub truncated: bool,
    pub path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<FindingTrigger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage: Option<FindingTriage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_fix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lens: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub propagates: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design_defect: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_grew: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_grew: Option<bool>,
    pub id: String,
    pub severity: String,
    pub file: String,
    pub line: Option<u64>,
    pub summary: String,
    pub status: String,
    /// The settlement decision on this finding, from the node's declared list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<String>,
    /// One line of why that disposition was chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The review severity vocabulary shared by acceptance, repair and export.
pub const SEVERITY_VOCABULARY: &[(&str, Severity)] = &[
    ("critical", Severity::Blocking),
    ("blocker", Severity::Blocking),
    ("blocking", Severity::Blocking),
    ("major", Severity::High),
    ("high", Severity::High),
    ("medium", Severity::Medium),
    ("moderate", Severity::Medium),
    ("minor", Severity::Low),
    ("low", Severity::Low),
    ("nit", Severity::Nit),
    ("info", Severity::Nit),
    ("style", Severity::Nit),
];

/// Decode the acceptance vocabulary; unrecognised words are high and warned.
pub fn decode_severity(word: &str) -> (Severity, bool) {
    let word = word.trim().to_ascii_lowercase();
    match SEVERITY_VOCABULARY
        .iter()
        .find(|(known, _)| *known == word.as_str())
    {
        Some((_, severity)) => (*severity, false),
        None => (Severity::High, true),
    }
}

/// Relative order for bounded export; unknown words have high priority.
pub fn severity_rank(word: &str) -> u8 {
    match decode_severity(word).0 {
        Severity::Blocking => 0,
        Severity::High => 1,
        Severity::Medium => 2,
        Severity::Low => 3,
        Severity::Nit => 4,
    }
}

/// Evidence checked by verification or synthesis before a repair writer starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingTrigger {
    pub sequence: String,
    pub reach: FindingReach,
    pub observed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingReach {
    DefaultPath,
    OperatorMistake,
    AttackerOrUnreachable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingTriage {
    pub decision: String,
    pub reason: String,
    pub worth_fixing: bool,
    pub root_cause: String,
    pub blocking_class: String,
    #[serde(default)]
    pub shared_flaw: bool,
}

impl Finding {
    pub fn trigger(&self) -> Option<FindingTrigger> {
        self.trigger.clone()
    }

    pub fn triage(&self) -> Option<FindingTriage> {
        self.triage.clone()
    }

    /// The named invariant this finding breaks. `class` is the single field
    /// the review contract uses for the invariant name; it is what shared-flaw
    /// routing and settlement checks key on.
    pub fn named_invariant(&self) -> Option<&str> {
        self.class
            .as_deref()
            .filter(|value| !value.trim().is_empty())
    }

    /// A declared blocking class remains binding even without a trigger.
    pub fn has_blocking_class(&self) -> bool {
        self.boundary_or_data_loss()
            || self.triage.as_ref().is_some_and(|triage| {
                matches!(
                    triage.blocking_class.as_str(),
                    "acceptance" | "default_path_defect"
                )
            })
    }

    pub fn boundary_or_data_loss(&self) -> bool {
        self.triage.as_ref().is_some_and(|triage| {
            matches!(triage.blocking_class.as_str(), "boundary" | "data_loss")
        }) || self
            .class
            .as_deref()
            .is_some_and(|class| matches!(class, "boundary" | "data_loss"))
    }

    pub fn default_path(&self) -> bool {
        self.trigger.as_ref().is_some_and(|trigger| {
            trigger.reach == FindingReach::DefaultPath && !trigger.sequence.trim().is_empty()
        })
    }

    pub fn effective_severity(&self) -> &str {
        &self.severity
    }

    pub fn falsified_with_evidence(&self) -> bool {
        self.verification.as_deref() == Some("falsified")
            && self
                .evidence
                .as_deref()
                .is_some_and(|evidence| !evidence.trim().is_empty())
    }

    pub fn rejected_boundary(&self) -> bool {
        self.boundary_or_data_loss()
            && (self.disposition.as_deref() == Some("rejected")
                || self
                    .triage
                    .as_ref()
                    .is_some_and(|triage| triage.decision == "rejected"))
    }

    pub fn spec_defect(&self) -> bool {
        self.kind.as_deref() == Some("spec_defect")
    }
}
