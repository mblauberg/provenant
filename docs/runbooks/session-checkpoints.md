# Session checkpoints

Owner: workflow
Anchor: tools/front-door/crates/workflow/src/session.rs

Save UTF-8 Markdown with `provenant workflow session save SESSION --file FILE`
and read it with `provenant workflow session show SESSION`. Each checkpoint is
limited to 256 KiB. Saving replaces the previous checkpoint for that project
and session; retain a separate copy when earlier content is needed. Session
hooks and hand-off prompts can include a shorter excerpt.
