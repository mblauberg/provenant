# Release Notes

Use when writing or editing release notes, a changelog or release fragments (towncrier, changesets, a `next.d/` directory). Cross-project. The reader is an upgrader: should they upgrade, what must they do, what do they gain. Notes are clerical, not promotional.

## Rules

1. **Lead with the upgrade.** Order: intro, breaking changes with their actions, behaviour changes, additions, fixes. Anything a user must do is a breaking change or sits in the intro's upgrade note, never in a lower section.
2. **Say what changed for the user, not how it works.** Name the command, flag, key or behaviour and its effect. Mechanism, failure sequences and tests go in linked docs or the issue.
3. **Say standing policy once.** Stability, supported platforms and upgrade-together rules live on one stable page. Each release links to it in a line. Never paste a boilerplate section into every release.
4. **Entries stand alone.** Subject first (the command, flag or feature, in code font), one to three sentences, one tense, one name per concept. A breaking entry may take up to five: what changed, who is affected, what to do.
5. **Fixes name the symptom** and the new behaviour. Do not narrate the root cause.
6. **Keep user-actionable numbers** (defaults, limits, units, flags). Drop internal ones (retry counts, schema versions) unless they change what the user must do.
7. **Leave out what no user notices:** refactors, tests, CI, fixes to unreleased code, slice or phase numbers, process words. Merge related changes into one entry. Do not repeat reassurance that something is unchanged.
8. **Reference, do not explain.** End with `(#123)` or `(#123, #124)`. Link a guide for anything longer than the entry.
9. **Group large releases by area.** Once a section passes about a dozen entries, add area sub-headings from a fixed project list, in a fixed order. Within a section, breaking first, then by impact.
10. **Deprecations and removals** name the replacement and the condition or version for removal.

## Template

```markdown
# 1.4.0

What this release is about, in two sentences. Link to the stability policy.
Upgrade note, only if something breaks.

## Breaking changes

- **`old-command` is removed.** Use `new-command`, which takes the same
  arguments. Scripts that call `old-command` fail with exit status 2 (#123).

## Additions

- `export --format csv` writes the report as CSV (#125).

## Fixes

- `sync` no longer hangs when the remote closes the connection early (#126).
```

Omit empty sections.

## Rewrites

Mechanism cut to the fact:

> Before: The scheduler no longer reads the wall clock. It uses the monotonic clock since boot, which all processes share and no clock change can move, so two jobs never start less than one interval apart even if the clock steps back.
> After: Job spacing no longer changes when the system clock does.

Repeated policy replaced by a link:

> Before: A "Stability" section with the same paragraph in every release.
> After: `This project makes no stability promise; see [stability](stability.md).`

## Changelogs

A changelog is the exhaustive record for developers; release notes are curated for upgraders. A changelog follows Keep a Changelog: one section per release, newest first, with version and ISO date; an `Unreleased` section on top; entries grouped as Added, Changed, Deprecated, Removed, Fixed, Security. Never paste a commit log.

## Fragments

Fragment authors write the finished sentence a reader will see, one change per file, with no intro, policy or extra headings. A change nobody outside the code notices needs no fragment. At release, one person reads the collated result end to end, merges entries on one topic, aligns tense and terms, and writes the intro last.

## Checklist

- [ ] Intro is two sentences or fewer; upgrade actions are in Breaking changes or the intro.
- [ ] Policy is linked, not pasted.
- [ ] Every entry opens with its subject, runs one to three sentences, and stands alone.
- [ ] No internal-only change, no process words, no em-dashes, no marketing words.
- [ ] When rewriting: every fact, issue number and breaking change survives, and merges and drops are listed for audit.
- [ ] Read end to end as one document.

## Sources

Retrieved 29 October 2026: [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/); [Common Changelog](https://common-changelog.org/); [Google developer documentation style guide, What's new](https://developers.google.com/style/release-notes); [Microsoft writing tips](https://learn.microsoft.com/en-us/style-guide/global-communications/writing-tips); [Go 1.24](https://go.dev/doc/go1.24) (compatibility promise stated once); [Rust 1.87.0](https://blog.rust-lang.org/2025/05/15/Rust-1.87.0/); [Django 5.2](https://docs.djangoproject.com/en/5.2/releases/5.2/) (upgrade pointer, "Backwards incompatible changes", area grouping); [uv 0.9.0](https://github.com/astral-sh/uv/releases/tag/0.9.0) and [uv versioning policy](https://docs.astral.sh/uv/reference/policies/versioning/); [GitHub changelog](https://github.blog/changelog/); [towncrier](https://towncrier.readthedocs.io/en/stable/tutorial.html); [changesets](https://github.com/changesets/changesets/blob/main/docs/adding-a-changeset.md); [Drew DeVault, How to write release notes](https://drewdevault.com/2021/05/19/How-to-write-release-notes.html) (2021).
