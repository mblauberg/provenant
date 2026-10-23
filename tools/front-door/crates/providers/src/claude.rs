use crate::{ProviderError, ProviderEvent as E, stream::wire};
use serde::Deserialize;
pub struct Claude;
crate::adapter!(Claude, "claude", false);
crate::quota_source!(Claude, "claude");
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
        parent_tool_use_id: Option<String>,
        error: Option<String>,
        #[serde(default)]
        is_api_error_message: bool,
    },
    #[serde(rename = "result")]
    Result {
        session_id: String,
        subtype: String,
        is_error: bool,
        result: String,
    },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
struct Message {
    model: String,
    role: String,
    content: Vec<Content>,
    usage: Option<Usage>,
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
    input_tokens: u64,
    output_tokens: u64,
}
fn decode(bytes: &[u8]) -> Result<Vec<E>, ProviderError> {
    Ok(match wire::<Event>(bytes)? {
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
            parent_tool_use_id: None,
            error,
            is_api_error_message,
        } if message.role == "assistant" => {
            let mut events = vec![E::Session(session_id), E::Model(message.model)];
            if let Some(error) = error {
                events.push(if error == "authentication_failed" {
                    E::Refusal
                } else {
                    E::Error
                });
            } else if is_api_error_message {
                events.push(E::Error);
            }
            for content in message.content {
                if let Content::Text { text } = content {
                    events.push(E::Text(text));
                }
            }
            if let Some(usage) = message.usage {
                events.push(E::Usage {
                    input: usage.input_tokens,
                    output: usage.output_tokens,
                });
            }
            events
        }
        Event::Result {
            session_id,
            subtype,
            is_error,
            result,
        } => {
            let mut events = vec![E::Session(session_id)];
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
