//! Return record for the provider output cycle.
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Exact `parse_output` return shape, including null fields and numeric retry evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedOutput {
    pub status: String,
    pub text: String,
    pub session_id: Option<String>,
    pub observed_model: Option<String>,
    pub reset_at: Option<String>,
    pub retry_after: Option<Value>,
    pub signature: Option<String>,
    pub excerpt: String,
    pub terminal: bool,
    pub question: Option<String>,
    pub failure_text: String,
}
impl Default for ParsedOutput {
    fn default() -> Self {
        Self {
            status: "failed".into(),
            text: String::new(),
            session_id: None,
            observed_model: None,
            reset_at: None,
            retry_after: None,
            signature: None,
            excerpt: String::new(),
            terminal: false,
            question: None,
            failure_text: String::new(),
        }
    }
}

/// `_observed_model` returns the observed id and its source independently nullable.
pub type ObservedModel = (Option<String>, Option<String>);
