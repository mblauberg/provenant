## Breaking changes
### CLI and install

- **`wait --json` emits settlement events before its final result.** Read the final JSON record for the command envelope, or consume each event as it arrives. `--any` returns on settlement, including a node finishing within a running graph; a node starting does not end the wait. (#3078)
