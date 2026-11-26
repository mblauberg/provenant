## Fixes
### Console and app fixtures

- The Swift replay fixtures for `console` and `output` are now generated from the same pure projection functions the CLI commands call, over one synthetic set of loaded values. No console envelope is hand-written in the generator, so a change to any console projection fails `scripts/check-swift-fixtures` until the fixtures and Swift decoders are updated (#3788).
- An untouched `# Console checkpoint` template no longer becomes a chair's derived summary line; the summary is unavailable until the checkpoint has real content (#3788).
- Installing with `PROVENANT_APPS_DIR` set away from `~/Applications` warns when an older bundle remains at the default path, instead of leaving a stale app silent (#3788).
