## Changes

### Verification and landing

- Approval carry now runs the port table's declared `verify.format` command
  instead of guessing a formatter from the verify tiers. With no declaration,
  stack detection supplies a default and the carry receipt names whether the
  command was detected or configured; setting `verify.format` to `none` turns
  format carry off and says so. (#3540)
