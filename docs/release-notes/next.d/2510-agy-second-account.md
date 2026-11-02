## Changes
### Accounts

- A second agy Google account can be registered with its own keychain. Give it a home directory holding `Library/Keychains/login.keychain-db`; `accounts register --adapter agy --directory <home>` takes its identity from the sign-in under that `HOME`, lanes link only that keychain, and `quota` probes it as its own row. Registered agy accounts are now pinned and verified at launch. A directory without the keychain file keeps the shared login keychain. Owner steps are in the [providers runbook](../../runbooks/providers.md#agy-accounts) (#2510).
