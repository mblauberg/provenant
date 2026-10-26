# Landing lease

Call `fabric_context` (`op: inspect`) to verify the selected project and inbox.
Lease actions use the `fabric_lease` tool. `landing_acquire` takes `reference`
(for example `refs/heads/main`), `expected` and `candidate` OIDs,
`lease_seconds` and an `operation_id`. Keep the returned `proof` private.
`landing_renew`, `landing_verify` and `landing_release` take that `proof`;
`landing_renew` and `landing_release` also need a unique `operation_id`, and
retries reuse that ID and identical input.

`landing_wait` observes another generation (`reference`, `generation`,
`wait_seconds`). The budget is 1 to 55 seconds; a timeout means the observation
expired. Waiting never acquires a lease. A lease is advisory coordination, not
push authority.

A terminal without MCP uses the same operations through `provenant fabric
landing acquire|renew|verify|release|wait`. `wait` takes `--wait` in seconds (1
to 14400).

Only the CLI fences a push: `provenant fabric landing push` and `provenant
fabric landing reconcile` settle its recorded outcome. The fence flags are
`--reference`, `--id` and `--generation`, copied from the lease `proof`. `push`,
`reconcile`, `renew` and `release` also take a unique `--operation-id`; `renew`
also takes `--lease-seconds`; `verify` takes the fence flags alone. All of them
read the lease token from `PROVENANT_LANDING_TOKEN`, so it never enters the
command arguments. The proof `token` is a string of 32 lower-case hex digits,
matching the CLI. With `PROOF` holding the proof object as JSON and `OP` set to
a unique operation ID, save this as a script, or run it as `sh -c '...'`,
because `set -e` and `exit` would close an interactive terminal:

```sh
set -e
REF=$(printf '%s' "$PROOF" | jq -er .reference)
ID=$(printf '%s' "$PROOF" | jq -er .id)
GEN=$(printf '%s' "$PROOF" | jq -er .generation)
TOKEN=$(printf '%s' "$PROOF" | jq -er .token)
[ "${#TOKEN}" -eq 32 ] || { echo "bad landing token" >&2; exit 1; }
: "${OP:?set OP to a unique operation ID}"
PROVENANT_LANDING_TOKEN=$TOKEN provenant fabric landing push \
  --reference "$REF" --id "$ID" --generation "$GEN" --operation-id "$OP"
```

Ownership refusal exits 1.
