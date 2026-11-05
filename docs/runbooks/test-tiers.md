# Test tier selection (partial implementation)

`scripts/verify-tier` selects T0 by default and T1 with `--tier 1`. It prints
selected crates, integration targets and their matching paths before running.
Selection compares the working tree and committed changes against `origin/main`;
the base, commands and integration mappings can be overridden in
`.provenant/verify.json`. T0 formats, checks and tests touched crates; T1 runs
mapped integration binaries. The mapping is intentionally incomplete and must
be extended as targets are identified.

T2 and T3 have no default commands yet. Configured commands may be supplied
under `tiers`, keyed by tier number, as arrays of command strings. A configured
runner under `runners` is an argument array whose `{commit}` and `{tier}` fields
are substituted before invocation. Remote execution and its commit provenance
are not yet independently verified. The existing CI workflow remains the source
of full-suite coverage. The entry point does not yet separate build-slot
occupancy from test execution.
