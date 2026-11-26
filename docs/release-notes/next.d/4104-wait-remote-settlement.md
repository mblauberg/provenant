## Fixes
### CLI and waits
- `provenant wait GRAPH` now re-drives the journal-locked graph advance when a successful remote reading reports a terminal attempt against an unsettled checkpoint. A graph whose peer attempt has finished settles within the wait's normal reread cadence instead of timing out until a manual `provenant tick`. (#4104)
- A tick runs graph advancement as its own pass before forge polling, so slow forge pages cannot place graph settlement behind a long poll. (#4104)
