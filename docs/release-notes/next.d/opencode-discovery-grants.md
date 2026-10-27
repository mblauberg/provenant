## OpenCode discovery directories

OpenCode lanes now launch when the project or an ancestor contains `.agents`,
`.claude` or `.codex` directories. Discovery stays literal and does not grant
reads of configuration contents outside existing read roots. Symlinks and
replaced held parents remain refused. Discovery anchor refusals now name the
path and validation stage.
