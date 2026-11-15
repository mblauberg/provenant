## Breaking changes
### Lanes and settlement

- **Move repository setup and verification declarations to ports.** Replace `domains.worktrees.setup` and project setup lists with the `post_create` command port, and move `.provenant/verify.json` declarations to `.provenant/ports.json`. The [consumer migration guide](../../runbooks/using-provenant-2-from-another-repository.md) explains ports. (#3078)
