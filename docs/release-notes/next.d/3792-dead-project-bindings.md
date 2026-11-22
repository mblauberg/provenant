## Fixes
### Git and safety

- `provenant upgrade --verify` removes its scratch project's binding, and launch confinement, tick settlement and forge polling skip a registered project whose common directory is gone instead of refusing unrelated launches. A project with an unreadable layout now protects its root and warns, and `provenant gc` retires dead bindings after the idle lifetime. (#3792)
