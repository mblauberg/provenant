## Fixes
### Checks

- `provenant check release` now ignores byte-pattern hits in the reviewed fixture and example paths while continuing to scan all other content and commit metadata (#2378).
  `crates/cli/src/check/release.rs::tests::reviewed_fixture_paths_suppress_content_hits_only_at_exact_paths`
- Owner: `crates/cli/src/check/`
