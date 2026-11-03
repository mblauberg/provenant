## Fixes
### Git and safety

A writer's own commit to a protected path after merging upstream settles `review_required`, and upstream-only protected changes stay `succeeded`. The settlement gate was already correct; the macOS writer fixture that checks it committed outside the lane's private Git view and now runs in macOS CI (#2803).
