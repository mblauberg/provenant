## Additions
### Pipelines and graphs

- Give graph scouts additional read roots and pin review or writer sources to an explicit commit, a dependency's commit or the combined integration head. Source commits survive replay, and existing clean writer contexts advance only by a safe fast-forward (#2193).
