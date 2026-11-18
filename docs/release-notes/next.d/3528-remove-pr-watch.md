## Breaking changes

### CLI and install

- `provenant pr watch` is removed; use `provenant wait --pr N`. There is no
  alias and no compatibility row: `provenant pr watch` now fails with clap's
  generic unknown-subcommand error (exit 2). (#3528)
