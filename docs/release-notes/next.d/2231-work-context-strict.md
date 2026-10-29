## Behaviour changes
### Lanes and settlement

- A work `context` with a field outside the nine fixed ones is now refused when a run is submitted, instead of being carried silently (#2231). Stored records still read unknown fields.
