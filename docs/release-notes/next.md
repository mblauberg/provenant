# Next release

## Upgrade

Let running lanes settle before you reinstall. A guardian now inherits a
sixth descriptor at hand-off, its stop channel, so a host and guardian from
either side of this release refuse each other's hand-off. A host process
still running the old binary, such as a parent lane's guardian, cannot launch
an attempt through the new binary. That launch ends unconfirmed and needs
`provenant reconcile`. Attempts already in custody keep running and can still
be cancelled.

## Behaviour changes

- A lane's child now reads its stop only from a host-owned control directory
  under the host state root, `control/<child store>/<AttemptId>/`. The lane
  cannot write there, so it can no longer pre-empt or undo the host's stop of
  its child. A `cancel.json` in the child's attempt directory no longer ends
  admission or labels the child cancelled. A lane can still cancel its own
  child: the parent's host relays the request (#1581).
