use crate::{ProviderError, ProviderEvent as E, stream::wire};
use serde::Deserialize;
pub struct Agy;
crate::adapter!(Agy, "agy", crate::launch::unrecorded);
const MODEL_ON_WIRE: bool = true;
crate::quota_source!(Agy, "agy");
#[derive(Deserialize)]
#[serde(tag = "event")]
enum Event {
    #[serde(rename = "init")]
    Init { conversation_id: String },
    #[serde(rename = "step_update")]
    Step { step_update: Step },
    #[serde(rename = "result")]
    Result { result: ResultWire },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
struct Step {
    conversation_id: String,
    step_type: String,
    text_delta: Option<String>,
}
#[derive(Deserialize)]
struct ResultWire {
    conversation_id: String,
    status: String,
    response: String,
    error: Option<String>,
    usage: Usage,
}
#[derive(Deserialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
}
fn decode(bytes: &[u8]) -> Result<Vec<E>, ProviderError> {
    Ok(match wire::<Event>(bytes)? {
        Event::Init { conversation_id } => vec![E::Session(conversation_id)],
        Event::Step { step_update } => {
            let mut events = vec![E::Session(step_update.conversation_id)];
            if step_update.step_type == "agent_response"
                && let Some(text) = step_update.text_delta
            {
                events.push(E::Text(text));
            }
            events
        }
        Event::Result { result } => {
            let mut events = vec![E::Usage {
                input: result.usage.input_tokens,
                output: result.usage.output_tokens,
            }];
            if !result.conversation_id.is_empty() {
                events.push(E::Session(result.conversation_id));
            }
            if result.error.is_some() || result.status != "SUCCESS" {
                events.push(E::Error);
            } else {
                events.push(E::Text(result.response));
                events.push(E::Terminal);
            }
            events
        }
        _ => vec![],
    })
}
