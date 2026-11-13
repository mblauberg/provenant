# Slice T Handoff Notes: Test Pruning in Cut-Over Crates (#3251, #3078)

## 1. What Changed

Applied the #3251 test pruning audit and addendum across the four owned crate groups (`fabric-mcp`, `workflow`, `routing`, `telemetry`):

### `crates/fabric-mcp`
- Deleted checked-in schema snapshot file `tests/fixtures/all-tool-schemas.json` and removed ignored maintenance test `update_schema_snapshot`.
- Rewrote `schema_tests.rs::generated_schema_snapshot_is_checked_in`: derives published operations directly from `catalogue()` and `all_catalogue()`, validating that schema definitions resolve and operations conform to runtime expectations without comparing against a checked-in fixture.
- Rewrote `schema_tests.rs::wire_schema_has_no_dangling_or_single_use_defs`: verifies definitions resolve and are referenced (`*count >= 1`), without rejecting single-use definitions.
- Rewrote `protocol_tests.rs::modern_tools_list_publishes_only_supported_output_schemas`: derives tool catalogue dynamically, checking `outputSchema` presence against supported capabilities without hardcoding tool count 5.
- Rewrote `protocol_tests.rs::published_legacy_initialisation_and_modern_discovery`: tests protocol properties (JSON-RPC version, error codes, version negotiation, serverInfo, tool existence) instead of exact whole-reply JSON matching.
- Merged `render_tests.rs::digest_keeps_routes_without_effort` into `digest_keeps_route_provenance_exact_and_skips_long_routes`. Rewrote tests to assert bounded provenance and confidentiality.
- Merged `tools_tests.rs::submit_is_removed_and_graph_run_is_the_only_launch_operation` into `removed_tools_are_unknown_and_merged_operations_are_typed`.
- In `stdio_tests.rs`, verified capacity limits and busy responses without pinning worker or queue counts.

### `crates/workflow`
- Deleted `session.rs::checkpoint_without_intent_is_reported_on_its_own_line`: presentation on its own line is layout rather than a stated contract.
- Rewrote `session.rs::the_excerpt_notice_carries_the_state_dir_and_stays_within_the_cap`: preserves byte bound `HOOK_OUTPUT_LIMIT` and usable state dir inclusion without pinning notice prose.
- Rewrote `session/diff.rs::tests`: asserted additions, removals, and newline distinctions are faithfully represented without pinning exact hunk header line numbers.
- Rewrote `session/standing.rs::tests`: asserted actionable standing and unknown state are conveyed without pinning fixed notice prose.
- Rewrote `session/state.rs::tests`: preserved bounded output, injection resistance, and actionable commands without pinning exact headings or layout strings.
- Rewrote `writing/tests.rs::rule_sets_retain_vocabulary_and_legal_severity`: derived vocabulary and severity semantics from loaded rules instead of pinning an arbitrary static word list.

### `crates/routing`
- Merged `src/tests/preferences.rs::routing_default_matches_bundled_asset` into `catalog.rs::route_names_include_configured_and_catalogue_pools`, removing the redundant test from `preferences.rs`.
- Rewrote `catalog.rs::tests`: derived warn models, cheaper models, and pool priority from the loaded catalogue and synthetic configurations, removing hardcoded references to today's models and advisory prose.
- Rewrote `policy.rs::tests`: asserted section line emission and compaction window number rendering without pinning fixed guidance prose.
- Rewrote `review.rs::legacy_panel_selection_matrix`: replaced ~160 lines of hardcoded legacy panel mappings with an invariant that iterates `review.shapes`, verifying base lens preservation and risk floor elevations.
- Rewrote `src/tests/bare_model.rs`: tested bare model resolution (unique, ambiguous, unknown) using a small synthetic catalogue; replaced fixed model test `bundled_pools_exclude_high_terra_but_exact_and_user_pool_allow_it` with `pool_excludes_unsupported_effort_but_exact_and_user_pool_allow_it`.
- Rewrote `src/tests/exclusions.rs::account_evidence_reasons_carry_their_repair_text`: asserted every exclusion reason carries non-empty, actionable repair guidance without pinning specific wording.
- Rewrote `src/tests/policies.rs`: derived warn models from the catalogue and tested advisory behavior without hardcoding exact model names.
- Rewrote `src/tests/quotas.rs::selected_windows_cover_pro_cursor_agy_and_free_profiles`: tested pacing calculations, factor basis points, and detail presence using catalogue models and window profiles.
- Rewrote `src/tests/selection.rs::table_driven_weighted_pools_record_effort_readiness_roll_and_seed`: replaced ~400 lines of rigid table cases with property invariants verifying eligible pick membership, exclusion of forbidden candidates, and exact reproducibility under identical seeds.

### `crates/telemetry`
- Rewrote `ingest/tests.rs::incremental_ingest_upgrades_existing_chair_without_rereading`: asserted idempotent attribution after upgrade without pinning `bytes_read == 0`.
- Rewrote `mileage.rs::tests`: retained contractual arithmetic while removing brittle formatted currency string comparisons.
- Merged `src/tests.rs::context_cost_rounding_uses_largest_fractional_remainders` into `fractional_tokens_and_cost_remainders_reconcile_by_sorted_key`, verifying tie-breaking and group sum reconciliation.

## 2. Decisions & Removed Tests with Reasons
- `all-tool-schemas.json` & `fabric-mcp/src/schema_tests.rs::update_schema_snapshot`: Deleted snapshot file and ignored updater; contracts are validated dynamically against runtime schemas.
- `fabric-mcp/src/render_tests.rs::digest_keeps_routes_without_effort`: Merged into `digest_keeps_route_provenance_exact_and_skips_long_routes`.
- `fabric-mcp/src/tools_tests.rs::submit_is_removed_and_graph_run_is_the_only_launch_operation`: Merged into `removed_tools_are_unknown_and_merged_operations_are_typed`.
- `workflow/src/session.rs::checkpoint_without_intent_is_reported_on_its_own_line`: Deleted because line presentation layout is prose rather than a contract.
- `routing/src/tests/preferences.rs::routing_default_matches_bundled_asset`: Merged into `catalog.rs::route_names_include_configured_and_catalogue_pools`.
- `telemetry/src/tests.rs::context_cost_rounding_uses_largest_fractional_remainders`: Merged into `fractional_tokens_and_cost_remainders_reconcile_by_sorted_key`.

## 3. Residual Adaptor Uses
- None introduced. All tests exercise core models, catalog loaders, and existing traits.

## 4. Unverified Items
- Full CI test suites (T0–T3) run on the pushed integration branch in GitHub CI, per repository verification policy.

## 5. Follow-ups
- Coordinate with C2/S2 for integration on `epic-3078-cutover`.
