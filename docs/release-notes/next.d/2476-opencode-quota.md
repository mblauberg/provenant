## Fixes
### Providers and quota

- Allow OpenCode read-only profiles to execute system grep ([#2471](https://github.com/mblauberg/provenant/issues/2471)).
- Cool down OpenCode free routes when usage-limited so pools choose other junior members ([#2475](https://github.com/mblauberg/provenant/issues/2475)).
- Mark partial quota usage as a lower bound in compact output and avoid pacing against uncalibrated partial windows ([#2476](https://github.com/mblauberg/provenant/issues/2476)).
