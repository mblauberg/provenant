# Release notes

Published versions are on [GitHub Releases](https://github.com/mblauberg/provenant/releases). [Pending fragments](next.d/) feed the next release; the [maintenance procedure](../../MAINTAINING.md#change-history) covers collation.

## Stability

Provenant makes no stability promise. Commands, configuration, state and output may change without a deprecation period or compatibility shims. Upgrade every host together; mixed peer versions are unsupported. Each release lists breaking changes first.

## Writing a fragment

Add one file per user-visible change to `next.d/`, named `<issue>-<topic>.md`. Follow the `writing` skill's release-note guidance. Use a section heading (`Breaking changes`, `Behaviour changes`, `Additions` or `Fixes`), then an area heading from the list below, then a concise bullet ending in the issue number. Breaking changes state the required action. Link a guide for detail and omit internal-only changes. Paths are relative to `next.d/`.

Areas: CLI and install; Lanes and settlement; Git and safety; Accounts, quota and routing; Pipelines and graphs; Chair, inbox and console; Fabric; Telemetry and providers.

Keep `_intro.md` to two sentences about the release and its breaking changes. Preview with `scripts/collate-release-notes` and check with `scripts/collate-release-notes --check`.
