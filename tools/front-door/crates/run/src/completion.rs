//! Partial-finish evidence in a lane's final answer. Only a structured block
//! the lane wrote counts, never a reading of its prose: the same `handoff`
//! block a graph writer uses to stop honestly.
use serde::Deserialize;

const OPEN: &str = "```handoff\n";
const CLOSE: &str = "\n```";
/// The longest note kept, as in the graph's handoff contract.
const NOTE_BYTES: usize = 4096;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Handoff {
    schema_version: u64,
    status: String,
    note: String,
}

/// The note of a valid `stopped_early` handoff that ends the final answer.
///
/// The block must be the last thing in the answer and appear once: a quoted
/// example earlier in the text, a repeated block or anything after the closing
/// fence leaves the evidence ambiguous, and ambiguity is no flag.
pub(crate) fn declared_stop(result: &str) -> Option<String> {
    let (_, rest) = result.split_once(OPEN)?;
    if rest.contains(OPEN) {
        return None;
    }
    let (json, after) = rest.split_once(CLOSE)?;
    if !after.trim().is_empty() || json.len() > 12 * 1024 {
        return None;
    }
    let handoff: Handoff = serde_json::from_str(json).ok()?;
    (handoff.schema_version == 1
        && handoff.status == "stopped_early"
        && !handoff.note.trim().is_empty()
        && handoff.note.len() <= NOTE_BYTES)
        .then(|| handoff.note.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const STOP: &str = "```handoff\n{\"schema_version\":1,\"status\":\"stopped_early\",\"note\":\"First half committed; finish the rest.\"}\n```";

    #[test]
    fn a_trailing_stopped_early_block_is_the_lanes_own_statement() {
        let answer = format!("Done: parser.\nNot done: renderer.\n\n{STOP}\n");
        assert_eq!(
            declared_stop(&answer).as_deref(),
            Some("First half committed; finish the rest.")
        );
    }

    #[test]
    fn prose_quotes_repeats_and_malformed_blocks_are_no_flag() {
        assert_eq!(declared_stop("Work remains: the renderer is not done."), None);
        assert_eq!(declared_stop(&format!("{STOP}\nThen I carried on.")), None);
        assert_eq!(declared_stop(&format!("{STOP}\n{STOP}")), None);
        assert_eq!(declared_stop(&STOP.replace("stopped_early", "done")), None);
        assert_eq!(declared_stop(&STOP.replace("\"note\":", "\"extra\":1,\"note\":")), None);
        assert_eq!(declared_stop(&STOP.replace("First half committed; finish the rest.", " ")), None);
        assert_eq!(declared_stop(STOP.trim_end_matches("\n```")), None);
        let long = STOP.replace("First half committed; finish the rest.", &"x".repeat(NOTE_BYTES + 1));
        assert_eq!(declared_stop(&long), None);
    }
}
