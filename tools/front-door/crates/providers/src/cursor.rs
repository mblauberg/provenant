use crate::{ProviderError, ProviderEvent as E, stream::wire};
use serde::Deserialize;
pub struct Cursor;
crate::adapter!(Cursor, "cursor", true);
crate::quota_source!(Cursor, "cursor");
#[derive(Deserialize)]
#[serde(tag = "type")]
enum Event {
    #[serde(rename = "system")]
    System {
        subtype: String,
        session_id: String,
        model: String,
    },
    #[serde(rename = "assistant")]
    Assistant {
        message: Message,
        session_id: String,
    },
    #[serde(rename = "result")]
    Result {
        session_id: String,
        subtype: String,
        is_error: bool,
        result: String,
        usage: Usage,
    },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
struct Message {
    role: String,
    content: Vec<Content>,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Content {
    Text {
        text: String,
    },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
struct Usage {
    #[serde(rename = "inputTokens")]
    input: u64,
    #[serde(rename = "outputTokens")]
    output: u64,
}
fn decode(bytes: &[u8]) -> Result<Vec<E>, ProviderError> {
    Ok(match wire::<Event>(bytes)? {
        // Auto is a selector, not answering-model evidence.
        Event::System {
            subtype,
            session_id,
            model,
        } => {
            drop(model);
            if subtype == "init" {
                vec![E::Session(session_id)]
            } else {
                vec![]
            }
        }
        Event::Assistant {
            message,
            session_id,
        } if message.role == "assistant" => {
            let mut events = vec![E::Session(session_id)];
            for content in message.content {
                if let Content::Text { text } = content {
                    events.push(E::Text(text));
                }
            }
            events
        }
        Event::Result {
            session_id,
            subtype,
            is_error,
            result,
            usage,
        } => {
            let mut events = vec![
                E::Session(session_id),
                E::Usage {
                    input: usage.input,
                    output: usage.output,
                },
            ];
            if is_error || subtype != "success" {
                events.push(E::Error);
            } else {
                events.push(E::Text(result));
                events.push(E::Terminal);
            }
            events
        }
        _ => vec![],
    })
}
