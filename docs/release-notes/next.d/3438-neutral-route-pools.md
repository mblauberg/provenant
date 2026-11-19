## Behaviour changes

### Accounts, quota and routing

- Bundled route-pool weights are neutral: every member carries the catalogue's
  default weight and ranking is tiered by capability preference alone, so no
  account, plan or provider is favoured by the shipped defaults. Account- and
  plan-specific weights belong in local configuration. `route list` now reports
  the layer that set each weight (bundled, host config or project config), not
  just the pool member's own source. (#3438)
