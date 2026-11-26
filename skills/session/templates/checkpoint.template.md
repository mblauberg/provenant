---
provenant_standing:
  schema_version: 1
  goal: ""
  grants: []
  obligations: []
  waiting_on_owner: []
---
# Session checkpoint

Open with the typed standing record above, from the
[continuity contract](../references/continuity.md). Every top-level field is
required: an empty `waiting_on_owner` is explicit, never an omitted one. Each
grant carries one `source` token and an `expires` instant (or `none`). The
chair's live grants — maintainer ownership, release, admin-merge, no local
builds and the CI cap — belong in this record with their source and expiry,
not in memory prose. A hook restores only sourced, unexpired grants.

`provenant workflow session save SESSION --file FILE --from-template`
scaffolds this shape: every section below is present, filled from the previous
revision where the new `--file` does not supply it (`-` reads stdin). Sections
from `--file` win. Frontmatter root keys carry forward unless the input supplies
the same key, so structured summary metadata survives edits that omit it. A
valid standing record carries forward unless the input supplies a valid one;
the empty record is scaffolded only when neither header has a valid record. The
first save has no previous revision and starts from the empty record above. A
save warns, without refusing, when the record or `## Intent` is missing or the
body passes 2 KiB after frontmatter.

## Goal
Outcome, done-criteria and the tracker or project document that owns them.

## Direction
User rulings, authority pointers, one-way doors and unresolved gates.

## Intent
Per live ruling, as the continuity contract describes.

## Process
Governing skills, retained evidence locations and verification responsibility.

## Plan
Next actions, blockers and resume trigger. Name the owner to reconcile first;
use PR #N for pull-request pointers.
