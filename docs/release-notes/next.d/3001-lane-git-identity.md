## Fixes
### Git and safety

- Lane commits use the host's configured Git identity when the repository has no local identity, including when Git is invoked directly. A commit is refused if neither identity is configured (#3001).
