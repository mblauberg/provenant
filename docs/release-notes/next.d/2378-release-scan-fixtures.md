## Fixes

- `provenant check release` suppresses only reviewed path, rule and exact-line digest matches; it reports changed literals and stale fixture entries, and records the suppression count (#2378).
  `crates/cli/src/check/release.rs::tests::reviewed_literal_is_suppressed_but_other_line_at_same_path_is_reported`;
  `::different_history_token_at_a_listed_path_is_reported`;
  `::stale_fixture_entry_is_reported_and_suppression_count_is_noted`
- Owner: `crates/cli/src/check/`
