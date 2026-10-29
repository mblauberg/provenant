//! Every bound on what the CLI prints, in one place. Specs, help constraints
//! and the generated reference quote these values rather than repeating them.

/// A whole encoded result; larger results are published as an artifact.
pub const MAX_RESULT_BYTES: usize = 1024 * 1024;

/// Rows an owner summary shows after its headline.
pub const SUMMARY_ROWS: usize = 12;
/// Longest summary row; longer rows end in an ellipsis.
pub const SUMMARY_ROW_BYTES: usize = 200;

/// The fallback truncator, for owners without a summary: text up to this size
/// is shown whole, larger text keeps its first lines.
pub const FALLBACK_BYTES: usize = 2048;
pub const FALLBACK_LINES: usize = 8;
pub const FALLBACK_ROW_BYTES: usize = 200;

/// `output` shows a result whole up to this size; above it, head and tail.
pub const OUTPUT_BYTES: usize = 12 * 1024;
/// Changed paths `output` names, and the longest path it prints.
pub const OUTPUT_PATHS: usize = 8;
pub const PATH_BYTES: usize = 120;
/// Longest line count `output --tail` returns.
pub const TAIL_LINES: u64 = 500;

/// How the default output is bounded, as reference text for specs and help.
pub fn reference() -> String {
    format!(
        "default output shows {SUMMARY_ROWS} summary rows of at most {SUMMARY_ROW_BYTES} bytes, \
         action rows first; --full shows everything; output shows a result whole up to \
         {} KiB, then its head and tail; --json is never abbreviated",
        OUTPUT_BYTES / 1024
    )
}
