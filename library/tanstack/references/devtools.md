# TanStack Devtools

Dev-only by default. Checked against the `devtools-production` and
`devtools-app-setup` skills shipped in `@tanstack/devtools`; load them with
Intent for detail.

- Install `@tanstack/react-devtools` and `@tanstack/devtools-vite` as dev
  dependencies. The Vite plugin strips the imports and JSX on `vite build`
  (`removeDevtoolsOnBuild` defaults to true), so no runtime guard is needed.
- Put `devtools()` first in the Vite `plugins` array, before `react()`.
- Without the Vite plugin, guard the import yourself, and confirm with a
  production build that no devtools code remains.
- Shipping devtools to production is a deliberate choice: regular
  dependencies, and the production skill's steps.
- Keep all `@tanstack/devtools-*` packages and plugin panels (Query, Router,
  Form and others) at matching versions. Mixed versions fail silently.
- Do not mount `TanStackDevtools` in server rendering without a client guard
  when the framework ignores `'use client'`.
