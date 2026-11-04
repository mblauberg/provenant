# Sign-in failure fixture provenance

`cursor/auth-required.stderr` reproduces the diagnostic quoted in issue #2744:
“Authentication required. Please run agent login”. It comes from the issue's
reported failing run; it is not a fresh capture from this test environment.

The Codex, OpenCode and agy `auth-required.stderr` files are modelled diagnostic
samples. They contain no account data and are not captured provider transcripts.
They exercise the adapter marker table and terminal classification with the
expected sign-in instructions. Replace them with sanitised, versioned stderr
captures when those are available; do not treat these samples as independent
proof of a particular provider release's output.

These tests never execute provider logins or inspect authentication stores.
