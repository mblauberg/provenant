## Fixes

### Lanes and recovery

- A project's declared no-build `verify.static_checks` tier is now recorded on the launch receipt as well as named in the writer's preamble. The receipt carries the surviving checks with the layer and key that declared the list; an absent or empty declaration adds neither preamble text nor a receipt field. A check that fails is reported in the handoff without refusing the push, and CI still gates the merge. (#3652)
