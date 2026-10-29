---
name: zod
description: "Use for Zod schemas, parsing, validation and migrations at TypeScript data boundaries. Not for TypeScript compiler configuration (typescript-clean-code) or test selection (test-design)."
---

# Zod

Parse untrusted input at the boundary and use the parsed output. Check the installed major and owning wire contract before changing a schema. A schema shared with another service must preserve that service's contract; do not extend legacy routes owned elsewhere without authorisation.

Defaults, transforms, records and error precedence can change accepted data or output without a compiler error. Check the dated migration notes and test representative missing, invalid and transformed values at the public boundary. A type predicate in a refinement is not necessarily reflected in the inferred type; check the actual output type. Choose strictness and transformations to match the wire contract.

Read the [dated release notes](references/releases-2026.md) for version-sensitive details.
