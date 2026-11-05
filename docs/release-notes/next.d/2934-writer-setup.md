## Fixes
### Lanes and settlement

- Writer cache preparation no longer holds up Git setup for other lanes in the same repository. A refused peer branch collision reports that the existing branch was kept. A local or peer submission that outlasts `--wait` reports an unconfirmed launch with its phase and operation ID (#2934).
