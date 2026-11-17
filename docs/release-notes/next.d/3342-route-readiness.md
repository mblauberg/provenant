## Behaviour changes

### Accounts, quota and routing

- Unknown provider errors whose type and status are absent or unknown are classified as `unclassified`, not `usage_limited`: they preserve the provider message, retry once without marking the account exhausted, and fall back across eligible routes and providers at the same tier before stopping at the chair. (#3342)
- Readiness output distinguishes unprobed accounts from probed accounts where the provider returned no figures, and suppresses the `provenant quota tick` probe hint once an account has been probed. (#3342)
- Quota probe failures rank their account below ready or untested accounts during their validity window, and warn on launch when only failed-probe accounts remain unless refused by the `PROBE_FAILURE` launch policy knob. (#3287)
- OpenCode local-ledger estimates and auth reprobing allow unverified accounts to prepare without spurious exhaustion markings or swapping pinned accounts. (#3293)
