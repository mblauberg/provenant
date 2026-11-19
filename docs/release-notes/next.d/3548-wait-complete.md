## Additions

### CLI and install

- `wait --pr N --until complete` waits for all observed checks and head-filtered Actions workflow runs to finish (falling back with a warning when runs are unavailable) and reports every failed or cancelled check with its available link. The default still returns on the first failure. (#3548)
