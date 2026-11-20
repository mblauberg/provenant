## Fixes

### Routing

- Monthly pacing uses available ledger usage and an inferred cycle when cycle evidence is missing, with a sourced warning instead of holding an account on uncertainty. Refused launches now explain the route and pacing hold. (#3690)

- Account pacing overrides also apply to the final allowance reservation, and `config set` accepts documented typed settings such as the OpenCode Go allowance preference. Unprobed OpenCode accounts remain eligible with a warning. (#3690)
