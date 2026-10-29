# Scout areas

Use when a scoping stage splits codebase reconnaissance across scouts.

Give each scout one area: a directory, a crate, a subsystem or a question
("where is X decided?"). Areas do not overlap, together cover the change, and
name their read roots. A scout reads, never edits.

Each scout returns, in under 300 words:
- Files and symbols that matter, as `path:line` with a one-line reason.
- Existing helpers and patterns the change should reuse.
- Constraints found in code, tests or docs that the spec must respect.
- What the scout could not determine, stated plainly.

Stop at the area boundary. Report an adjacent finding as a pointer for another
scout rather than following it.
