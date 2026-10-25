# Landing lease

Call `fabric_identity` to verify the selected project and inbox.
Lease actions use the `fabric_landing` tool. `acquire` takes `reference`
(for example `refs/heads/main`), `expected` and `candidate` OIDs, `lease_ms`
and an `operation_id`. Keep the returned `proof` private. `renew`, `verify` and
`release` take that `proof`; `renew` and `release` also need a unique
`operation_id`, and retries reuse that ID and identical input.

`wait` observes another generation (`reference`, `generation`, `wait_ms`). The
budget is 1–14400 seconds; a timeout means the observation expired. Waiting
never acquires a lease. A lease is advisory coordination, not push authority.

Only the CLI fences a push: `provenant fabric landing push` and
`provenant fabric landing reconcile` settle its recorded outcome. Both take the
private 32-digit hex token through `PROVENANT_LANDING_TOKEN`; it never enters
the command arguments. Ownership refusal exits 1.
