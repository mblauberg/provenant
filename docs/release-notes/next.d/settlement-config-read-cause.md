## Fixes

### Git and safety

- Settlement's Git configuration reads now name the operation, file and key or include that failed with a bounded underlying diagnostic, naming static key classes instead of keys or values so a credential in a config file cannot leak through a refusal; a transient Git config-lock collision from a sibling writer is waited out within one deadline shared by all of the operation's attempts, with the same bounded retry as Git mutations and the hardened environment and post-read checks still applied to the retried output, so concurrent settlement no longer fails with an opaque "repository Git configuration could not be read"
