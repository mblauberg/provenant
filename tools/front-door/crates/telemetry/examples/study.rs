//! `cargo run -p provenant-telemetry --example study -- INPUT.json [--json]`
//!
//! Reads a recorded `StudyInput` and prints the token-per-merged-PR study.
//! Offline: no network, no clock. Method: docs/telemetry/token-per-pr-study.md.
#![allow(clippy::print_stdout, clippy::print_stderr)]
use provenant_telemetry::study::{StudyInput, reduce, render};

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: study INPUT.json [--json]");
        return std::process::ExitCode::from(2);
    };
    let result = std::fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|text| serde_json::from_str::<StudyInput>(&text).map_err(|e| e.to_string()))
        .and_then(|input| reduce(&input));
    match result {
        Ok(study) if args.iter().any(|a| a == "--json") => {
            match serde_json::to_string_pretty(&study) {
                Ok(text) => println!("{text}"),
                Err(e) => {
                    eprintln!("study: {e}");
                    return std::process::ExitCode::FAILURE;
                }
            }
            std::process::ExitCode::SUCCESS
        }
        Ok(study) => {
            print!("{}", render(&study));
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("study: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
