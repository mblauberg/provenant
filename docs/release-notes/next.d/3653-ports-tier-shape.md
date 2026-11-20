## Behaviour changes

### CLI and install

- `config/ports.json` tier commands are now objects. Binaries older than this release warn "malformed port layer ignored" for a repository that uses the new shape, and fall back to their built-in tiers until the host is upgraded.
