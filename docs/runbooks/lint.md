# Repository checks

Use `provenant check harness` for tracked assets and `provenant check writing
PATHS --rules general` for prose. Use `--rules academic` or `--rules legal`
when appropriate. `provenant check release --base OID --head OID` checks a
pinned Git range. Findings return exit 1; invalid input returns exit 2.

The Python branch and commit-message linter and its commit-msg hook are retired.
Follow the [branch and commit doctrine](../../skills/setup-repo/references/doctrine.md)
when preparing changes. The existing `scripts/hooks/pre-push` adapter stays until the Rust
`provenant hook git pre-push REMOTE LOCATION` handler is runnable. Its current
CLI leaf returns unavailable; do not replace a working guard with that stub.
