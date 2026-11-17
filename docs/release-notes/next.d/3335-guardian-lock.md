## Fixes

### Lanes and settlement

- Guardians retain a dedicated kernel lock through terminal settlement, so
  recovery can prove same-boot guardian loss alongside free writer custody and
  a clear worktree census. Automatic settlement still requires Run driver
  integration. (#3335)
