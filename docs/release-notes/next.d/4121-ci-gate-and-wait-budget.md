## Fixes

### CI and checks

- The generated-artefacts job is now declared in the `rust` suite manifest, so `ci-status` gates it like every other job it waits for and a manifest/workflow mismatch no longer fails repository checks. (#4121)
- Wait budgets read their remaining time through the deadline's public accessor instead of its private field, restoring builds of `provenant-core` after deadlines moved into their own module. (#4121)
