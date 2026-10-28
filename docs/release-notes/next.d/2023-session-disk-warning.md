## Behaviour changes

- Chair session start and checkpoint saves warn when free space falls below
  twice the dispatch floor, capped at half the volume's capacity, with commands
  to inspect disk use and run GC.
