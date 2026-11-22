## Behaviour changes
### Accounts, quota and routing

- Lanes can now read account and routing state with `quota show`, `quota history` and `route resolve`. Mutating quota commands (`quota tick`, `quota clear`, `quota display`) stay refused inside a lane because they run provider probes with the chair's credentials and write the chair's quota record; each refusal names the command to run from the chair's session.
