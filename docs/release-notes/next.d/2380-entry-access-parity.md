## Fixes

- Entry access checks agree with the confinement compiler: read and exec roots inside a Codex standalone release are granted like compile grants them, while write roots keep the credential-root refusal. A refused credential root is now reported with the flag and the root that carried it.
