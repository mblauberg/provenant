# Landing lease

Use `provenant fabric identity --json` to verify the selected project and inbox.
Acquire with `provenant fabric landing acquire --reference refs/heads/main
--expected OID --candidate OID --lease-seconds 300 --operation-id ID`.
Keep the returned `proof` private. Renew, verify and release use `--reference`,
`--id` and `--generation`. Supply the private 32-digit hex token through
`PROVENANT_LANDING_TOKEN`, including for push and reconciliation; it never enters
the command arguments. Renew and release also
require a unique `--operation-id`; retries reuse that ID and identical input.

Observe another generation with `provenant fabric landing wait --reference
refs/heads/main --generation N --wait SECONDS`. The budget is 1–14400 seconds;
exit 124 means the observation expired. Waiting never acquires a lease.
Ownership refusal exits 1. A lease is advisory coordination, not push authority.
Fenced home-host push and reconciliation belong to the Git transport step F7.
