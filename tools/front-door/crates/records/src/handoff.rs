//! One bounded handoff for final answers, progress and continuation briefs.
use serde::{Deserialize, Serialize};

pub const HANDOFF_BYTES: usize = 12 * 1024;
pub const NOTE_BYTES: usize = 4096;
const ITEMS: usize = 64;
const OPEN: &str = "```handoff";

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
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WriteScopeReport {
    Full {
        #[serde(deserialize_with = "scope_report_schema")]
        schema_version: u64,
        code: String,
        base: String,
        head: String,
        strength: crate::policy::Strength,
        source: crate::policy::Source,
        out_of_scope_paths: Vec<crate::PathWire>,
    },
    Compact {
        code: String,
        strength: crate::policy::Strength,
        source: crate::policy::Source,
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
                source: *source,
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

    /// Only one complete fence at the end of the answer is a declaration.
    /// A malformed declaration is explicit evidence, never an absent handoff.
    pub fn from_result(result: &str) -> HandoffResult {
        let mut capture = HandoffCapture::default();
        capture.push(result.as_bytes());
        capture.finish()
    }

    /// The declaration a provider's completed turns settle on. A fence counts
    /// only at the end of the turn it was written in, so a handoff quoted or
    /// drafted mid-turn is never final. A later turn that neither declares
    /// nor runs a tool, such as an acknowledgement of a late notification,
    /// leaves the earlier declaration standing; a later declaration, a later
    /// malformed one or later tool work replaces it. Without usable turn
    /// boundaries this is exactly [`Handoff::from_result`].
    pub fn from_turns(result: &str, turns: &[TurnEnd]) -> HandoffResult {
        let mut start = 0;
        let mut segments = Vec::with_capacity(turns.len());
        for turn in turns {
            if turn.at < start || !result.is_char_boundary(turn.at) {
                return Self::from_result(result);
            }
            segments.push((&result[start..turn.at], turn.tool_activity));
            start = turn.at;
        }
        // Text no turn ended, such as an appended diagnostic or an interrupted
        // answer, keeps the whole-answer rule.
        if segments.len() < 2 || !result[start..].trim().is_empty() {
            return Self::from_result(result);
        }
        for (text, tool_activity) in segments.into_iter().rev() {
            match Self::from_result(text)? {
                Some(handoff) => return Ok(Some(handoff)),
                None if tool_activity => return Ok(None),
                None => {}
            }
        }
        Ok(None)
    }

    pub fn unfinished(&self) -> bool {
        self.status != HandoffStatus::Done
    }

    pub fn fenced(&self) -> String {
        format!("```handoff\n{}\n```", serde_json::json!(self))
    }
}

/// Where one completed provider turn ends in the decoded result text, and
/// whether that turn ran a tool.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TurnEnd {
    /// Byte offset in the result text.
    pub at: usize,
    pub tool_activity: bool,
}

/// Parse the complete stream, independently of clipped result windows. Lines
/// and declaration JSON are bounded; oversized fence syntax fails closed.
#[derive(Default)]
pub struct HandoffCapture {
    line: Vec<u8>,
    overflow: bool,
    fence: Option<(u8, usize)>,
    json: Option<Vec<u8>>,
    declaration: Option<Handoff>,
    error: Option<HandoffError>,
}

impl HandoffCapture {
    pub fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' {
                self.end_line();
            } else if self.line.len() <= HANDOFF_BYTES {
                self.line.push(byte);
            } else {
                self.overflow = true;
            }
        }
    }

    fn end_line(&mut self) {
        let line = std::mem::take(&mut self.line);
        let overflow = std::mem::take(&mut self.overflow);
        if self.error.is_some() {
            return;
        }
        if overflow {
            // This prefix may end in the middle of a UTF-8 character. Only
            // ASCII fence syntax matters when discarding a long prose line.
            let indent = line.iter().take_while(|byte| **byte == b' ').count();
            let fence_prefix = indent <= 3
                && line
                    .get(indent..indent + 3)
                    .is_some_and(|prefix| prefix == b"```" || prefix == b"~~~");
            if self.json.is_some() || self.declaration.is_some() || fence_prefix {
                self.error = Some(HandoffError::new("$", "handoff line exceeds 12288 bytes"));
            }
            return;
        }
        let Ok(text) = std::str::from_utf8(&line) else {
            self.error = Some(HandoffError::new("$", "invalid handoff UTF-8"));
            return;
        };
        let marker = text.trim_end_matches('\r');
        if self.declaration.is_some() {
            if !marker.trim().is_empty() {
                self.error = Some(HandoffError::new("$", "handoff must end the answer"));
            }
        } else if let Some(json) = &mut self.json {
            if marker == OPEN {
                self.error = Some(HandoffError::new("$", "handoff block repeated"));
            } else if marker == "```" {
                // The line ending preceding the closer is fence syntax, not
                // part of the bounded JSON payload.
                let payload = json
                    .strip_suffix(b"\r\n")
                    .or_else(|| json.strip_suffix(b"\n"))
                    .unwrap_or(json);
                match Handoff::from_json(payload) {
                    Ok(handoff) => self.declaration = Some(handoff),
                    Err(error) => self.error = Some(error),
                }
                self.json = None;
            } else if json.len().saturating_add(line.len() + 1) > HANDOFF_BYTES + 2 {
                self.error = Some(HandoffError::new("$", "handoff exceeds 12288 bytes"));
            } else {
                json.extend_from_slice(&line);
                json.push(b'\n');
            }
        } else if let Some((delimiter, length)) = self.fence {
            if let Some((found, count, suffix)) = fence(marker)
                && found == delimiter
                && count >= length
                && suffix.bytes().all(|byte| matches!(byte, b' ' | b'\t'))
            {
                self.fence = None;
            }
        } else if marker == OPEN {
            self.json = Some(Vec::new());
        } else if let Some((delimiter, length, info)) = fence(marker)
            && (delimiter != b'`' || !info.contains('`'))
        {
            self.fence = Some((delimiter, length));
        }
    }

    pub fn finish(mut self) -> HandoffResult {
        self.end_line();
        if let Some(mut error) = self.error {
            if error.retained.is_empty()
                && let Some(handoff) = self.declaration
                && let Ok(fields) = serde_json::from_value(serde_json::json!(handoff))
            {
                error.retained = fields;
            }
            Err(error)
        } else if self.json.is_some() {
            Err(HandoffError::new("$", "handoff block incomplete"))
        } else {
            Ok(self.declaration)
        }
    }
}

fn fence(line: &str) -> Option<(u8, usize, &str)> {
    let text = line.trim_start_matches(' ');
    if line.len() - text.len() > 3 {
        return None;
    }
    let delimiter = *text.as_bytes().first()?;
    if !matches!(delimiter, b'`' | b'~') {
        return None;
    }
    let length = text.bytes().take_while(|byte| *byte == delimiter).count();
    (length >= 3).then_some((delimiter, length, &text[length..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    const STOP: &str = "```handoff\n{\"schema_version\":2,\"status\":\"stopped_early\",\"note\":\"Parser done.\",\"remaining\":[\"Renderer\"],\"decisions\":[],\"verify\":[\"Parser tests pass\"]}\n```";

    #[test]
    fn handoff_fence_context_survives_chunks_and_long_examples() {
        let handoff = serde_json::json!({"schema_version":2,"status":"stopped_early","note":"Continue","remaining":[],"decisions":[],"verify":[]});
        let block = format!("```handoff\n{handoff}\n```");
        for fence in ["````", "~~~"] {
            let example = format!("{fence}markdown\n{}\n{block}\n{fence}", "x".repeat(28_000));
            for (text, expected) in [
                (example.clone(), false),
                (format!("{example}\n{block}"), true),
            ] {
                for size in [1, 7, 8192] {
                    let mut capture = HandoffCapture::default();
                    for chunk in text.as_bytes().chunks(size) {
                        capture.push(chunk);
                    }
                    let parsed = capture.finish().unwrap();
                    assert_eq!(parsed.is_some(), expected);
                    if let Some(parsed) = parsed {
                        assert_eq!(parsed.note, "Continue");
                    }
                }
            }
        }
        // A previous declaration outside the tail still makes the final one ambiguous.
        let mut capture = HandoffCapture::default();
        capture.push(format!("{block}\n{}\n{block}", "x".repeat(28_000)).as_bytes());
        assert!(capture.finish().is_err());
    }

    /// Join turns as the decoder does, recording where each one ended.
    fn turns(parts: &[(&str, bool)]) -> (String, Vec<TurnEnd>) {
        let mut result = String::new();
        let mut ends = Vec::new();
        for (text, tool_activity) in parts {
            if !result.is_empty() && !result.ends_with('\n') {
                result.push('\n');
            }
            result.push_str(text);
            ends.push(TurnEnd {
                at: result.len(),
                tool_activity: *tool_activity,
            });
        }
        (result, ends)
    }

    #[test]
    fn a_turn_ending_handoff_survives_a_later_acknowledgement() {
        let stop = Handoff::from_result(STOP).unwrap();
        let done = STOP.replace("stopped_early", "done");
        let report = format!("Report.\n{STOP}");
        // Acceptance 1: a later turn with prose only, even several, keeps it.
        let (result, ends) = turns(&[
            (&report, true),
            ("The wait was redundant.", false),
            ("Noted.", false),
        ]);
        assert!(Handoff::from_result(&result).is_err());
        assert_eq!(Handoff::from_turns(&result, &ends), Ok(stop.clone()));
        // Acceptance 4: a later valid declaration wins.
        let (result, ends) = turns(&[(&report, true), ("Ack.", false), (&done, false)]);
        assert_eq!(
            Handoff::from_turns(&result, &ends).unwrap().unwrap().status,
            HandoffStatus::Done
        );
        // Acceptance 3: a later malformed declaration still fails.
        let (result, ends) = turns(&[(&report, false), ("```handoff\n{}\n```", false)]);
        assert!(Handoff::from_turns(&result, &ends).is_err());
        // Later tool work without a new declaration invalidates the old one.
        let (result, ends) = turns(&[(&report, false), ("Pushed a fix.", true)]);
        assert_eq!(Handoff::from_turns(&result, &ends), Ok(None));
        // Without turn boundaries, or with text no turn ended, nothing changes.
        assert_eq!(
            Handoff::from_turns(&result, &[]),
            Handoff::from_result(&result)
        );
        let mut tail = ends.clone();
        tail.pop();
        assert_eq!(
            Handoff::from_turns(&result, &tail),
            Handoff::from_result(&result)
        );
        let bad = [TurnEnd {
            at: result.len() + 1,
            tool_activity: false,
        }];
        assert_eq!(
            Handoff::from_turns(&result, &bad),
            Handoff::from_result(&result)
        );
    }

    #[test]
    fn a_handoff_that_did_not_end_its_turn_is_not_final() {
        // Acceptance 2: drafted mid-turn, with work continuing after it.
        let drafted = format!("Draft:\n{STOP}\nNow running the tests.");
        let (result, ends) = turns(&[(&drafted, true), ("Ack.", false)]);
        assert!(Handoff::from_turns(&result, &ends).is_err());
        let quoted = format!("The brief's example:\n````text\n{STOP}\n````");
        let (result, ends) = turns(&[(&quoted, true), ("Ack.", false)]);
        assert_eq!(Handoff::from_turns(&result, &ends), Ok(None));
    }

    #[test]
    fn declared_fields_round_trip_and_malformed_fields_are_retained_as_typed_outcomes() {
        use crate::run::{Outcome, SettlementNodeOutcome, SettlementReason, TerminalEvidence};
        let original = Handoff::from_result(STOP).unwrap().unwrap();
        assert!(original.spec_concerns.is_empty());
        assert!(!original.fenced().contains("spec_concerns"));
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
                source: crate::policy::Source::Host,
                paths_artifact: "policy-write-scope.json".into(),
                out_of_scope_count: 1,
                paths_omitted_from_handoff: true,
            });
            handoff
                .extra
                .insert("future".into(), serde_json::json!({"any": [true, null]}));
            let value = serde_json::to_value(&handoff).unwrap();
            assert_eq!(
                Handoff::from_result(&handoff.fenced()),
                Ok(Some(handoff.clone()))
            );
            for field in Handoff::FIELDS {
                assert!(
                    value.get(field).is_some(),
                    "declared field {field} missing from roundtrip fixture"
                );
                let mut wrong = value.clone();
                wrong[field] = serde_json::json!(false);
                let parsed = Handoff::from_result(&format!("```handoff\n{wrong}\n```"));
                let error = parsed.as_ref().unwrap_err();
                assert_eq!(error.dropped[0].field, *field);
                assert!(!error.retained.contains_key(*field));
                for other in Handoff::FIELDS.iter().filter(|other| *other != field) {
                    assert_eq!(error.retained[*other], value[*other]);
                }
                assert_eq!(error.retained["future"], value["future"]);
                let classified = SettlementReason::classify(
                    Outcome::Succeeded,
                    Some(&TerminalEvidence::ProcessExit),
                    &[],
                    None,
                    parsed.as_ref().map(Option::as_ref),
                )
                .unwrap();
                assert_eq!(classified.reason, SettlementReason::HandoffMalformed);
                assert_eq!(classified.outcome, Outcome::ReviewRequired);
                assert_eq!(classified.node_outcome, SettlementNodeOutcome::Failed);
                let stop = classified.stop.unwrap();
                assert_eq!(stop.cause(), crate::StopCause::HandoffMalformed);
                assert_eq!(stop.handoff_result(), &parsed);
                assert_eq!(
                    serde_json::from_value::<crate::Stop>(serde_json::json!(stop)).unwrap(),
                    stop
                );
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

    #[test]
    fn unambiguous_agent_spellings_decode_through_the_types() {
        let mut value: serde_json::Value =
            serde_json::from_str(STOP.split('\n').nth(1).unwrap()).unwrap();
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
            assert_eq!(Handoff::from_result(&handoff.fenced()), Ok(Some(handoff)));
        }
    }

    #[test]
    fn statuses_and_forward_fields_share_the_same_codec() {
        for status in ["done", "stopped_early", "blocked"] {
            let block = STOP.replace("stopped_early", status);
            let parsed = Handoff::from_result(&format!("Report.\n{block}\n"))
                .unwrap()
                .unwrap();
            assert_eq!(parsed.unfinished(), status != "done");
            assert_eq!(
                Handoff::from_result(&parsed.fenced()).unwrap(),
                Some(parsed)
            );
        }
        let block = STOP.replace("\"note\":", "\"future\":{\"value\":1},\"note\":");
        let parsed = Handoff::from_result(&block).unwrap().unwrap();
        assert_eq!(parsed.extra["future"]["value"], 1);
        assert!(parsed.fenced().contains("future"));
        assert!(
            Handoff::from_result(&STOP.replace('\n', "\r\n"))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn maximum_json_round_trips_after_oversized_unicode_prose() {
        let mut value = serde_json::json!({"schema_version":2,"status":"done","note":"Done","remaining":[],"decisions":[],"verify":[],"future":""});
        let padding = HANDOFF_BYTES - serde_json::to_vec(&value).unwrap().len();
        value["future"] = serde_json::json!("x".repeat(padding));
        let bytes = serde_json::to_vec(&value).unwrap();
        assert_eq!(bytes.len(), HANDOFF_BYTES);
        let handoff = Handoff::from_json(&bytes).unwrap();
        assert_eq!(
            Handoff::from_result(&handoff.fenced()),
            Ok(Some(handoff.clone()))
        );
        assert_eq!(
            Handoff::from_result(&handoff.fenced().replace('\n', "\r\n")),
            Ok(Some(handoff.clone()))
        );
        assert_eq!(
            Handoff::from_result(&format!("{}\n{}", "é".repeat(7000), handoff.fenced())),
            Ok(Some(handoff))
        );
    }

    #[test]
    fn fenced_examples_are_not_final_declarations() {
        for (open, close) in [
            ("````markdown", "````"),
            ("~~~markdown", "~~~"),
            ("`````text", "`````"),
            ("   ~~~~ text", "   ~~~~"),
        ] {
            let example = format!("Example:\n{open}\n{STOP}\n{close}");
            assert_eq!(Handoff::from_result(&example), Ok(None), "{example}");
            assert_eq!(Handoff::from_result(&format!("{open}\n{STOP}")), Ok(None));
            assert_eq!(
                Handoff::from_result(&format!("{example}\nReport.\n{STOP}")),
                Handoff::from_result(STOP),
            );
        }
        let example = format!("~~~~markdown\n~~~\n```\n{STOP}\n~~~~");
        assert_eq!(Handoff::from_result(&example), Ok(None));
        let example = format!("````markdown\n````\u{a0}\n{STOP}");
        assert_eq!(Handoff::from_result(&example), Ok(None));
    }

    #[test]
    fn absent_blocks_remain_absent_and_malformed_or_ambiguous_blocks_are_errors() {
        for result in [
            "Work remains.",
            "Status: incomplete",
            "> ```handoff\n{}\n```",
            "inline ```handoff\n{}\n```",
        ] {
            assert_eq!(Handoff::from_result(result), Ok(None));
        }
        for result in [
            format!("{STOP}\nMore text"),
            format!("{STOP}\n{STOP}"),
            STOP.trim_end_matches("\n```").to_owned(),
            STOP.replace("\"remaining\":[\"Renderer\"],", ""),
            STOP.replace("stopped_early", "incomplete"),
            STOP.replace("schema_version\":2", "schema_version\":1"),
            STOP.replace("Parser done.", " "),
            STOP.replace("Parser done.", &"x".repeat(NOTE_BYTES + 1)),
            STOP.replace(
                "[\"Renderer\"]",
                &serde_json::json!(vec!["x"; ITEMS + 1]).to_string(),
            ),
            STOP.replace("[\"Renderer\"]", "[1]"),
            STOP.replace("[\"Renderer\"]", "[\"\"]"),
            STOP.replace(
                "[\"Renderer\"]",
                &serde_json::json!(vec!["x".repeat(4096); 3]).to_string(),
            ),
        ] {
            assert!(Handoff::from_result(&result).is_err(), "{result}");
        }
    }
}
