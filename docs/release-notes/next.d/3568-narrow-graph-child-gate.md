## Fixes

### Lanes and settlement

Settlement cleanup no longer refuses to end a lane's own processes because an
unrelated graph child is still running. A live graph child is a separate
attempt: it is never a recorded descendant of the lane's provider and never
carries the lane's sandbox mark, so it was never among the processes the lane
could signal. The old all-or-nothing gate held background loops and services
past settlement and kept the guardian in retained custody until that child
finished. Now a graph child spares only its own tree, which its own guardian
ends; the lane's recorded descendants and sandbox-marked escapees receive TERM
and then KILL after the descendant grace as usual, so cleanup resolves and the
writer fence is released on time. (#3568)
