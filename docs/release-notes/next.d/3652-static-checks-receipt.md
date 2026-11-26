## Fixes

### Lanes and recovery

- A declared no-build `verify.static_checks` tier is recorded on the launch receipt as well as named in the writer's preamble. The receipt carries the surviving checks with the layer and exact file and key that declared the list, including host and bound host-project ports files; an absent or empty declaration adds neither preamble text nor a receipt field. A check that fails is reported in the handoff without refusing the push, and CI still gates the merge. (#3652)
