## Behaviour changes

### CLI and install

- `provenant workflow session save --project PATH` now warns and saves under the selected project when `PATH` names a project other than the working directory's, instead of refusing with `SESSION_PROJECT_MISMATCH`. The warning names both paths in the JSON `warnings` and the human output; a session registered under another project still needs an explicit `--project PATH` to be saved from elsewhere.
