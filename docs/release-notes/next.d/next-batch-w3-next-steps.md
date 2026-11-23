## Breaking changes
### CLI and install

- Suggested next commands omit `--state-dir` when it is the default the caller's
  environment resolves, and appear as `next` argv arrays in the JSON envelope
  instead of separately formatted text. Read suggested commands from the `next` argv arrays. (#3814)
