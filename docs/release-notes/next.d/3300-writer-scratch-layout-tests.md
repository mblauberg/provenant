## Fixes

### Lanes and settlement

- The writer preparation scratch test no longer runs a single oversized fixture that timed out on macOS. Each GitHub credential layout (missing token, linked config, linked XDG config) now has its own test, and the continuation phase records an effective sandbox so it actually exercises copying retained session state into the resumed attempt's private provider state. The test leaves quarantine and its per-test timeout override is gone. (#3300)
