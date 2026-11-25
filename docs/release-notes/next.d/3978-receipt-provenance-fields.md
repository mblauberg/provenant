## Fixes

### CLI and install

- `output --part receipt --json` now reports sourced provenance for every
  adapter: the family comes from the routing catalogue (including OpenCode's
  Hy4 models, alongside the existing DeepSeek, Z.ai and OpenAI families), the
  effort carries its applied value and source when a drop or default was
  recorded, and `resolved_from` names the launch decision for dispatched
  named-route lanes. The `role` field is pinned for both reviewer and writer
  lanes. (#3978)
