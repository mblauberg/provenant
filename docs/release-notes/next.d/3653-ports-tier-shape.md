## Behaviour changes

### CLI and install

- `config/ports.json` tier commands are now `{stack, cwd, argv}` objects. A binary older than this release reads the new shape as malformed and ignores the whole project port layer (verify, land and admission), warning "malformed port layer ignored", until the host is upgraded. (#3654)
