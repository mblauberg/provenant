## Changes
### CLI

- `provenant run` prints the full `GRAPH/NODE/ATTEMPT` lane ID of each node it launched, one `lane:` line each, and `--json` carries them as `lane_ids` (#3186).
- The default `status` listing shows every active and needs-attention lane however many there are, and states how many settled lanes it left out (#3186).
- A launch or `retry` refused before admission prints the recorded route and reason instead of the fixed sentence (#3186).
