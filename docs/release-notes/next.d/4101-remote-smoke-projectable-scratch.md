## Fixes
### Fabric and verification
- `provenant upgrade --verify --host PEER` now builds its smoke fixture in an owner-only scratch project under the home-relative `domains.smoke.fixture_root` instead of the system temporary directory. A peer can only name a project whose canonical common directory is relative to its home, so the old `$TMPDIR` fixture could never be projected and a remote smoke refused at `task--read` with `peer_project_unprojectable`. The `--dry-run` preview uses the same root. (#4101)
