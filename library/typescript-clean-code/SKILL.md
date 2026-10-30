---
name: typescript-clean-code
description: "Use for TypeScript compiler configuration, type safety, unsafe casts and TS upgrades. Not for TanStack cache behaviour (tanstack) or restructuring alone (refactor)."
---

# TypeScript

Read the compiler version, configuration and dependency peer ranges before migration. Consult the [dated compiler notes](references/ts-6-7.md).

Parse JSON, environment, storage and network values at the boundary instead of asserting their shape. Use discriminated unions and exhaustive handling where they remove invalid states. Avoid speculative nulls that force every caller to compensate.

A voided promise can still reject: await, return or attach an error handler. Type-aware lint must be enabled for floating-promise checks. A normal test run may not check type assertions; run the project's typecheck mode.

Change one compiler flag at a time and run the repository-prescribed targeted typecheck after each change, using `tsc --build` where project references require it. Record diagnostics and tooling incompatibilities rather than suppressing them. Leave working naming and import conventions alone.
