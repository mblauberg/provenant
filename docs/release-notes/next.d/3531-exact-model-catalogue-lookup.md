## Fixes

### Accounts, quota and routing

- Exact-model launch and `route list` resolve models through one effective-catalogue
  lookup, so a model reported as `source=discovered` no longer warns that it is
  absent from the catalogue at launch. A truly unknown model still warns and is
  passed to the provider for checking. (#3531)
