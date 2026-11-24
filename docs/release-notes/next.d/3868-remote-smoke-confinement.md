## Fixes

### Fabric

- A peer-placed graph node whose project cannot be projected across hosts (for example the `upgrade --verify` smoke fixture in a scratch directory outside the home) no longer fails with an unattributable `confinement_refused`. The refusal now names the confinement rule and the path that could not be projected, both in the node result and in the human error. A peer binding its own read-only source checkout also names the rule and path if that binding fails. (#3868)
