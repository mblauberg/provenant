## Features

### Session checkpoints

`workflow session save` now validates the checkpoint against the `session`
skill's template and warns, without refusing, on each gap it finds: missing
`provenant_standing` frontmatter, missing `## Intent`, or a body over 2 KiB
measured after frontmatter. The 2 KiB template budget is advisory and separate
from the configured soft budget floored at 8 KiB, so a short checkpoint stays
quiet. `workflow session show` prints the whole checkpoint by default instead
of the bounded compact view; `--json` is unchanged and `--full` remains
explicit. `workflow session save --from-template` scaffolds every template
section, prefilled from the previous revision and overridden per section by
`--file` (`-` reads stdin), including custom sections and preamble text,
carrying the typed standing record forward; the
first save starts from an empty record. The chair's standing grants belong in
that typed record with their source and expiry, so hooks restore only sourced,
unexpired grants. (#4029)
