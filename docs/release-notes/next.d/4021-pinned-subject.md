## Behaviour changes

### Pipelines and graphs

- One `PinnedSubject` (repository, base, head, origin, policy and observation time) is resolved at admission and carried on the operation, graph document and checkpoint. A graph document with a subject uses schema v2; peer readers receive the same subject and head. Launch, review, wait and settlement consume those OIDs instead of re-resolving mutable refs, so a push between admission and capture cannot silently change an operation's subject; a moved head is reported as `head_check: moved{pinned, current}` and re-resolved only under an explicit `follow_latest` policy. Status shows the pinned subject beside its origin for every operation, and graphs admitted before this change load without one. (#4021)
