## Fixes
### CLI and install

- `provenant run --model` accepts a bare model name, such as `claude-sonnet-5-5`, when exactly one catalogued provider offers it. A name that several providers offer is refused with the `provider/model` forms to choose from, and an unknown name is refused with the form to use. The `provider/model` form is unchanged.
