## Behaviour changes
### Lanes and settlement

- PR and CI observation use conditional requests and configurable fast/slow intervals; recent check-run durations determine when observation accelerates. Conditional PR wait rows report unknown review decisions; one-shot `pr status` retains its detailed review observation. (#3078)
