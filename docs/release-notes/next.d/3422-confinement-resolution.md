## Fixes

### Git and safety

- Keep nested credential declarations protected beneath selected runtime and
  authentication grants. Admission and sandbox rendering now share one resolved
  policy, and unavailable enforcement cannot silently widen a selected sandbox
  route. Ordinary hard links remain available outside protected roots. (#3422)
