# Landing lease

Call `fabric_identity` to verify the selected project and inbox.
Lease actions use the `fabric_landing` tool. `acquire` takes `reference`
(for example `refs/heads/main`), `expected` and `candidate` OIDs, `lease_ms`
and an `operation_id`. Keep the returned `proof` private. `renew`, `verify` and
`release` take that `proof`; `renew` and `release` also need a unique
`operation_id`, and retries reuse that ID and identical input.

`wait` observes another generation (`reference`, `generation`, `wait_ms`). The
budget is 1000 to 55000 milliseconds; a timeout means the observation expired.
Waiting never acquires a lease. A lease is advisory coordination, not push
authority.

A terminal without MCP uses the same operations through
`provenant fabric landing acquire|renew|verify|release|wait`. `wait` takes
`--wait` in seconds (1 to 14400).

Only the CLI fences a push: `provenant fabric landing push` and
`provenant fabric landing reconcile` settle its recorded outcome. Both take
`--reference`, `--id`, `--generation` and `--operation-id`, copied from the
lease `proof` and a unique operation ID. The proof `token` is a JSON array of
16 integers; the CLI needs it as 32 lowercase hex digits in
`PROVENANT_LANDING_TOKEN`, so it never enters the command arguments. Convert
and run, with `PROOF` holding the proof object as JSON:

```sh
export PROVENANT_LANDING_TOKEN=$(printf '%s' "$PROOF" |
  jq -r '.token | map("0123456789abcdef"[(./16|floor):(./16|floor)+1]
    + "0123456789abcdef"[(.%16):(.%16)+1]) | join("")')
provenant fabric landing push --reference "$REF" --id "$ID" \
  --generation "$GEN" --operation-id "$OP"
```

Ownership refusal exits 1.
