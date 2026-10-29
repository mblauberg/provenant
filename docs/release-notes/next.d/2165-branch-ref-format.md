## Behaviour changes
### Git and safety

- Branch names follow Git's ref-format rules (`git check-ref-format --branch`), so names such as `feat/a+b`, `fix#12`, `user@x` and `@` work (#2165).
