## Fixes

### CLI and install

- `output --part receipt --json` now reports sourced provenance for every
  adapter: the family comes from the effective routing catalogue, including
  discovery and overlays. OpenCode Hy4 models are included alongside the
  existing DeepSeek, Z.ai and OpenAI families. The
  effort carries its applied value and source when a drop or default was
  recorded; a request alone leaves effort and its source `unknown`.
  `resolved_from` names the launch decision for dispatched
  named-route lanes. The `role` field is pinned for both reviewer and writer
  lanes. (#3978)
