## Fixes
### Accounts, quota and routing

- `provenant quota` reports an agy account whose isolated keychain is missing as `KEYCHAIN_UNAVAILABLE` instead of an authentication failure. The report row (`status: "keychain_unavailable"`) carries a `repair` line naming the expected keychain path and how to re-register or recreate it, and `provenant hosts doctor` lists the same check for every registered agy account (#2645).
- The menu bar app shows a `keychain_unavailable` account as needing action, like a sign-in is required, and now shows any unrecognised account or source status as unknown instead of failing to read the whole quota report (#2645).
