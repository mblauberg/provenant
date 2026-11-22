## Fixes

### CLI and install

Upgrade and install now report the installed Provenant Console app version, and upgrade, install and `hosts doctor` warn when a read version trails the CLI. A bundle that is not installed is reported as missing, without a version comparison; a missing or damaged app asset is a warning rather than a failed CLI upgrade (#3773, #3785).
