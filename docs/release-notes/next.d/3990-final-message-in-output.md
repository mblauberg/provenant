## Fixes
### CLI and install
- `provenant output TARGET --final --to FILE` exports the complete sealed lane result, including long final messages and handoffs, even when they span output pages. The default view remains bounded. (#3990)
