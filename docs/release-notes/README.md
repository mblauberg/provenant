# Release notes

Released versions, newest first:

- [1.2.1](v1.2.1.md)
- [1.2.0](v1.2.0.md)
- [1.1.0](v1.1.0.md)
- [1.0.0](v1.0.0.md)

[Pending fragments](next.d/) describe changes for the next release. The
[maintenance procedure](../../MAINTAINING.md#change-history) covers collation
with [the release-note script](../../scripts/collate-release-notes) and updating
this version list when publishing a release.

## Stability

Provenant makes no stability promise. Commands, flags, `--json` output,
`config.toml` keys, the state layout and Fabric MCP tool names and schemas can
change in any release, with no deprecation period and no compatibility shims.
Upgrade every host together, because peers on different versions are not
supported. Each release lists its breaking changes first. This is the only
place the policy is stated; release notes link here and do not repeat it.

## Writing a fragment

Follow the [release-notes reference](../../skills/engineering-writing/references/release-notes.md).
Add one file per user-visible change under `next.d/`, named `<issue>-<topic>.md`.

```markdown
## Fixes

- `status` no longer reports a finished run as running (#123).
```

- **First line.** Exactly one of `## Breaking changes`, `## Behaviour changes`,
  `## Additions` or `## Fixes`. No other heading is allowed in a fragment.
- **Body.** One to three bullets, each a finished sentence for the published
  notes: subject first, in the user's terms, ending with `(#issue)`. A breaking
  change also says who is affected and what to do instead, in up to five
  sentences. Write `**The change.** Action.` for breaking entries.
- **Do not include.** Stability or version boilerplate, an intro, mechanism or
  failure-sequence detail, slice or wave numbers, internal-only changes, or
  entries copied from commit messages. A change no user can notice needs no
  fragment.
- **Links.** Paths are relative to `next.d/`. Link to a guide or ADR for depth
  instead of explaining it in the entry.
- **Intro.** `next.d/_intro.md` is two sentences on what the release is about,
  a one-line link to this page's Stability section, and an upgrade note only if
  the release breaks something. Maintainers write it last.
