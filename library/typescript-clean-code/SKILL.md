---
name: typescript-clean-code
description: "Use for TypeScript compiler and type-safety work: tsconfig, TS 6/7 upgrades, unsafe casts, floating promises, type tests. Not for restyling working code; pair with implement, code-review or refactor."
---

# TypeScript

Check the installed compiler first. A repository may pin TypeScript 6.x as
`typescript` and install 7.x under an alias such as `typescript-native`.

Use discriminated unions, `unknown`, `satisfies`, `never` exhaustiveness
and boundary schemas.

## TypeScript 6 and 7

- **6.0 defaults:**
  - `strict` is on;
  - `module` is `esnext` and `target` is `es2025`;
  - `rootDir` is the tsconfig's own directory;
  - `noUncheckedSideEffectImports` is on;
  - `types` is `[]`, so global type packages such as `@types/node` must be
    listed. A new "cannot find name `process`" after upgrading is usually this.
- **Retired options:** 6.0 deprecates or removes `baseUrl`, `moduleResolution`
  `node10` and `classic`, `target` `es5`, `downlevelIteration`, AMD, UMD and
  System modules, `outFile`, `esModuleInterop: false` and import `assert` (use
  `with`). `ignoreDeprecations: "6.0"` only buys time for the deprecated ones,
  and 7.0 turns all of them into hard errors. Fix the config instead.
- **7.0 is the Go-native compiler.** It is much faster but has no programmatic
  API until 7.1. typescript-eslint, ts-morph and some bundler plugins still load
  TypeScript 6. Keep 6.x installed through npm aliases
  (`@typescript/typescript6` re-exports the 6.0 API and ships `tsc6`) rather
  than deleting it to "finish" a migration.
- **Flags outside `strict` that earn their keep:** `noUncheckedIndexedAccess`,
  `exactOptionalPropertyTypes`, `noImplicitOverride` and `verbatimModuleSyntax`.
  With `exactOptionalPropertyTypes`, `x?: T` means "may be absent", not "may
  hold `undefined`", so expect churn. Turn each one on as its own migration.
- **Erasable syntax:** `erasableSyntaxOnly` rejects enums, runtime namespaces,
  constructor parameter properties and `import =`. Node 22.18 or later cannot
  run these when it executes `.ts` files by stripping types. Prefer string
  literal unions or `as const` objects to enums.

## Defects worth hunting

- **Casting outside data:** `as T` or `as unknown as T` on JSON, storage,
  messages or environment values claims a shape no one checked. Parse it with
  the project's schema library. `as const` and `satisfies` are fine.
- **Voided promises:** prefixing a promise with `void` silences
  `no-floating-promises`, which allows voiding by default, but a rejection
  still goes nowhere. Each promise needs a handler: awaited, returned, or given
  `.catch(report)`. The rule only runs under type-aware linting
  (`parserOptions.projectService`), which many ESLint configs lack.
- **Unbounded `Promise.all`:** over a large or user-sized collection, it has no
  limit. Use a pool.
- **Defensive nulls:** adding `| null | undefined` to a return type "just in
  case" forces a check on every caller.
- **Caught errors:** a caught error is `unknown`. Narrow it before reading
  `.message`, and pass it as `cause` when wrapping.

## Type tests

- **`expectTypeOf` and `assertType`** are only checked in Vitest's typecheck
  mode (`vitest --typecheck` or the `typecheck` config). A normal run never
  checks them.
- **`@ts-expect-error`** marks code that must fail to compile. It needs a
  stated reason (`@typescript-eslint/ban-ts-comment` can enforce one). Never
  use `@ts-ignore`.

## Leave alone

Do not churn working code over taste: `type` against `interface`, `null`
against `undefined`, or import style. Follow the repository.

## Sources

Checked 22 October 2026: devblogs.microsoft.com/typescript (5.8, 5.9, 6.0 and
7.0 announcements), typescriptlang.org release notes,
typescript-eslint.io/rules/no-floating-promises, vitest.dev/api/expect-typeof,
and nodejs.org type-stripping docs.
