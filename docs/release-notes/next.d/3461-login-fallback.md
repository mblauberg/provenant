## Fixes

### Accounts, quota and routing

- `provenant` skips unresolved and login-required accounts and continues to the
  next route after a local launch preparation failure. Unpinned fallback routes can use
  each candidate's account. (#3461)
- Login failures found during preparation are recorded for the whole account and
  remain unavailable until a newer successful login check or sign-in. Mixed
  candidate exclusions retain route fallback eligibility. Graph fallback briefly
  rechecks missing custody evidence across live settlement and resume before
  refusing. (#3461)
