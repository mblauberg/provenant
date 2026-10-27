## Behaviour changes

- Agy read-only lanes can run system commands such as `git log -p` within
  their existing confinement boundary. Remaining provider tool denials name
  the denied action in the terminal reason.
- Like read-only Codex lanes, read-only Agy lanes can run system programs with
  the lane's existing network access, so read roots must not hold anything the
  lane may not see.
