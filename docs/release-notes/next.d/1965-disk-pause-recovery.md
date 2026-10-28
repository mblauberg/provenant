## Fixes

Running builds resume when the disk-floor configuration becomes unreadable.
Recovery attempts every saved build identity even if one resume fails, and
corrupt pause journals no longer block observation or settlement. Admission
uses the same configuration root as the guardian. Legacy disk-guard retirement
reports corrupt pause state and still removes the retired agent and state.
