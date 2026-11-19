## Fixes

### Git and safety

- Git-driven tests in lanes can import into their own fixture repositories without the lane's private Git binding redirecting the operation. The lane Git wrapper also scrubs host global configuration for commands that act outside the lane checkout, so a host identity or credential helper never reaches another repository. (#3550)
