## Fixes
### Accounts, quota and routing
- Claude lanes no longer launch on a stale login after an account switch. A recorded Claude login failure is cleared at launch only by a probe that inspects the account credential; a CLI `auth status --json` answer still names the original account after its credentials were replaced, so it cannot clear the failure. Launch selects a logged-in account or refuses with the account name and its `claude auth login` remedy. (#3974)
