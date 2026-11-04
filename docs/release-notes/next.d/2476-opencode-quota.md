## Fixes
### Accounts, quota and routing

- Allow OpenCode read-only profiles to execute system grep ([#2471](https://github.com/mblauberg/provenant/issues/2471)).
- Stop stalled OpenCode processes after a quota refusal, and cool down free routes so pools choose other junior members ([#2475](https://github.com/mblauberg/provenant/issues/2475)).
- Mark partial quota usage as a lower bound in compact output and avoid pacing against uncalibrated partial windows ([#2476](https://github.com/mblauberg/provenant/issues/2476)).
