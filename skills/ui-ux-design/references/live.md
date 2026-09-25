<!-- Modified for Provenant. -->

# Live iteration

Live is a write-capable sub-mode of an active implementation lifecycle. Before
setup, that lifecycle must bound the project root, allowed source paths, config
changes, and output/cache locations. A read-only request never enters live
setup; use read-only rendered evidence or an authorised isolated prototype.

Enter with `node "$(provenant root)/skills/ui-ux-design/scripts/live.mjs"`.
Its `--help`, each `live-*.mjs --help`, and the JSON event contract are the
operational procedure for config, polling, recovery, carbonisation, and stop.
Keep focus keys as protocol data, not a user interaction model. Stop on
ambiguity, server loss, stale session identity, malformed events, or an expanded
write set. Use protocol accept/discard/completion paths, not ad hoc edits.
Validate each project-relative source target with descriptor-bound, no-follow
containment before any write.

On `config_missing`, create only the returned path using the schema in
`live.mjs --help`; never invent keys. Keep secrets out of logs and screenshots.
Preview selection is not lifecycle acceptance.

## Handle fallback

On `element_not_in_source`, `element_not_found`, `file_is_generated`, or
`element_ambiguous`, the wrapper returns `fallback: "agent-driven"` without a
source write. Read candidate ranges and rendered context; retry with a more
specific element id/classes/tag/text or explicit source `--file`. For runtime-
generated elements, persist the result in its canonical source owner. Manually
place the exact session wrapper only inside the bounded source path; if identity
or ownership remains ambiguous, stop and report it.

## Required after accept

Complete the carbonisation task: replace run-owned variant scaffolding with
project-native source, verify the preview, and record `live-complete.mjs` before
polling again. Reporting a carbonisation error explicitly abandons the task;
the accepted change stays saved.

## Exit

Stop only this run's exact background handle, or a run-owned PID matched by
command and start identity; never kill by broad name or pattern. Verify source/config cleanup against the baseline and keep the recovery
journal. Remove only run-owned transient output after source and server state
are verified. If ownership is uncertain, report the residual path and stop.
