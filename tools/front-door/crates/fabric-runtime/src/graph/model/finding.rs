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

/// The three severity classes named by the review doctrine. External reviewers
/// use other words; the vocabulary below is the single translation to these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeverityClass {
    Blocking,
    ShouldFix,
    Note,
}

/// One vocabulary map: each severity word an external reviewer may use, its
/// doctrine class and its relative rank for bounded export. No module keeps a
/// separate severity list.
pub const SEVERITY_VOCABULARY: &[(&str, SeverityClass, u8)] = &[
    ("critical", SeverityClass::Blocking, 0),
    ("blocker", SeverityClass::Blocking, 0),
    ("blocking", SeverityClass::Blocking, 0),
    ("p0", SeverityClass::Blocking, 0),
    ("high", SeverityClass::Blocking, 1),
    ("error", SeverityClass::Blocking, 1),
    ("p1", SeverityClass::Blocking, 1),
    ("medium", SeverityClass::ShouldFix, 2),
    ("moderate", SeverityClass::ShouldFix, 2),
    ("should-fix", SeverityClass::ShouldFix, 2),
    ("p2", SeverityClass::ShouldFix, 2),
    ("low", SeverityClass::Note, 3),
    ("warning", SeverityClass::Note, 3),
    ("p3", SeverityClass::Note, 3),
    ("info", SeverityClass::Note, 4),
    ("informational", SeverityClass::Note, 4),
    ("note", SeverityClass::Note, 4),
    ("p4", SeverityClass::Note, 4),
];

fn severity_word(word: &str) -> Option<(&'static str, SeverityClass, u8)> {
    let word = word.trim().to_ascii_lowercase();
    SEVERITY_VOCABULARY
        .iter()
        .copied()
        .find(|(known, _, _)| *known == word.as_str())
}

/// Translate a reviewer's severity word once. An unknown word is not blocking.
pub fn severity_class(word: &str) -> Option<SeverityClass> {
    severity_word(word).map(|(_, class, _)| class)
}

/// Relative order for bounded export; unknown words sort last.
pub fn severity_rank(word: &str) -> u8 {
    severity_word(word).map_or(5, |(_, _, rank)| rank)
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
        if self.boundary_or_data_loss() || self.default_path() {
            &self.severity
        } else {
            "note"
        }
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
