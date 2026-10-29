# shadcn on Base UI

Checked: 30 October 2026 against [shadcn's Vite installation guide](https://ui.shadcn.com/docs/installation/vite), [Base UI default announcement](https://ui.shadcn.com/docs/changelog/2026-07-base-ui-default) and shadcn CLI 4.23.0. Confirm the installed versions and project conventions before applying these notes. A project may override this starter with its own component harvest process.

The adjacent starter is a small Vite, React and Tailwind application with Base UI. Its exact dependency versions are in `package.json`. Copy it into a new project, install dependencies, typecheck and build; do not overwrite an existing component system. The project owns the resulting component source and its updates.

For an existing project, follow the version-matched Vite guide and initialise shadcn with Base UI. `components.json` tells the CLI where to write components, utilities and CSS, and selects the Base UI style. Match its aliases in TypeScript and the bundler. Inspect generated files and dependencies before adopting them.

Base UI's `render` prop composes a primitive with another element while preserving its interaction behaviour. Follow the specific primitive's documentation when merging props, refs and handlers; test the resulting keyboard and focus behaviour. Do not assume Radix's `asChild` API applies.

Keep colour and surface values in semantic CSS tokens and map them to Tailwind theme variables. The starter's `cn()` combines conditional classes and resolves Tailwind conflicts. Extend tokens for the product's states and themes rather than scattering raw colours across controls.

Use native form semantics, labels, validation and error associations. The starter demonstrates a controlled field, native required validation and a status message. For complex forms, choose the project's form and schema libraries and preserve values through recoverable errors. Verify keyboard, focus and accessible names in the rendered app.
