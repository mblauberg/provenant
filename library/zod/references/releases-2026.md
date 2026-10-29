# Release notes

Checked: 30 October 2026. The registry reported Zod 4.6.7. Check the installed version against the [Zod 4 migration guide](https://zod.dev/v4/changelog) and [release notes](https://zod.dev/v4).

In Zod 4, a default short-circuits parsing on undefined and uses the output type; `prefault` instead supplies an input to parse. Defaults inside optional object fields can be applied to absent keys. Enum-keyed records are exhaustive; `partialRecord` permits missing keys. UUID validation is stricter, schema-level errors take precedence over per-parse errors, and a type predicate in `refine` does not narrow the inferred type.
