## Additions

- Graphs can use a configurable synthesise node to combine selected upstream outputs.
- Deterministic verdict and budget gates record attempt-less decisions, route failures to recovery nodes and preserve outcomes across checkpoint replay.
- Typed verdicts now require a unique final standalone verdict line. Malformed or repeated verdicts fail consuming gates, including when duplicates fall outside the captured result windows.
- Budget gates check settled usage at a point in time. CI-state and confined argv-command gates are deferred to [#2075](https://github.com/mblauberg/provenant/issues/2075).
