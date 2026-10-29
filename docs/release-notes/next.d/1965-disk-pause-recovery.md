## Fixes

- Paused builds resume when the disk-floor configuration becomes unreadable, and a corrupt pause journal no longer blocks observation or settlement. Retiring the legacy disk guard reports corrupt pause state and still removes the retired agent and state.
