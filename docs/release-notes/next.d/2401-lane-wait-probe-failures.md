## Fixes

- A starting or queued lane whose host resource probe fails now says so: the wait reason names the failed memory pressure or process budget probe and a bounded error, and names both when both probes fail. Before, a failed probe fell through to a generic capacity or launch reason, so operators could not tell a probe failure from a real wait ([#2401](https://github.com/mblauberg/provenant/issues/2401)).
