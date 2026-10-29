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
        let mut offset = 0;
        let mut start = None;
        for line in result.split_inclusive('\n') {
            if line.trim_end_matches(['\r', '\n']) == OPEN {
                start = Some(offset + line.len());
                break;
            }
            offset += line.len();
        }
        let Some(start) = start else { return Ok(None) };
        let rest = &result[start..];
        let mut json_end = None;
        let mut offset = 0;
        for line in rest.split_inclusive('\n') {
            let marker = line.trim_end_matches(['\r', '\n']);
            if marker == OPEN {
                return Err("handoff block repeated");
            }
            if marker == "```" && json_end.is_none() {
                json_end = Some(offset);
                if !rest[offset + line.len()..].trim().is_empty() {
                    return Err("handoff must end the answer");
                }
            }
            offset += line.len();
        }
        let end = json_end.ok_or("handoff block incomplete")?;
        Self::from_json(rest[..end].trim_end().as_bytes()).map(Some)
    }

    pub fn unfinished(&self) -> bool {
        self.status != HandoffStatus::Done
    }

    pub fn fenced(&self) -> String {
        format!("```handoff\n{}\n```", serde_json::json!(self))
    }
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
