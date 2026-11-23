## Fixes
### CLI and install

- `provenant upgrade --verify` now re-executes the installed destination binary
  to run its smoke verification, so the verifier that ships in a release is the
  one that checks that release; the report names the version that verified.
  Install also retires a leftover `cargo-target-seeds/` directory under the state
  root, reporting its path and size once, and the smoke fixture leaves no
  registered scratch project for `tick` to read. (#3799 #3813 #3812)
