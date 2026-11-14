//! Machine-readable owners for scripts/generate-reference; not an installed binary.
use std::io::Write;

fn main() -> std::process::ExitCode {
    let policy: Vec<_> = provenant_records::policy::table()
        .iter()
        .filter_map(|rule| {
            rule.threshold.as_ref().map(|threshold| {
                serde_json::json!({ "threshold": threshold, "effect": rule.behaviour() })
            })
        })
        .collect();
    let data = serde_json::json!({
        "external_contract": provenant_cli::external_contract::external_contract(),
        "policy": policy,
    });
    let mut out = std::io::stdout().lock();
    match serde_json::to_writer(&mut out, &data) {
        Ok(()) if writeln!(out).is_ok() => std::process::ExitCode::SUCCESS,
        _ => std::process::ExitCode::FAILURE,
    }
}
