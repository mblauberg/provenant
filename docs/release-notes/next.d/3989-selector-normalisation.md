## Fixed

- Restrictive `lanes.pr` policies now classify forge API writes and PR commands consistently when repository or hostname selectors precede command names. The policy mediates ordinary `gh` use and is not a sandbox. [#3989](https://github.com/mblauberg/provenant/issues/3989)
