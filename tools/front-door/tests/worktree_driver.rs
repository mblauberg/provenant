// Compile the real command edge with test-only native worker observations.
// The workspace's unsafe-code prohibition applies to this target too.
include!("../src/lib.rs");

fn main() -> std::process::ExitCode {
    worktree_tests::main()
}
