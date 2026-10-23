# provenant-core

Inert shared identity, root, path and Python compatibility policy. No command
selects these implementations yet. Rulebook: `PORTING.md` version 4.

## Python compatibility (`provenant_core::py`)

| API | Contract |
| --- | --- |
| `python_repr(&str)` | Python string repr, quote selection and escapes; Unicode 16.0 printable table copied from the verified quota/CLI helpers. |
| `python_str(&Value)` / `python_string` | Python str for JSON values, preserving integer digits and Python float notation. |
| `python_value_repr(&Value)` | Recursive repr, including quoted strings, True/False and None. |
| `python_dict_repr(entries)` | Render `(key, already_rendered_value)` entries in caller insertion order, including recursively rendered dictionaries. |
| `python_truth(&Value)` / `python_truthy` | Empty containers/strings, zero, false and null are false. |
| `python_strip(&str)` / `python_whitespace(char)` | Python whitespace, including U+001C through U+001F. |
| `python_json_eq(&Value, &Value)` | Recursive Python equality; bool/int/float compare exactly, including integers above 2^53. |
| `python_fromisoformat(&str)` | Parsed `PythonDateTime`, retaining naive clocks and fractional UTC offsets. Basic/extended calendar and week dates, Unicode separators, comma fractions, and truncation to microseconds. |
| `python_iso_timestamp(&str)` | Aware-only adapter returning `Option<PythonTimestamp>`. |
| `from_timestamp(f64)` | The single float-second conversion: split integral/fractional seconds, round half-even to microseconds, then enforce Python years 1..9999. |

`PythonTimestamp::as_microsecond()` is exact across Python's entire datetime
range. `to_jiff()` is a checked adapter: Jiff reserves the last twelve hours
of year 9999, so it cannot represent every Python timestamp. ISO grammar is
verified against CPython 3.14.8 (including `24:00` and fractions requiring explicit
seconds); older CPython releases differ on these edge cases.

A `serde_json::Value` object has already lost input insertion order. Its rendering
uses its current iteration order; callers observing Python dict order must keep
that order at decode time and use `python_dict_repr`, recursively for nested
objects. The helpers do not parse JSON, represent lone surrogate strings, or
change serde_json features. The checked integer codec remains owned by
`provenant_records::checked_json`; core cannot depend on records (R2).

The foundation gathers quota's verified str/repr/Unicode and float-second
helpers, plus PR #1298's exact truth/equality contracts. ISO parsing also covers
the Unicode separator/week-date gaps in that PR. Other crates keep their pending
copies until their owners migrate them.

## Port status

- `roots`: product/skills-root policy and the `scripts/lib/roots.py` re-export
  contract, with explicit environment/home/install observations.
- `host_identity`, `jsonc`, `product_root_resolver`: partial ports; see the
  retained RP-1001/RP-1002/RP-1003 gaps in `divergences.md`. No cut-over claim.
- `pending::atomic_write`: minimal private safe publication operation owned by
  `provenant-supervisor`; no matching publication API exists on main yet.

## Verification

Run from `tools/front-door`:

```sh
cargo fmt --all
cargo clippy -p provenant-core --all-targets -- -D warnings
PROVENANT_PORT_DIFF_REQUIRED=1 cargo test -p provenant-core
```

One oracle runner selects `PROVENANT_PORT_PYTHON` (default `python3`, minimum
3.12), uses `-I` and an explicit environment, and bounds execution to 30 seconds.
Absent/old interpreters emit `skip:` and fail required mode. The ISO grammar
fixture requires CPython >=3.14.8, and the printable-table fixture requires
Unicode 16.0.0; these tests explicitly refuse incompatible oracles. Tests compare
CPython outputs exactly; heavyweight repository checks run only in CI.
