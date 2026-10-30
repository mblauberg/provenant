## Fixes

- Starting and queued lanes now surface a failed host resource probe instead of hiding it behind a generic capacity or launch reason. The reason names the failed memory pressure or process budget probe and a bounded error, and says the failure is not holding the lane because admission continues when a probe fails. A full process budget is still reported by count even when the memory pressure probe fails ([#2425](https://github.com/mblauberg/provenant/issues/2425), follow-up to [#2401](https://github.com/mblauberg/provenant/issues/2401)).
