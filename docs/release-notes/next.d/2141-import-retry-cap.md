## Fixes

- Count only attempts that reach object import towards the three-attempt limit,
  back off between claims, and release custody on terminal import refusal while
  retaining private Git for host recovery.
