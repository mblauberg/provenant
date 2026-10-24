use crate::{ProviderError, ProviderEvent as E, stream::wire};
use serde::Deserialize;
pub struct Claude;
crate::adapter!(Claude, "claude");
crate::quota_source!(Claude, "claude");
#[derive(Deserialize)]
#[serde(tag = "type")]
enum Event {
    #[serde(rename = "system")]
    System { subtype: String, session_id: String },
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
        result: Option<String>,
        #[serde(default)]
        api_error_status: Option<u16>,
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
        } => {
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
            let mut events = vec![E::Session(session_id)];
            if error.is_none() && !is_api_error_message && message.model != "<synthetic>" {
                events.push(E::Model(message.model));
            }
            if let Some(error) = error {
                events.push(if error == "authentication_failed" {
                    E::AuthRequired
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
            api_error_status,
        } => {
            let mut events = vec![E::Session(session_id)];
            if api_error_status == Some(401) {
                events.push(E::AuthRequired);
            } else if is_error || subtype != "success" {
                events.push(E::Error);
            } else {
                if let Some(result) = result {
                    events.push(E::Text(result));
                }
                events.push(E::Terminal);
            }
            events
        }
        _ => vec![],
    })
}

/// Metadata only. The caller must never read a token to establish presence.
#[derive(Clone, Copy)]
pub enum CredentialPresence {
    Present,
    Absent,
    Unavailable,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SigninState {
    SignedIn,
    SignedOut,
    CredentialStorePresent,
    KeychainUnavailable,
    Unknown,
}
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SigninStatus {
    pub state: SigninState,
    /// A local status probe does not establish network token validity.
    pub token_validity: String,
}
/// Decode only the CLI's login bit. Account identifiers and diagnostics never
/// escape this boundary; a negative probe alone cannot prove sign-out over SSH.
pub fn signin_status(
    stdout: &[u8],
    exit_code: Option<i64>,
    presence: CredentialPresence,
) -> SigninStatus {
    #[derive(Deserialize)]
    struct Status {
        #[serde(rename = "loggedIn")]
        logged_in: bool,
    }
    let state = match wire::<Status>(stdout) {
        Ok(status) if status.logged_in && exit_code == Some(0) => SigninState::SignedIn,
        Ok(status) if !status.logged_in => match presence {
            CredentialPresence::Present => SigninState::CredentialStorePresent,
            CredentialPresence::Absent => SigninState::SignedOut,
            CredentialPresence::Unavailable => SigninState::KeychainUnavailable,
            CredentialPresence::Unknown => SigninState::Unknown,
        },
        _ => SigninState::Unknown,
    };
    SigninStatus {
        state,
        token_validity: "unverified".into(),
    }
}
