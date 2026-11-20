## Additions

### Lanes and settlement

- Each attempt's `launch.json` now carries a `build` object with the launching binary's `version` and `source_digest`. The digest is handed to the guardian by the running binary, so a launch names the build that actually launched it even when an upgrade lands while the attempt is queued. The field is additive: custody recovery still reads launch records written before it existed. (#3634)
