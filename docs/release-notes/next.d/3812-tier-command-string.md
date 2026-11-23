## Additions
### CLI and install

- `verify.tiers[].commands[]` in `ports.json` now accepts a shell-word string as
  shorthand for a `TierCommand` table: `cargo fmt {packages} -- --check` splits
  into argv without a shell, with `stack` and `cwd` left at their defaults. A
  string that does not split warns as a malformed tier command and names its
  leaf. (#3812)
