## Behaviour changes

- Session start and checkpoint saves warn when free space falls below twice the
  dispatch floor, with commands to inspect disk use and run GC.
