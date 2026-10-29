## Behaviour changes

- Branch names follow Git's own ref-format rules (`git check-ref-format --branch`), so names such as `feat/a+b`, `fix#12`, `user@x` and `@` work. The `caseless` and `unicode-normalization` dependencies and their licence exception are gone.
