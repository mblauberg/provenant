# UI evidence runtime

This private runtime contains the deterministic UI anti-pattern detector used
by `ui-ux-design`. It is not a separate skill, npm package, workspace, or
service. The skill remains the only user-facing owner and keeps
`skills/ui-ux-design/scripts/detect.mjs` as its stable command.

The runtime is resolved from an explicit product root or from the physical
source checkout. It does not use the target project's current directory,
`node_modules`, Fabric registration, seats, or state.

## Detector modes

Regex scanning works without optional packages; use `--fast` to force that
supported mode for every file. Static HTML/CSS scanning requires `htmlparser2`,
`css-select`, `css-tree`, and `domutils`. URL scanning requires `puppeteer`,
unless a caller supplies a browser or launcher. The runtime never installs
these optional engines in a target project.

The checked-in browser bundle is rebuilt only for detector maintenance:

```sh
node runtime/ui-evidence/build-browser-detector.mjs
node runtime/ui-evidence/build-browser-detector.mjs --check
```
