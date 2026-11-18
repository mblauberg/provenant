## Fixes

### Accounts, quota and routing

- `provenant` skips unresolved and login-required accounts and continues to the
  next route after a local launch preparation failure. Unpinned fallback routes can use
  each candidate's account. (#3461)
