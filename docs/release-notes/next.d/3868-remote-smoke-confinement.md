## Fixes

### Fabric

- A peer-placed graph node whose project cannot be projected across hosts (for example the `upgrade --verify` smoke fixture in a scratch directory outside the home) no longer fails with an unattributable `confinement_refused`. The refusal now names the confinement rule and the path that could not be projected, in the node result. A peer binding its own read-only source checkout names the rule and path when the checkout cannot be anchored or fingerprinted; a checkout that cannot be resolved keeps its own typed cause. (#3868)
