use crate::{ProviderError, ProviderEvent as E, stream::wire};
use serde::Deserialize;
pub struct Codex;
crate::adapter!(Codex, "codex", false);
crate::quota_source!(Codex, "codex");
#[derive(Deserialize)]
#[serde(tag = "type")]
enum Event {
    #[serde(rename = "thread.started")]
    Thread { thread_id: String },
    #[serde(rename = "turn.started")]
    Started,
    #[serde(rename = "item.completed")]
    Item { item: Item },
    #[serde(rename = "turn.completed")]
    Completed { usage: Usage },
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "turn.failed")]
    Failed { error: Failure },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
struct Failure {
    message: String,
}
#[derive(Deserialize)]
#[serde(tag = "type")]
enum Item {
    #[serde(rename = "agent_message")]
    Answer { text: String },
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
        Event::Thread { thread_id } => vec![E::Session(thread_id)],
        Event::Item {
            item: Item::Answer { text },
        } => vec![E::Text(text)],
        Event::Completed { usage } => vec![
            E::Usage {
                input: usage.input_tokens,
                output: usage.output_tokens,
            },
            E::Terminal,
        ],
        Event::Error { message }
        | Event::Failed {
            error: Failure { message },
        } => {
            drop(message);
            vec![E::Error]
        }
        _ => vec![],
    })
}
