## Fixes

- A legacy slot-root writer reservation now fences only the attempt it names for recovery evidence, recovery acquisition and writer admission, so a successor with no reservation of its own settles as provably absent instead of carrying the previous attempt's fence. A record that cannot be read, decoded or validated, including one with a wrong schema version or anchor, still counts as retained and keeps every attempt fenced (#2433).
