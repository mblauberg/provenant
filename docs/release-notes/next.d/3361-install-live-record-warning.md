## Behaviour changes

### CLI and install

- `install` and `upgrade` now warn on stderr and name the live attempts when a release changes the live-attempt record format and attempts are still live. The warning also appears as `live_record_warning` in `--json` output. Install never refuses on it; drain the named attempts first or expect them to settle under the new release. See the [install runbook](../../runbooks/install.md#upgrade). (#3361)
