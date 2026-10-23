//! Private compatibility parser shared with the legacy lane until cutover.
#[allow(
    dead_code,
    reason = "The binary only reads pointers; lane code also uses redaction."
)]
#[path = "../crates/supervisor/src/legacy_json.rs"]
mod legacy_json;

pub use legacy_json::{Value, parse};
