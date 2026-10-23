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

// port: belongs in provenant-core
/// JavaScript Number(string) used by the legacy terminal-notice key.
pub(crate) fn js_number(text: &str) -> Option<f64> {
    let text = text.trim_matches(js_whitespace);
    if text.is_empty() {
        return Some(0.0);
    }
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).ok().map(|v| v as f64);
    }
    if let Some(bits) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
        return u64::from_str_radix(bits, 2).ok().map(|v| v as f64);
    }
    if let Some(octal) = text.strip_prefix("0o").or_else(|| text.strip_prefix("0O")) {
        return u64::from_str_radix(octal, 8).ok().map(|v| v as f64);
    }
    if text == "Infinity" || text == "+Infinity" {
        return Some(f64::INFINITY);
    }
    if text == "-Infinity" {
        return Some(f64::NEG_INFINITY);
    }
    if text
        .bytes()
        .any(|b| !matches!(b, b'0'..=b'9' | b'.' | b'-' | b'+' | b'e' | b'E'))
    {
        return None;
    }
    text.parse().ok()
}

// port: belongs in provenant-core
/// A UTF-16 prefix, replacing lone surrogates as the UTF-8 SQLite binding does.
pub(crate) fn js_slice(text: &str, end: usize) -> String {
    String::from_utf16_lossy(&text.encode_utf16().take(end).collect::<Vec<_>>())
}
