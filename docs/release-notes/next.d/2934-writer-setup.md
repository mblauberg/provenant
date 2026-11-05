## Fixes
### Lanes and settlement

- Writer cache preparation no longer holds up Git setup for other lanes in the same repository. A refused peer branch collision reports that the existing branch was kept. A submission that outlasts its preparation window reports an unconfirmed launch with its phase and operation ID; local preparation has at least five seconds and a larger window for multi-task submissions (#2934).
