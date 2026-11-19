## Fixes

### Waits and CI observations

- `wait` ends with one line that states each target's settle state — settled ok,
  failed, needs input or still running at timeout — and the next command: resume
  the still-running targets, or re-read status once every target has settled.
  The observation-method note moves out of default human output into `--full`
  and the JSON envelope, so a chair can tell settled from timed out at a glance.
  (#3437)
