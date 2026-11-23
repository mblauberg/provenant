## Breaking changes
### CLI and install

- Suggested next commands are typed `next` entries in the JSON envelope, each
  with an `argv` array of literal arguments and a `why` sentence. Command owners
  build these values, and the binary edge renders the human `next:` line from the
  same array, so JSON and human output cannot diverge. Flags equal to the
  caller's environment defaults (`--state-dir`, `--project`) are omitted from the
  printed command. Read suggested commands from the `next` arrays instead of
  scraping the printed line. (#3814)
