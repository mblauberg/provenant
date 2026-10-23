//! Small compatibility contracts awaiting their shared owners.
use serde::{Deserialize, Serialize};

// port: belongs in provenant-fabric-runtime
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub project: String,
    pub agent_id: String,
    pub provider: String,
}

// port: belongs in provenant-core
/// ECMAScript trim, which differs from Rust's Unicode whitespace set.
pub(crate) fn js_whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

// port: belongs in provenant-supervisor
/// Runtime-supplied UUID generator. The store never reads a global random source.
pub trait IdSource {
    fn uuid(&self) -> crate::Result<String>;
}

