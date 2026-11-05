## Behaviour changes
### CLI and install

- GC defaults to collecting eligible `target` directories for Cargo projects without a project-specific GC entry. An explicit entry overrides the default (#2834).
