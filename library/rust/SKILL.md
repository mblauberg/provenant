---
name: rust
description: "Use for Rust code, Cargo configuration and edition migrations. Not for behaviour-preserving restructuring alone (refactor) or root-cause investigation (diagnose)."
---

# Rust

Check the toolchain, edition, lockfile and lint policy before choosing an API. Use types to encode invariants and return errors for expected failure.

Edition migrations can change temporary drop order and opaque return lifetime capture without changing apparent control flow. Inspect guards and borrows around `if let`, tail expressions and `impl Trait`; test affected behaviour. Cargo workspace lint inheritance cannot be combined with package-specific lint tables. Put local lint attributes in the crate when inheritance is enabled.

A release profile may abort on panic while tests unwind. A passing `catch_unwind` test does not establish release recovery. Check the actual profile and exercise the deployed process when recovery matters. Async traits and framework handlers depend on installed crate versions; check `Send` and ownership against the locked API.

Read the [dated release notes](references/releases-2026.md) for version-sensitive details.
