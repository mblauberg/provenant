# Repository checks

Use `provenant check harness` for tracked assets and `provenant check writing
PATHS --rules general` for prose. Use `--rules academic` or `--rules legal`
when appropriate. `provenant check release --base OID --head OID` checks a
pinned Git range. Findings return exit 1; invalid input returns exit 2.

The Python branch and commit-message linter and its commit-msg hook are retired.
Follow the [branch and commit doctrine](../../skills/setup-repo/references/doctrine.md)
when preparing changes. Git pre-push protection uses
`provenant hook git pre-push REMOTE LOCATION`. `install --hooks on` owns
registration of the fixture-backed Rust hooks.
