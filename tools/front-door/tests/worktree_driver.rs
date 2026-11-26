// Link the library's fixture-gated worktree harness instead of compiling the
// library source inline. The workspace's unsafe-code prohibition applies too.
use provenant::worktree_tests;
use std::path::PathBuf;

fn main() -> std::process::ExitCode {
    // Cargo sets CARGO_BIN_EXE_provenant on this test target, but not on the
    // library the harness now lives in. Hand the same path across the seam.
    worktree_tests::main(option_env!("CARGO_BIN_EXE_provenant").map(PathBuf::from))
}
