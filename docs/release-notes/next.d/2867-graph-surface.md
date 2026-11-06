## Breaking changes
### Pipelines and graphs

- **`dispatch`, `review`, `lanes`, `resume`, `cancel` and `preset` are removed.** Use `run`, `run --use review`, `status`, `retry`, `stop` and `templates` respectively. Replace `run --preset NAME --prompt-file FILE` with `run --use NAME --file FILE`; port user presets to templates by hand (#2867).
- **`fabric graph` and MCP `fabric_graph` are removed.** Use `run`, `status`, `output --trace`, `edit`, `retry` and `stop`, or MCP `fabric_run` with a document. `PRESET_INCOMPATIBLE` is removed; use template admission diagnostics and upgrade every host together after draining live work. Unreadable checkpoints are preserved and reported through `retire_unreadable` (#2867).
- **Configuration discovery is compact.** Use `config schema --all` and the [advanced reference](../../reference/config-keys-all.md) for host policy and tuning outside the documented surface (#2867).
