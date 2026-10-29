## Behaviour changes
### Lanes and settlement

- On macOS, lanes now run at lower priority by default: nice 10 for writers, 5 for read-only lanes and 15 for build-heavy lanes. Set these values in configuration, pass `--build-heavy` for build-intensive work, and read the applied value from `lanes --json` (#2018).
