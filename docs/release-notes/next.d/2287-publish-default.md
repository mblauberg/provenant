## Behaviour changes
### Lanes and settlement

- `lanes.publish` is now opt-in (default `false`). A networked writer lane no longer receives `~/.config/gh`, the git credential helper or the real `gh` executable unless the operator sets `lanes.publish=true`, which gives lanes the GitHub credential. Host-side delivery (`delivery = "pr"`) is unchanged.
