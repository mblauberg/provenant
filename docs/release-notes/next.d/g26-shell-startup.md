## Behaviour changes
### Git and safety

- Lanes do not read login shell startup files, such as `~/.zshenv` and `/etc/zprofile`. They keep the `PATH` of the environment that launched them, so their Git wrapper stays first.
