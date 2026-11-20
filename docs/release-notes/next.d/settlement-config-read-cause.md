## Fixes

### Git and safety

- Settlement's Git configuration reads now name the operation, file and key or include that failed with a bounded underlying diagnostic, naming keys instead of values so a credential in a config file cannot leak through a refusal; a transient Git config-lock collision from a sibling writer is waited out within the existing operation deadline with the same bounded retry as Git mutations, so concurrent settlement no longer fails with an opaque "repository Git configuration could not be read"
