## Features

### Pipelines and graphs

- Review lanes now ship the host-pinned base as the named ref `refs/provenant/review/base` in their detached view and state in the preamble that the host-supplied base and head are authoritative. A review no longer goes `inconclusive` because its detached checkout lacks `origin/<default>` and the lane cannot fetch to confirm the merge base. (#3977)
