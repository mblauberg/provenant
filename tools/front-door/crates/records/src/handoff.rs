//! One bounded handoff for final answers, progress and continuation briefs.
use serde::{Deserialize, Serialize};

pub const HANDOFF_BYTES: usize = 12 * 1024;
pub const NOTE_BYTES: usize = 4096;
const ITEMS: usize = 64;

/// Absent, valid, or malformed. Never discard the error to classify absence.
pub type HandoffResult = Result<Option<Handoff>, HandoffError>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffFieldError {
    pub field: String,
    pub error: String,
}

/// Invalid fields are dropped whole; independently decoded fields survive.
/// The remainder is evidence, never a valid completion declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffError {
    pub dropped: Vec<HandoffFieldError>,
    pub retained: crate::ExtraFields,
}

impl HandoffError {
    pub fn new(field: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            dropped: vec![HandoffFieldError {
                field: field.into(),
                error: error.into(),
            }],
            retained: Default::default(),
        }
    }
}

impl std::fmt::Display for HandoffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, issue) in self.dropped.iter().enumerate() {
            if i > 0 {
                write!(f, "; ")?;
            }
            write!(f, "handoff field {}: {}", issue.field, issue.error)?;
        }
        Ok(())
    }
}
impl std::error::Error for HandoffError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffStatus {
    Done,
    StoppedEarly,
    Blocked,
}

impl HandoffStatus {
    /// The wire spelling, for human rendering.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::StoppedEarly => "stopped_early",
            Self::Blocked => "blocked",
        }
    }
}

impl<'de> Deserialize<'de> for HandoffStatus {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        match text.trim().to_ascii_lowercase().as_str() {
            "done" => Ok(Self::Done),
            "stopped_early" => Ok(Self::StoppedEarly),
            "blocked" => Ok(Self::Blocked),
            _ => Err(serde::de::Error::custom(
                "expected status done, stopped_early or blocked",
            )),
        }
    }
}

/// What blocked a writer that declared `status: blocked`. The handoff names
/// it in the optional `blocked_on` field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockedOn {
    /// A choice only the owner can make.
    OwnerDecision,
    /// A capability, credential, repository or access the brief needs and the
    /// writer lacks.
    Capability,
    /// A tool, lock, build, hook, disk or network failure the writer could not
    /// clear.
    Environment,
    /// Waiting on another lane, CI or a merge.
    Dependency,
}

impl BlockedOn {
    pub const ALL: [BlockedOn; 4] = [
        Self::OwnerDecision,
        Self::Capability,
        Self::Environment,
        Self::Dependency,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OwnerDecision => "owner_decision",
            Self::Capability => "capability",
            Self::Environment => "environment",
            Self::Dependency => "dependency",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        let text = text.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|kind| kind.as_str() == text)
    }
}

/// The handoff's `blocked_on` as written. The field is additive, so a text
/// value that names no known kind is kept as written and warned about, never
/// refused; only a value that is not text is malformed, as for every field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockedOnField {
    Kind(BlockedOn),
    Unrecognised(String),
}

impl Serialize for BlockedOnField {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Kind(kind) => serializer.serialize_str(kind.as_str()),
            Self::Unrecognised(text) => serializer.serialize_str(text),
        }
    }
}

impl<'de> Deserialize<'de> for BlockedOnField {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Ok(BlockedOn::parse(&text).map_or(Self::Unrecognised(text), Self::Kind))
    }
}

/// The one statement of what each handoff status means, for every prompt that
/// teaches it: the lane preamble and the routing policy's guidance both use it.
pub const STATUS_GUIDANCE: &str = "Use status done only when the brief is finished. Use stopped_early only when you cannot carry on in this session; a fresh session then continues on this branch from your handoff, so commit and push your work first and list what remains in remaining[]. Never use stopped_early to report work you could do now. Use blocked only when you cannot proceed, and set blocked_on to say what blocked you: owner_decision (a choice only the owner can make), capability (a capability, credential, repository or access the brief needs and you lack), environment (a tool, lock, build, hook, disk or network failure you could not clear; retry or work around it first and note what you tried) or dependency (waiting on another lane, CI or a merge). Use blocked, not stopped_early, in those cases.";

/// A writer's evidenced challenge to a criterion, for the chair to settle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SpecConcern {
    pub criterion: String,
    pub claim: String,
    pub evidence: String,
    pub proposed_change: String,
    pub status: SpecConcernStatus,
}

impl<'de> Deserialize<'de> for SpecConcern {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Fields {
            #[serde(default)]
            criterion: String,
            claim: String,
            #[serde(default)]
            evidence: String,
            #[serde(default)]
            proposed_change: String,
            #[serde(default)]
            status: SpecConcernStatus,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Input {
            Text(String),
            Object(Fields),
        }
        Ok(match Input::deserialize(d)? {
            Input::Text(claim) => Self {
                criterion: String::new(),
                claim,
                evidence: String::new(),
                proposed_change: String::new(),
                status: SpecConcernStatus::default(),
            },
            Input::Object(v) => Self {
                criterion: v.criterion,
                claim: v.claim,
                evidence: v.evidence,
                proposed_change: v.proposed_change,
                status: v.status,
            },
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecConcernStatus {
    #[default]
    AsWritten,
    Diverged,
    Stopped,
}

/// Typed scope evidence attached by the host after settlement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WriteScopeReport {
    Full {
        #[serde(deserialize_with = "scope_report_schema")]
        schema_version: u64,
        code: String,
        base: String,
        head: String,
        strength: crate::policy::Strength,
        source: crate::provenance::Source,
        out_of_scope_paths: Vec<crate::PathWire>,
    },
    Compact {
        code: String,
        strength: crate::policy::Strength,
        source: crate::provenance::Source,
        paths_artifact: String,
        out_of_scope_count: usize,
        paths_omitted_from_handoff: bool,
    },
}

fn scope_report_schema<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let version = u64::deserialize(deserializer)?;
    crate::RecordHeader::new(version)
        .map(|header| header.schema_version())
        .map_err(serde::de::Error::custom)
}

impl WriteScopeReport {
    pub fn compact(&self) -> Self {
        match self {
            Self::Full {
                code,
                strength,
                source,
                out_of_scope_paths,
                ..
            } => Self::Compact {
                code: code.clone(),
                strength: *strength,
                source: source.clone(),
                paths_artifact: "policy-write-scope.json".into(),
                out_of_scope_count: out_of_scope_paths.len(),
                paths_omitted_from_handoff: true,
            },
            Self::Compact { .. } => self.clone(),
        }
    }

    pub fn out_of_scope_count(&self) -> usize {
        match self {
            Self::Full {
                out_of_scope_paths, ..
            } => out_of_scope_paths.len(),
            Self::Compact {
                out_of_scope_count, ..
            } => *out_of_scope_count,
        }
    }
}

fn strings<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    // A visitor preserves the item path (e.g. verify[0]) on an invalid array.
    struct Strings;
    impl<'de> serde::de::Visitor<'de> for Strings {
        type Value = Vec<String>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a string or array of strings")
        }
        fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Self::Value, E> {
            Ok(vec![text.to_owned()])
        }
        fn visit_string<E: serde::de::Error>(self, text: String) -> Result<Self::Value, E> {
            Ok(vec![text])
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut items = Vec::new();
            while let Some(item) = seq.next_element::<String>()? {
                items.push(item);
            }
            Ok(items)
        }
    }
    d.deserialize_any(Strings)
}

fn text_valid(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= NOTE_BYTES
}
fn strings_valid(items: &[String]) -> bool {
    items.len() <= ITEMS && items.iter().all(|s| text_valid(s))
}
fn concerns_valid(items: &[SpecConcern]) -> bool {
    items.len() <= ITEMS
        && items.iter().all(|c| {
            text_valid(&c.claim)
                && [&c.criterion, &c.evidence, &c.proposed_change]
                    .iter()
                    .all(|s| s.len() <= NOTE_BYTES)
        })
}

// The fields, decoding rules, validation and test field list have one owner.
macro_rules! handoff_fields {
    ($( $(#[$attr:meta])* $name:ident: $ty:ty => $valid:expr; )*) => {
        #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub struct Handoff {
            $($(#[$attr])* pub $name: $ty,)*
            #[serde(flatten)] pub extra: crate::ExtraFields,
        }
        impl Handoff {
            #[cfg(test)]
            const FIELDS: &'static [&'static str] = &[$(stringify!($name)),*];

            pub fn validate(&self) -> Result<(), &'static str> {
                $(if !($valid)(&self.$name) { return Err(concat!("invalid handoff field ", stringify!($name))); })*
                Ok(())
            }

            fn decode(value: serde_json::Value) -> Result<Self, HandoffError> {
                let Some(object) = value.as_object() else { return Err(HandoffError::new("$", "expected a handoff object")); };
                let mut retained = crate::ExtraFields::default();
                let mut dropped = Vec::new();
                $({
                    // Decode each field independently with its own serde attributes.
                    #[derive(Serialize, Deserialize)]
                    struct Field { $(#[$attr])* $name: $ty }
                    let mut input = serde_json::Map::new();
                    if let Some(value) = object.get(stringify!($name)) { input.insert(stringify!($name).into(), value.clone()); }
                    let input = serde_json::Value::Object(input);
                    match serde_path_to_error::deserialize::<_, Field>(input) {
                        Ok(field) if ($valid)(&field.$name) => { retained.insert(stringify!($name).into(), serde_json::json!(field.$name)); }
                        Ok(_) => dropped.push(HandoffFieldError { field: stringify!($name).into(), error: "value exceeds bounds or has no required text".into() }),
                        Err(error) => {
                            let path = error.path().to_string();
                            dropped.push(HandoffFieldError { field: if path.is_empty() || path == "." { stringify!($name).into() } else { path }, error: error.inner().to_string() });
                        }
                    }
                })*
                for (key, value) in object {
                    if !matches!(key.as_str(), $(stringify!($name))|*) { retained.insert(key.clone(), value.clone()); }
                }
                if !dropped.is_empty() { return Err(HandoffError { dropped, retained }); }
                serde_json::from_value(serde_json::json!(retained)).map_err(|error| HandoffError::new("$", error.to_string()))
            }
        }
    };
}

handoff_fields! {
    schema_version: u64 => |v: &u64| *v == 2;
    status: HandoffStatus => |_: &HandoffStatus| true;
    note: String => |v: &String| text_valid(v);
    #[serde(deserialize_with = "strings")]
    remaining: Vec<String> => |v: &Vec<String>| strings_valid(v);
    #[serde(deserialize_with = "strings")]
    decisions: Vec<String> => |v: &Vec<String>| strings_valid(v);
    #[serde(deserialize_with = "strings")]
    verify: Vec<String> => |v: &Vec<String>| strings_valid(v);
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    spec_concerns: Vec<SpecConcern> => |v: &Vec<SpecConcern>| concerns_valid(v);
    #[serde(default, skip_serializing_if = "Option::is_none")]
    write_scope: Option<WriteScopeReport> => |_: &Option<WriteScopeReport>| true;
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blocked_on: Option<BlockedOnField> => |_: &Option<BlockedOnField>| true;
}

impl Handoff {
    pub fn from_json(json: &[u8]) -> Result<Self, HandoffError> {
        if json.len() > HANDOFF_BYTES {
            return Err(HandoffError::new("$", "handoff exceeds 12288 bytes"));
        }
        let value = serde_json::from_slice(json)
            .map_err(|error| HandoffError::new("$", format!("invalid handoff JSON: {error}")))?;
        Self::decode(value)
    }

    pub fn unfinished(&self) -> bool {
        self.status != HandoffStatus::Done
    }

    /// What blocked the writer: the recognised `blocked_on` of a `blocked`
    /// handoff. `None` when the field is absent, unrecognised, or given with
    /// another status, where it is ignored.
    pub fn blocked_on(&self) -> Option<BlockedOn> {
        match &self.blocked_on {
            Some(BlockedOnField::Kind(kind)) if self.status == HandoffStatus::Blocked => {
                Some(*kind)
            }
            _ => None,
        }
    }

    /// The warning for a `blocked_on` that is ignored: an unrecognised value,
    /// or a value given with a status other than `blocked`. The handoff is
    /// accepted either way and keeps the field.
    pub fn blocked_on_warning(&self) -> Option<String> {
        let field = self.blocked_on.as_ref()?;
        if self.status != HandoffStatus::Blocked {
            return Some("handoff blocked_on is only used with status blocked; ignoring it".into());
        }
        match field {
            BlockedOnField::Kind(_) => None,
            BlockedOnField::Unrecognised(text) => Some(format!(
                "handoff blocked_on \"{text}\" is not one of {}; ignoring it",
                BlockedOn::ALL.map(BlockedOn::as_str).join(", ")
            )),
        }
    }

    pub fn fenced(&self) -> String {
        format!("```handoff\n{}\n```", serde_json::json!(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DECL: &str = r#"{"schema_version":2,"status":"stopped_early","note":"Parser done.","remaining":["Renderer"],"decisions":[],"verify":[]}"#;

    #[test]
    fn declared_fields_round_trip_and_malformed_fields_are_retained_as_typed_outcomes() {
        let original = Handoff::from_json(DECL.as_bytes()).unwrap();
        assert!(original.spec_concerns.is_empty());
        for status in [
            SpecConcernStatus::AsWritten,
            SpecConcernStatus::Diverged,
            SpecConcernStatus::Stopped,
        ] {
            let mut handoff = original.clone();
            handoff.spec_concerns.push(SpecConcern {
                criterion: "Criterion".into(),
                claim: "Claim".into(),
                evidence: "Evidence".into(),
                proposed_change: "Change".into(),
                status,
            });
            handoff.write_scope = Some(WriteScopeReport::Compact {
                code: "write_scope".into(),
                strength: crate::policy::Strength::Warn,
                source: crate::provenance::Source {
                    layer: crate::Layer::Host,
                    origin: Some("compiled policy table".to_owned()),
                },
                paths_artifact: "policy-write-scope.json".into(),
                out_of_scope_count: 1,
                paths_omitted_from_handoff: true,
            });
            handoff.blocked_on = Some(BlockedOnField::Kind(BlockedOn::Environment));
            handoff
                .extra
                .insert("future".into(), serde_json::json!({"any": [true, null]}));
            let value = serde_json::to_value(&handoff).unwrap();
            let bytes = serde_json::to_vec(&value).unwrap();
            assert_eq!(Handoff::from_json(&bytes), Ok(handoff.clone()));
            for field in Handoff::FIELDS {
                assert!(
                    value.get(field).is_some(),
                    "declared field {field} missing from roundtrip fixture"
                );
                let mut wrong = value.clone();
                wrong[field] = serde_json::json!(false);
                let error = Handoff::from_json(&serde_json::to_vec(&wrong).unwrap()).unwrap_err();
                assert_eq!(error.dropped[0].field, *field);
                assert!(!error.retained.contains_key(*field));
                for other in Handoff::FIELDS.iter().filter(|other| *other != field) {
                    assert_eq!(error.retained[*other], value[*other]);
                }
                assert_eq!(error.retained["future"], value["future"]);
            }
        }
        // A malformed array item reports its serde path, not just the parent.
        for field in Handoff::FIELDS {
            let mut value = serde_json::to_value(&original).unwrap();
            if value.get(field).is_some_and(serde_json::Value::is_array)
                || *field == "spec_concerns"
            {
                value[field] = serde_json::json!([false]);
                let error = Handoff::from_json(&serde_json::to_vec(&value).unwrap()).unwrap_err();
                assert_eq!(error.dropped[0].field, format!("{field}[0]"));
            }
        }
    }

    fn handoff_with(status: &str, blocked_on: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 2, "status": status, "note": "n", "remaining": [],
            "decisions": [], "verify": [], "blocked_on": blocked_on
        }))
        .unwrap()
    }

    /// A new field never makes a valid v2 handoff invalid.
    #[test]
    fn blocked_on_round_trips_and_never_refuses_a_handoff() {
        for kind in BlockedOn::ALL {
            let handoff =
                Handoff::from_json(&handoff_with("blocked", serde_json::json!(kind.as_str())))
                    .unwrap();
            assert_eq!(handoff.blocked_on(), Some(kind));
            assert_eq!(handoff.blocked_on_warning(), None);
            assert_eq!(handoff.blocked_on, Some(BlockedOnField::Kind(kind)));
            assert!(handoff.extra.is_empty());
            let bytes = serde_json::to_vec(&handoff).unwrap();
            assert_eq!(Handoff::from_json(&bytes), Ok(handoff));
        }
        // Casing and whitespace are tolerated, as for the status.
        let spaced =
            Handoff::from_json(&handoff_with("blocked", serde_json::json!(" Environment ")))
                .unwrap();
        assert_eq!(spaced.blocked_on(), Some(BlockedOn::Environment));
        // An unknown value is kept as written and warned about, never refused.
        let handoff =
            Handoff::from_json(&handoff_with("blocked", serde_json::json!("weather"))).unwrap();
        assert_eq!(handoff.blocked_on(), None);
        assert_eq!(
            handoff.blocked_on,
            Some(BlockedOnField::Unrecognised("weather".into()))
        );
        assert!(handoff.blocked_on_warning().unwrap().contains("weather"));
        // A value that is not text is malformed, as for every declared field.
        assert!(Handoff::from_json(&handoff_with("blocked", serde_json::json!(7))).is_err());
        // With any other status it is ignored, with a warning.
        for status in ["done", "stopped_early"] {
            let handoff =
                Handoff::from_json(&handoff_with(status, serde_json::json!("environment")))
                    .unwrap();
            assert_eq!(handoff.blocked_on(), None, "{status}");
            assert!(
                handoff
                    .blocked_on_warning()
                    .unwrap()
                    .contains("only used with status blocked"),
                "{status}"
            );
        }
        // Absent is not a warning.
        let absent =
            Handoff::from_json(DECL.replace("stopped_early", "blocked").as_bytes()).unwrap();
        assert_eq!(absent.blocked_on(), None);
        assert_eq!(absent.blocked_on_warning(), None);
    }

    #[test]
    fn unambiguous_agent_spellings_decode_through_the_types() {
        let mut value: serde_json::Value = serde_json::from_str(DECL).unwrap();
        value["spec_concerns"] =
            serde_json::json!(["Question this criterion", {"claim": "Need evidence"}]);
        for field in ["remaining", "decisions", "verify"] {
            value[field] = serde_json::json!("One item");
        }
        for status in ["DONE", "Stopped_Early", " Blocked "] {
            value["status"] = serde_json::json!(status);
            let handoff = Handoff::from_json(&serde_json::to_vec(&value).unwrap()).unwrap();
            assert_eq!(handoff.spec_concerns[0].claim, "Question this criterion");
            assert_eq!(
                handoff.spec_concerns[0].status,
                SpecConcernStatus::AsWritten
            );
            assert!(handoff.spec_concerns[0].criterion.is_empty());
            assert_eq!(handoff.remaining, ["One item"]);
            assert_eq!(handoff.decisions, ["One item"]);
            assert_eq!(handoff.verify, ["One item"]);
            let bytes = serde_json::to_vec(&handoff).unwrap();
            assert_eq!(Handoff::from_json(&bytes), Ok(handoff));
        }
    }

    #[test]
    fn statuses_and_forward_fields_share_the_same_codec() {
        for status in ["done", "stopped_early", "blocked"] {
            let value: serde_json::Value =
                serde_json::from_str(&DECL.replace("stopped_early", status)).unwrap();
            let parsed = Handoff::from_json(&serde_json::to_vec(&value).unwrap()).unwrap();
            assert_eq!(parsed.unfinished(), status != "done");
            let bytes = serde_json::to_vec(&parsed).unwrap();
            assert_eq!(Handoff::from_json(&bytes), Ok(parsed));
        }
        let value: serde_json::Value =
            serde_json::from_str(&DECL.replace("\"note\":", "\"future\":{\"value\":1},\"note\":"))
                .unwrap();
        let parsed = Handoff::from_json(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(parsed.extra["future"]["value"], 1);
        assert!(serde_json::to_string(&parsed).unwrap().contains("future"));
    }

    #[test]
    fn maximum_json_round_trips_without_reading_prose_or_fences() {
        let mut value = serde_json::json!({"schema_version":2,"status":"done","note":"Done","remaining":[],"decisions":[],"verify":[],"future":""});
        let padding = HANDOFF_BYTES - serde_json::to_vec(&value).unwrap().len();
        value["future"] = serde_json::json!("x".repeat(padding));
        let bytes = serde_json::to_vec(&value).unwrap();
        assert_eq!(bytes.len(), HANDOFF_BYTES);
        let handoff = Handoff::from_json(&bytes).unwrap();
        let round = serde_json::to_vec(&handoff).unwrap();
        assert_eq!(Handoff::from_json(&round), Ok(handoff));
        // The declaration channel carries JSON bytes only: prose, fenced blocks
        // and trailing text are never read as a declaration.
        let mut prose = b"Report.\n```handoff\n".to_vec();
        prose.extend_from_slice(&bytes);
        prose.extend_from_slice(b"\n```\nThe wait was redundant.\n");
        assert!(Handoff::from_json(&prose).is_err());
        let mut fenced = format!("```handoff\n{DECL}\n```").into_bytes();
        fenced.extend_from_slice(b"\nVERDICT: PASS\n");
        assert!(Handoff::from_json(&fenced).is_err());
    }
}
