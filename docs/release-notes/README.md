# Release notes

Published versions are on [GitHub Releases](https://github.com/mblauberg/provenant/releases). [Pending fragments](next.d/) feed the next release; the [release procedure](../../MAINTAINING.md#change-history) covers collation.

## Stability

The [consumer contract](../specs/consumer-contract.md#stability-tiers) sets which release may change each item. There is no deprecation period and there are no compatibility shims. Upgrade every host together; mixed peer versions are unsupported. Each release lists breaking changes first.

## Writing a fragment

Add one file per user-visible change to `next.d/`, named `<issue>-<topic>.md`. Follow the `writing` skill's release-note guidance. Line 1 is one section heading (`## Breaking changes`, `## Behaviour changes`, `## Additions` or `## Fixes`), then an area heading (`### <area>`) from the list at the top of `scripts/collate-release-notes`, then a concise bullet ending in the issue number. One section per fragment; use two fragments for two sections. Breaking changes state the required action. Link a guide for detail and omit internal-only changes. Paths are relative to `next.d/`.

`scripts/collate-release-notes --check` runs in CI and in `scripts/prepare-release`. It fails, naming file and line, on anything collation would drop or misfile: a later `##` heading, or content before the first heading. An unknown section or area only warns and lists the valid choices; the entry is filed under Additions or left untagged.

Keep `_intro.md` to two sentences about the release and its breaking changes. Preview with `scripts/collate-release-notes`.
