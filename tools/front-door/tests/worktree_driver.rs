// Link the library's fixture-gated worktree harness instead of compiling the
// library source inline. The workspace's unsafe-code prohibition applies too.
use provenant::worktree_tests;

fn main() -> std::process::ExitCode {
    worktree_tests::main()
}
