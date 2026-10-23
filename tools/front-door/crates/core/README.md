# provenant-core

Inert shared identity, root, path and Python compatibility policy. No command
selects these implementations yet. Rulebook: `PORTING.md` version 4.

## Python compatibility (`provenant_core::py`)

| API | Contract |
| --- | --- |
| `python_repr(&str)` | Python string repr, quote selection and escapes; Pinned Unicode 16.0 generated property table. |
| `python_str(&Value)` / `python_string` | Python str for JSON values, preserving integer digits and Python float notation. |
| `python_value_repr(&Value)` | Recursive repr, including quoted strings, True/False and None. |
| `python_dict_repr(entries)` | Render `(key, already_rendered_value)` entries in caller insertion order, including recursively rendered dictionaries. |
| `python_truth(&Value)` / `python_truthy` | Empty containers/strings, zero, false and null are false. |
| `python_strip(&str)` / `python_whitespace(char)` | Python whitespace, including U+001C through U+001F. |
| `python_json_eq(&Value, &Value)` | Recursive Python equality; bool/int/float compare exactly, including integers above 2^53. |
| `python_fromisoformat(&str)` / `python_fromisoformat_with` | Parsed `PythonDateTime`, retaining naive clocks and fractional UTC offsets. Basic/extended calendar and week dates, Unicode separators, comma fractions, and truncation to microseconds. |
| `python_iso_timestamp(&str)` | Aware-only adapter returning `Option<PythonTimestamp>`. |
| `from_timestamp(f64)` | The single float-second conversion: split integral/fractional seconds, round half-even to microseconds, then enforce Python years 1..9999. |

`py::parse_time` implements telemetry's naive-as-UTC parsing policy;
`PythonTimestamp::iso_utc` formats the shared UTC `Z` wire spelling;
`py::iso` adapts Jiff timestamps through exact integer microseconds.
`PythonTimestamp::as_microsecond()` is exact across Python's entire datetime
range. `to_jiff()` is a checked adapter: Jiff reserves the last twelve hours
of year 9999, so it cannot represent every Python timestamp. ISO grammar is
verified against CPython 3.14.4 and 3.14.8 (including `24:00` and fractions
requiring explicit seconds). `IsoformatSemantics::Python3144` is the default,
matching CI's pinned 3.14.4: a fractional offset with zero hours/minutes/seconds
is treated as UTC. `Python3148` retains that fractional offset, matching the
newer runtime. Both behaviors have exact differential coverage. Older CPython
releases differ on midnight/partial-clock grammar.

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
- Host byte inputs support UTF-8/16/32 BOM and null-byte detection;
  `slug_value` also accepts JSON-shaped non-string inputs. Extended JSON remains pending.
- JSONC unterminated-comment diagnostics match Python; generic JSON errors,
  exact integer ingress, extended values and input map order remain pending.
- Pointer writing preserves Unix surrogateescape and Python ASCII JSON bytes;
  pointer loading still awaits the extended JSON/path representation.
- `PointerWriter`: publication is supplied by supervisor at composition time;
  the std atomic-write backend is compiled only for tests.
- `pending::path_json` / `PathEncodingError`: private native-path encoder owned
  by records; main has no surrogateescaped path codec, and R2 forbids core from
  depending on records.

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
fixture requires CPython >=3.14, and the printable-table fixture requires
Unicode 16.0.0; these tests explicitly refuse incompatible oracles. Tests compare
CPython outputs exactly; heavyweight repository checks run only in CI.

## Shared helper adoption (core-3)

Consumer crates can replace their private `belongs in provenant-core` copies
with these public owners. No consumer is switched in this lane.

- `py::{printable, decimal_digit, python_digit, python_alpha, python_numeric,
  python_whitespace, python_word}` use one generated Unicode 16.0 property
  table. `scripts/generate_unicode.py` regenerates it with CPython's pinned
  Unicode data. Decimal and digit properties differ (`²` is a digit but is not
  decimal); word characters are Python alphanumeric characters plus `_`.
- `py::{python_splitlines, python_splitlines_with}` cover all Python line
  boundaries, pair CRLF and omit a trailing empty line. The latter retains
  terminators when requested.
- `py::python_number_str` is public alongside the existing `python_str`,
  `python_value_repr`, `python_repr`, `python_truthy` and `python_json_eq`.
- `py::numbers` owns `Integer`, `decimal_integer`/`IntegerError`, typed range errors, `python_float`,
  `float_whole`, `divmod_decimal` and `shell_split`, also re-exported by `py`.
  `Integer` retains decimal text for exact comparisons before checked machine
  integer conversion. Numeric whitespace excludes U+001C–U+001F.
- `paths::{resolve, realpath, path_text, path_sort_key, parent}` distinguish
  non-strict Python resolution from strict native realpath. `path_text`
  requires UTF-8 and returns a typed error otherwise; `path_sort_key`
  preserves native Python surrogateescape ordering as Unicode code points.
- `bytes::hex` (also `core::hex`) emits lowercase two-digit byte spelling.
- `js::{trim_ecmascript, js_whitespace, js_slice, js_number, js_iso, JsError}`
  own ECMAScript trim, UTF-16 prefix slicing at the UTF-8 SQLite boundary,
  `Number(string)` (NaN as `None`), and integral-millisecond Date formatting
  through the inclusive TimeClip bounds. Radix inputs larger than u64 retain
  ECMAScript rounding; this improves incorrect private copies to match source.

### ISO parsing contract

`python_fromisoformat` follows CPython 3.14.4 by default. The explicit
`python_fromisoformat_with` selects the zero-second fractional-offset behaviour
of 3.14.4 or 3.14.8. Naive civil values retain `None` for the UTC offset;
`python_iso_timestamp` accepts aware values only.

| Form | Accepted spelling / semantics |
| --- | --- |
| Calendar date | `YYYY-MM-DD` or `YYYYMMDD`; years 1–9999 |
| ISO week date | `YYYY-Www-D`, `YYYYWwwD`, or week alone (Monday) |
| Clock | Two-digit `HH`, `HH:MM`, `HHMM`, `HH:MM:SS`, `HHMMSS` |
| Separator | Any one Unicode character between date and clock |
| Fraction | Dot or comma after **seconds**, at least one digit; pad or truncate to six microsecond digits, without rounding |
| Offset | `Z`, `±HH`, `±HHMM`, `±HH:MM`, with optional seconds and a seconds fraction in the clock's matching basic/extended form |
| Offset bounds | Normalise minutes/seconds, then require magnitude below 24 hours; retain fractional microseconds |
| Midnight | `24`/`24:00` with all lower units zero becomes next day's midnight; year 10000 is refused |
| Refused | Reduced calendar precision, ordinal dates, fractional hours/minutes, leap seconds, missing fraction digits and invalid dates |

`py::datetime::form_tests` exercises the documented forms; the differential
matrix compares calendar/week dates, separators, partial clocks, fractions,
offsets and failures with CPython exactly.

### Partial-unit owner boundaries

The three assigned units remain partial: main's
`provenant_records::checked_json` cannot yet retain non-finite values, lone
surrogates or object insertion order, and does not expose Python JSON error
positions. R2 forbids core from importing records. `jsonc::parse_jsonc_with`
lets a composition layer supply the records decoder and keep its typed errors;
the existing finite-value convenience reader still has the recorded ingress
gaps. No new record parser or checked-codec copy is introduced here.

`product_root_resolver::PointerWriter` lets the composition layer use
supervisor's safe filesystem API; production core has no atomic-write copy.
The old std backend exists only in tests. The path-wire encoder remains a
private records-owned pending item until records exports that codec.

On the supported macOS APFS filesystem, invalid UTF-8 directory creation is
refused with EPERM. The test asserts that result and compares Python's errno;
it does not skip. On Linux, creation and pointer byte publication must succeed.
The pure path-wire differential runs on both platforms for every byte.
