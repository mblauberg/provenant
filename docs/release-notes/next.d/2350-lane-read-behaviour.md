## Behaviour changes
### CLI and install

- `provenant lanes --all` lists the newest lanes first. The default listing keeps its order (#2350).
- A lane selection that matches nothing asks for a unique prefix of 8 or more characters or the full ID. When a peer is configured it also names `--host HOST` for that peer's lanes (#2350).
- `provenant disk report` prints the volume and each class total as plain key-value lines instead of an escaped JSON string. The JSON output is unchanged (#2350).
