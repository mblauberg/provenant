## Fixes
### CLI and install

- `provenant check install` now verifies the roots recorded in the install receipt rather than targets derived from the caller's `HOME`, so a healthy install reports no findings when the check runs from a lane or any account-directory `HOME`. (#3811)
