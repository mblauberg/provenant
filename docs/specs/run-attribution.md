# Run attribution and correlation

Owner: `crates/run`, `crates/fabric-runtime`
Anchor: `crates/run/src/pr_contribution.rs`, `crates/run/src/correlation.rs`

Test paths are relative to `tools/front-door/`.

## PR contributions

An attempt-to-PR link is idempotent and its weight is unknown until a complete
allocation is sealed. A sealed allocation replays transactionally; conflicting
replays or additional links are refused. An attempt counts once per PR. Branch
generations prevent later work from being attributed to a closed binding.
Ambiguous or truncated coverage must not be counted as a complete allocation.
- Why: attribution must not double-count work or invent a share.
- Test: `src/pr_land_tests.rs::pr_contribution_roundtrip`

## Work correlation

The host seals attempt membership before launch; nested membership comes from
the host journal, not lane-supplied bytes. Membership does not prove a session,
launch or authority. A graph-only match cannot be verified as complete, and
contradictory facts take precedence over missing evidence.
- Why: correlation must not turn partial or untrusted facts into authority.
- Test: `crates/run/src/correlation.rs::tests`

Chair association is bound at first launch and inherited by later attempts;
a conflicting chair is refused. Association outlives payload collection.
- Why: a graph must retain its chair identity across continuation and retention.
- Test: `crates/fabric-runtime/src/chairs.rs::tests::associations_are_keyed_by_project_and_graph_and_inherit_by_name`

Nested attempts share their graph's budget, cancellation and trace; the host
resolves the caller from custody-bound state and lanes receive no journal grant.
- Why: a child must not acquire authority by asserting its own identity.
- Test: `src/fabric/interface.rs::explicit_writer_children_use_their_selected_project_custody`
