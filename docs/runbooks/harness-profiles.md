# Configuration presets

The Rust rewrite replaces the profile command and knob registry with one
optional `<state>/config.toml`. See the accepted
[state and config design](../specs/rust-port/design/state.md#configuration).
The default state directory is `~/.local/share/provenant/v1`.

```toml
schema_version = 1
profile = "economy"
```

Omit the file or `profile` to select `balanced`. `economy` requests a concurrency
cap of four, a context cap of 200,000 tokens and weighted pool rotation.
`rigorous` requests one higher effort except for mechanical and bulk tasks,
one additional review, cross-family review and full re-review. Domain owners
validate these settings and enforce their policy minima.

`provenant-config` provides read-only show/check library entry points; the CLI
build will wire `config show` and `check config`. Domain sections and their
TOML assets arrive with their owners. Unknown keys fail closed.

Profile mutation, project overrides, per-request selectors and knob environment
variables are retired. The shared Python profile helpers remain temporarily
for consumers whose Rust rewrite has not landed; they are removed with their
last importer in state slice 5. No Python-to-Rust adapter is provided.
