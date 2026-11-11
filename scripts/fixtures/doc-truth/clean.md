# Clean references
Owner: documentation maintainers
Anchor: `tools/front-door/src/main.rs`

Read `AGENTS.md`, `docs/adr/0001-example.md`, `../adr/0001-example.md`
and `docs/adr/*.md`. Inspect `ADR 0001` and `docs/adr/0001-example.md#decision`.
Run `provenant route list --pool example --json`, or `provenant gc --apply`.
Use `provenant --json route list` or `provenant --state-dir /tmp/example route list`.
Read `provenant help route list` for `--pool`.
Help accepts `provenant --json help route list` and
`provenant --state-dir /tmp/example help route list`.
Alternatives `provenant config set|unset|show --sources` resolve each verb.
The external command `git push --force-with-lease` has its own flags.
Descriptions `provenant route ...` and `provenant <verb>` are templates.
Templates `docs/TASK/example.md`, `$ROOT/no.md`, `/runtime/no.md`,
`https://example.com/no.md` and protocol `tools/list` are not repository paths.

```sh
# Examples are never executed; fenced lines that do not start with provenant are not checked.
echo `provenant missing`
provenant route list --pool example
provenant gc --apply  # comments are ignored --vanished
```
