# Retired CLI judge

The step 1 Clap parser replaces the router, manual help and Python-compatible
output this suite judged. Its Python runner, differential normaliser and
recorded old outputs retire with those subjects under PORTING R9/R10. Native
contract tests now live in `tools/front-door/tests/parser.rs` and the CLI crate.

The CI workflow still invokes `pytest tests/golden`; it must remove that step
and its `PROVENANT_GOLDEN_*` inputs before this parser PR lands. Workflow edits
are outside the parser lane's write scope. No empty or parity replacement test
is supplied to make that stale invocation succeed.
