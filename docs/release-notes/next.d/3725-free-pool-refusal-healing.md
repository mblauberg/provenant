## Fixes

### Accounts, quota and routing

- A recorded free-pool refusal no longer holds every lane on the account for a
  whole provider retry-after after the provider has recovered. A later
  successful launch on the same pool clears it with source `observed_success`,
  and `provenant quota tick` re-checks a free-pool refusal older than a bounded
  interval with the adapter's registered quota probe, keeping a refusal that
  the re-check confirms. (#3725)
