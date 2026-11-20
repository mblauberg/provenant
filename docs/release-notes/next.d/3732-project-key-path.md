## Fixes

### CLI and install

- `provenant config path` now reports the ProjectId binding directory used by the host-project ports reader, so following the path no longer creates a stray directory under `projects/`; the 64-hex target-seed key is used only for target-seed ownership. The binding inventory warns and skips an unrecognised entry instead of refusing every project, and the malformed-port and custody-inventory warnings it caused degrade to that single clear warning, while a corrupt recognised binding still refuses (#3732)
