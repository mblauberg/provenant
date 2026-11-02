//! Prints every published wire schema as one JSON object, `{file: schema}`, for
//! `scripts/generate-reference`. Not part of the shipped binary set.
use std::io::Write;

fn main() -> std::process::ExitCode {
    let mut files = serde_json::Map::new();
    for (name, schema) in provenant_records::wire::published() {
        files.insert(name, schema);
    }
    files.insert(
        "error-codes".to_owned(),
        provenant_records::wire::edge_error_codes(),
    );
    let mut out = std::io::stdout().lock();
    match serde_json::to_writer(&mut out, &serde_json::Value::Object(files)) {
        Ok(()) if writeln!(out).is_ok() => std::process::ExitCode::SUCCESS,
        _ => std::process::ExitCode::FAILURE,
    }
}
