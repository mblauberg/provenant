# Repository integration ports

## Breaking changes

### Lanes and settlement

- `domains.worktrees.setup` and per-project setup command lists are removed. Declare the repository setup command as the `post_create` command port in `.provenant/ports.json` or host `ports.json`. (#3078)

## Additions

### CLI and install

- `config ports` lists effective backends, sources and unavailable declarations. Repository ports override host ports; naming, post-create, admission, CI observation, tracker and release checks support command backends with execution receipts. (#3078)
