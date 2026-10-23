use crate::{ProviderError, ProviderEvent as E, stream::wire};
use serde::Deserialize;
pub struct OpenCode;
crate::adapter!(OpenCode, "opencode", true);
// The recordings do not establish v2. Decode their evidence, never activate v2
// or the exit-after-output exception from an unversioned stream.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    Text {
        #[serde(rename = "sessionID")]
        session: String,
        part: Text,
    },
    StepFinish {
        #[serde(rename = "sessionID")]
        session: String,
        part: Finish,
    },
    Error {
        error: ErrorWire,
    },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
struct Text {
    text: String,
}
#[derive(Deserialize)]
struct Finish {
    reason: String,
}
#[derive(Deserialize)]
struct ErrorWire {
    message: String,
}
fn decode(bytes: &[u8]) -> Result<Vec<E>, ProviderError> {
    Ok(match wire::<Event>(bytes)? {
        Event::Text { session, part } => vec![E::Session(session), E::Text(part.text)],
        Event::StepFinish { session, part } => {
            let mut events = vec![E::Session(session)];
            if part.reason == "stop" {
                events.push(E::Terminal);
            }
            events
        }
        Event::Error { error } => {
            drop(error.message);
            vec![E::Error]
        }
        _ => vec![],
    })
}
