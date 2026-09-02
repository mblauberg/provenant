# ADR 0024 — Git history and ADRs own the change record

**Status:** Accepted 2026-09-02 (issue [#765](https://github.com/mblauberg/provenant/issues/765))

## Context

`CHANGELOG.md` had grown to roughly 20 KB with every entry under a single
`## Unreleased` heading, covering more than 1,500 commits without a release
ever closing the section. Nothing read the file: no script parsed it, no test
asserted on it, no release step consumed it, and its only mechanical role was
one entry in the CI path filter. It was a hand-maintained third copy of
information that Git already records exactly and that the ADRs already record
with reasoning.

The choice was to close the section by cutting a `0.1.0` tag, or to drop the
file. Keeping it meant paying an ongoing curation cost for a record with no
reader, and every prior attempt had drifted rather than being trimmed.

## Decision

The user chose to drop the file. `CHANGELOG.md` is deleted and is not
reinstated.

The change record has two owners:

- `git log` owns what changed and when. It is complete, generated, and cannot
  drift from the tree it describes.
- `docs/adr/` owns why a decision was taken, what it amends or supersedes, and
  what follows from it. Decisions land as ADRs, not as changelog lines.

Issues and pull requests remain the working record of in-flight change under
ADR 0011; they are not a third history owner.

If Provenant is later published on a versioned cadence with an audience that
needs a curated summary, release notes are generated for that release from the
tag range at the time. That is a release artifact, not a file maintained
between releases, and reviving a running changelog would need a fresh decision.

## Consequences

One less hand-maintained document, and no more entries written for a section
that never closed. Anyone asking what changed reads `git log`; anyone asking
why reads the ADR index. The CI path filter loses its `CHANGELOG.md` entry, and
no job, script, test or skill referenced the file, so nothing else changes.

The cost is that there is no single curated narrative of the pre-release tree.
That narrative was not being read, and the ADRs carry the decisions that made
it worth reading.
