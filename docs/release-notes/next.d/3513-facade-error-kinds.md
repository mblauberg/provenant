## Fixes

### Lanes and settlement

- Keep run refusal causes distinct at the CLI and Fabric facade: the formerly collapsed secret, refusal, unsupported, writer-scope, confinement, disk, memory, lane-cap, build-setting and guardian-identity kinds now carry their own codes and next steps, and a held writer lease is a typed `writer_lease_held` with local holder facts. `--branch B --base REF` binds its managed worktree from the primary checkout or a linked worktree with the cause preserved when it cannot. (#3513)
