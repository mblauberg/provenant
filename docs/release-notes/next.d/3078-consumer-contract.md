## Breaking changes
### CLI and install

- **Repositories that use Provenant have a stated contract.** Each lane variable, repository file, exit code and the `provenant process-tree --json` fields are marked stable, provisional or internal in the [consumer contract](../../specs/consumer-contract.md). Run files are internal, so read them through `provenant status --json`, `provenant output` or `provenant trace`. Replace `provenant dispatch` with `provenant run TEMPLATE BRIEF`. The [adoption guide](../../runbooks/using-provenant-2-from-another-repository.md) covers what changed, opting into ports, running beside a repository's own tooling and rolling back to 1.x. (#3078)
