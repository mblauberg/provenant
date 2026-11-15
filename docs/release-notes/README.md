# Release notes

Published versions are on [GitHub Releases](https://github.com/mblauberg/provenant/releases). [Pending fragments](next.d/) feed the next release; the [maintenance procedure](../../MAINTAINING.md#change-history) covers collation.

## Stability

The [consumer contract](../specs/consumer-contract.md) marks each command, output, variable and repository file stable, provisional or internal. Stable items change only in a major release; provisional ones may change in a minor release and are named here when they do; internal ones may change in any release. There is no deprecation period and there are no compatibility shims. Upgrade every host together; mixed peer versions are unsupported. Each release lists breaking changes first.

## Writing a fragment

Add one file per user-visible change to `next.d/`, named `<issue>-<topic>.md`. Follow the `writing` skill's release-note guidance. Use a section heading (`Breaking changes`, `Behaviour changes`, `Additions` or `Fixes`), then an area heading from the list below, then a concise bullet ending in the issue number. Breaking changes state the required action. Link a guide for detail and omit internal-only changes. Paths are relative to `next.d/`.

Areas: CLI and install; Lanes and settlement; Git and safety; Accounts, quota and routing; Pipelines and graphs; Chair, inbox and console; Fabric; Telemetry and providers.

Keep `_intro.md` to two sentences about the release and its breaking changes. Preview with `scripts/collate-release-notes` and check with `scripts/collate-release-notes --check`.
