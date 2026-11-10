//! One bounded handoff for final answers, progress and continuation briefs.
use serde::{Deserialize, Serialize};

pub const HANDOFF_BYTES: usize = 12 * 1024;
pub const NOTE_BYTES: usize = 4096;
const ITEMS: usize = 64;
const OPEN: &str = "```handoff";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffStatus {
    Done,
    StoppedEarly,
    Blocked,
}

/// A writer's evidenced challenge to a criterion, for the chair to settle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecConcern {
    pub criterion: String,
    pub claim: String,
    pub evidence: String,
    pub proposed_change: String,
    pub status: SpecConcernStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecConcernStatus {
    AsWritten,
    Diverged,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    pub schema_version: u64,
    pub status: HandoffStatus,
    pub note: String,
    pub remaining: Vec<String>,
    pub decisions: Vec<String>,
    pub verify: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spec_concerns: Vec<SpecConcern>,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

impl Handoff {
    pub fn from_json(json: &[u8]) -> Result<Self, &'static str> {
        if json.len() > HANDOFF_BYTES {
            return Err("handoff exceeds 12288 bytes");
        }
        let handoff: Self = serde_json::from_slice(json).map_err(|_| "invalid handoff JSON")?;
        handoff.validate()?;
        Ok(handoff)
    }

    /// The declaration bounds, for a handoff decoded inside another record.
    pub fn validate(&self) -> Result<(), &'static str> {
        let handoff = self;
        let text_valid = |s: &str| !s.trim().is_empty() && s.len() <= NOTE_BYTES;
        if handoff.schema_version != 2
            || !text_valid(&handoff.note)
            || handoff.spec_concerns.len() > ITEMS
            || handoff.spec_concerns.iter().any(|concern| {
                [
                    &concern.criterion,
                    &concern.claim,
                    &concern.evidence,
                    &concern.proposed_change,
                ]
                .iter()
                .any(|text| !text_valid(text))
            })
            || [&handoff.remaining, &handoff.decisions, &handoff.verify]
                .iter()
                .any(|items| items.len() > ITEMS || items.iter().any(|s| !text_valid(s)))
        {
            return Err(
                "handoff requires schema_version 2, a note of 1..4096 bytes and arrays of at most 64 non-empty strings of 1..4096 bytes",
            );
        }
        Ok(())
    }

    /// Only one complete fence at the end of the answer is a declaration.
    /// Missing and malformed declarations never imply unfinished work.
    pub fn from_result(result: &str) -> Result<Option<Self>, &'static str> {
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
    pub fn from_turns(result: &str, turns: &[TurnEnd]) -> Result<Option<Self>, &'static str> {
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
    error: Option<&'static str>,
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
                self.error = Some("handoff line exceeds 12288 bytes");
            }
            return;
        }
        let Ok(text) = std::str::from_utf8(&line) else {
            self.error = Some("invalid handoff UTF-8");
            return;
        };
        let marker = text.trim_end_matches('\r');
        if self.declaration.is_some() {
            if !marker.trim().is_empty() {
                self.error = Some("handoff must end the answer");
            }
        } else if let Some(json) = &mut self.json {
            if marker == OPEN {
                self.error = Some("handoff block repeated");
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
                self.error = Some("handoff exceeds 12288 bytes");
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

    pub fn finish(mut self) -> Result<Option<Handoff>, &'static str> {
        self.end_line();
        if let Some(error) = self.error {
            Err(error)
        } else if self.json.is_some() {
            Err("handoff block incomplete")
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
    fn spec_concerns_round_trip_and_absence_is_unchanged() {
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
                criterion: "Retain old format".into(),
                claim: "The format is retired".into(),
                evidence: "The approved scope removes the old reader".into(),
                proposed_change: "Accept the current format only".into(),
                status,
            });
            assert_eq!(
                Handoff::from_result(&handoff.fenced()).unwrap(),
                Some(handoff)
            );
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
    fn missing_malformed_and_ambiguous_blocks_are_no_declaration() {
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
