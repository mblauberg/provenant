## Behaviour changes
### Lanes and settlement

- Writer lanes now have a private Git directory, and shared refs and reflogs are read-only to them. The host verifies lane objects before they reach the shared repository, and launch bases stay pinned until publication (#2130, #2131, #2138, #2139, #2158).
- Settlement resumes after a crash, and `cancel` and `reconcile` stay responsive while Git runs. Lane waits stay in settlement until custody release is durable, so a terminal result no longer needs a second `reconcile`, and a terminal import refusal keeps the private Git for host recovery (#2141, #2151, #2154, #2157).
