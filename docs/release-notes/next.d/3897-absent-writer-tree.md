## Fixes

### Lanes and settlement

- `provenant reconcile --abandon` can settle a lane whose writer checkout has been removed. The census now treats a removed root as decidable: a process counts as inside it only when its path lies under the removed path, so an unreadable system process elsewhere no longer blocks abandonment, while a process left in the deleted tree still does. Closes #3897.
