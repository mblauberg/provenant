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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    pub schema_version: u64,
    pub status: HandoffStatus,
    pub note: String,
    pub remaining: Vec<String>,
    pub decisions: Vec<String>,
    pub verify: Vec<String>,
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
}

impl Handoff {
    pub fn from_json(json: &[u8]) -> Result<Self, &'static str> {
        if json.len() > HANDOFF_BYTES {
            return Err("handoff exceeds 12288 bytes");
        }
        let handoff: Self = serde_json::from_slice(json).map_err(|_| "invalid handoff JSON")?;
        let text_valid = |s: &str| !s.trim().is_empty() && s.len() <= NOTE_BYTES;
        if handoff.schema_version != 2
            || !text_valid(&handoff.note)
            || [&handoff.remaining, &handoff.decisions, &handoff.verify]
                .iter()
                .any(|items| items.len() > ITEMS || items.iter().any(|s| !text_valid(s)))
        {
            return Err(
                "handoff requires schema_version 2, a note of 1..4096 bytes and arrays of at most 64 non-empty strings of 1..4096 bytes",
            );
        }
        Ok(handoff)
    }

    /// Only one complete fence at the end of the answer is a declaration.
    /// Missing and malformed declarations never imply unfinished work.
    pub fn from_result(result: &str) -> Result<Option<Self>, &'static str> {
        let mut capture = HandoffCapture::default();
        capture.push(result.as_bytes());
        capture.finish()
    }

    pub fn unfinished(&self) -> bool {
        self.status != HandoffStatus::Done
    }

    pub fn fenced(&self) -> String {
        format!("```handoff\n{}\n```", serde_json::json!(self))
    }
}

/// Parse the complete stream, independently of clipped result windows. Lines
/// and declaration JSON are bounded; oversized fence syntax fails closed.
#[derive(Default)]
pub(crate) struct HandoffCapture {
    line: Vec<u8>,
    overflow: bool,
    fence: Option<(u8, usize)>,
    json: Option<Vec<u8>>,
    declaration: Option<Handoff>,
    error: Option<&'static str>,
}

impl HandoffCapture {
    pub(crate) fn push(&mut self, bytes: &[u8]) {
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

    pub(crate) fn finish(mut self) -> Result<Option<Handoff>, &'static str> {
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
