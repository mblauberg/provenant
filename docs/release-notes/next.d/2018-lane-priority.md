## Behaviour changes

- On macOS, Fabric lowers writer provider priority by default to nice 10; read-only lanes default to 5 and build-heavy lanes to 15. Configure these values or use `--build-heavy` for build-intensive work, and inspect the applied value with `lanes --json` (#2018).
- Peer hosts must upgrade together to use the updated Fabric protocol (#2018).
