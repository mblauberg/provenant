## Fixes
### Lanes and settlement

- OpenCode lanes on free (Zen) models no longer fail `identity_unobserved` after the model answers. The answering provider and model are now read from either of OpenCode's session tables, and a lane is still refused if the session names a different provider than the one requested, so Go lanes stay strict. (#3152)
