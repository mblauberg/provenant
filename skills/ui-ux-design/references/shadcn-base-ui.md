# shadcn on Base UI

Confirm the installed versions and project conventions before applying these
notes; a project may override the starter with its own component harvest process.
Reference: [shadcn Vite installation](https://ui.shadcn.com/docs/installation/vite).

The adjacent starter is a small Vite, React and Tailwind application with Base
UI; exact versions are in its `package.json`. Copy it into a new project, install,
typecheck and build. Do not overwrite an existing component system: the project
owns the resulting component source and its updates.

For an existing project, follow the version-matched Vite guide and initialise
shadcn with Base UI. `components.json` tells the CLI where to write components,
utilities and CSS and selects the Base UI style; match its aliases in TypeScript
and the bundler, and inspect generated files and dependencies before adopting them.

- Base UI composes through the `render` prop, not Radix's `asChild`. Follow the
  specific primitive's documentation for merging props, refs and handlers, and
  test the resulting keyboard and focus behaviour.
- Keep colour and surface values in semantic CSS tokens mapped to Tailwind theme
  variables; extend tokens for product states and themes rather than scattering raw
  colours. The starter's `cn()` merges conditional classes and resolves Tailwind
  conflicts.
- Use native form semantics, labels, validation and error associations. The
  starter shows a controlled field, native required validation and a status
  message; for complex forms use the project's form and schema libraries and
  preserve values through recoverable errors. Verify keyboard, focus and accessible
  names in the rendered app.
