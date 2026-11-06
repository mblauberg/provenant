## Fixes
### Lanes and settlement

- Lane rows keep the Provenant-written facts that a refusal or retry needs. An unresolvable configured command, an unconfigured host-native route and an unobserved provider identity carry the typed `terminal_cause` values `command_unavailable`, `host_native_unconfigured` and `identity_unobserved`. The command refusal names the configured executable, and a policy retry names its attempt, for example "retrying this session (attempt 2 of 2)". Neither copies provider output or an operating-system error (#2820).
