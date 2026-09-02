#!/usr/bin/env python3
"""Shared prose-style lint engine for the writing family.

Owned by `natural-writing`. Its own academic wrapper plus
`engineering-writing` and `legal-writing` import this module from thin wrapper
scripts and pass domain-specific phrase/pattern overlays (LaTeX safety,
engineering process language, legal filing bans) rather than re-implementing
the overlapping regex engine three times.

This module supplies:

- the shared word/phrase vocabulary that all three domain checkers flagged
  (em dash, puffery adjectives, second-tier inflation vocabulary, copula
  avoidance, US-spelling candidates, wordy fillers, -ly-adverb hyphenation,
  should-be-closed compounds, stacked hyphen chains);
- `ignored_spans` / `is_ignored` / `is_table_separator_line` / `line_col`:
  the span bookkeeping so a finding never fires inside frontmatter, a code
  fence, inline code, a Markdown link target, a bare URL, or (when
  `latex=True`) LaTeX math, verbatim environments, comments, and non-prose
  commands;
- `scan` and `prose_word_count`: the two operations every domain wrapper
  needs, parameterised by that domain's phrase/pattern overlay;
- `run_cli`: the shared `argparse` + `--wordcount` + findings-report main
  body, so each wrapper's `main()` is a few lines that supply its overlay.

A domain skill that needs severity tiers, directory recursion, or
structural checks beyond word-level style (legal-writing's FAIL/WARN split,
heading-blank-line and HTML-comment-leak checks) keeps that logic in its own
script and imports only the shared vocabulary and span/scan primitives from
here — the goal is one source of truth for the regex vocabulary that used to
be hand-duplicated three times, not to force every domain into one CLI shape.
"""

from __future__ import annotations

import argparse
import re
from pathlib import Path

# --- Shared vocabulary -------------------------------------------------------
# Each domain wrapper merges these with its own overlay dict/list rather than
# re-typing the words. Keep additions here only when at least two of the
# three domain skills independently flagged the same item.

TIER2_PUFFERY_WORDS: tuple[str, ...] = (
    "pivotal",
    "crucial",
    "transformative",
    "groundbreaking",
    "cutting-edge",
    "seamless",
    "seamlessly",
    "robust",
    "comprehensive",
    "holistic",
)

TIER2_INFLATION_WORDS: tuple[str, ...] = (
    "leverages",
    "leveraging",
    "multifaceted",
    "meticulous",
    "myriad",
    "plethora",
    "paramount",
    "tapestry",
    "in the realm of",
    "delves into",
)

COPULA_AVOIDANCE_WORDS: tuple[str, ...] = (
    "serves as",
    "stands as",
    "boasts",
    "plays a role",
    "plays a vital role",
    "plays a key role",
    "plays a crucial role",
)

INTERPRETATION_SMUGGLING_WORDS: tuple[str, ...] = (
    "showcases",
    "underscores",
    "serves as a testament",
    "a testament to",
    "highlights the importance",
)

VAGUE_BENEFIT_PHRASES: tuple[str, ...] = (
    "enhances reliability",
    "enhances performance",
    "ensures reliability",
    "supports scalability",
    "drives impact",
    "streamlines",
)

US_SPELLING_WORDS: tuple[str, ...] = (
    "analyze",
    "analyzed",
    "analyzing",
    "organize",
    "organized",
    "organizing",
    "recognize",
    "recognized",
    "recognizing",
    "behavior",
    "color",
    "modeling",
    "labeling",
    "center",
    "paralyze",
    "catalyze",
)

WORDY_FILLERS: tuple[str, ...] = (
    "utilise",
    "utilised",
    "utilising",
    "prior to",
    "in order to",
    "aforementioned",
    "facilitate",
    "facilitates",
)

BASE_PHRASES: dict[str, list[str]] = {
    "AI-style or inflated phrasing": [
        *TIER2_PUFFERY_WORDS,
        *TIER2_INFLATION_WORDS,
        *INTERPRETATION_SMUGGLING_WORDS,
    ],
    "Vague benefit claim": list(VAGUE_BENEFIT_PHRASES),
    "Copula avoidance or inflated verb": list(COPULA_AVOIDANCE_WORDS),
    "US spelling candidate": list(US_SPELLING_WORDS),
    "Complex word (prefer a simpler one)": list(WORDY_FILLERS),
}

EM_DASH_PATTERN = re.compile("—")

# Hyphenation and structural patterns shared verbatim across the three
# original checkers (only the -ly adverb word list differed by a couple of
# entries; the union below is a strict superset so no domain checker loses
# a finding it used to make).
LY_ADVERBS = (
    "highly|clearly|fully|newly|rapidly|widely|closely|broadly|tightly|loosely"
    "|poorly|badly|heavily|finely|partly|wholly|purely|simply|directly|carefully"
    "|randomly|statistically|formally|naturally|locally|globally|recently|previously"
    "|increasingly|deeply|nearly|largely|mostly|readily|equally|strongly|weakly"
    "|densely|sparsely|manually|automatically|empirically|theoretically|significantly"
    "|substantially|relatively|inherently|explicitly|implicitly|publicly|properly"
)

BASE_PATTERNS: list[tuple[str, re.Pattern[str]]] = [
    ("Em dash", EM_DASH_PATTERN),
    ("Negative-parallelism padding", re.compile(r"\bnot (only|just|merely)\b", re.I)),
    ("Overall as a paragraph opener", re.compile(r"(?m)^\s*(?:[Oo]verall|In conclusion|In summary|Ultimately)\b")),
    (
        "Adverb-LY hyphen (AU: no hyphen after an -ly adverb)",
        re.compile(rf"\b(?:{LY_ADVERBS})-(?!fledged|fashioned)[a-z]+\b", re.I),
    ),
    (
        "Should-be-closed compound (modern AU)",
        re.compile(
            r"\b(?:data-set|hyper-parameter|pre-processing|pre-process(?:ed|ing)|on-line|e-mail"
            r"|co-ordinat[a-z]*|co-operat[a-z]*|web-site|data-base|hyper-link)\b",
            re.I,
        ),
    ),
    ("Stacked hyphen chain of 4+ words (consider recasting)", re.compile(r"\b[A-Za-z]+(?:-[A-Za-z]+){3,}\b")),
    ("'X-versus-Y' hyphen stack (recast as a phrase)", re.compile(r"\b[A-Za-z]+-versus-[A-Za-z-]+\b", re.I)),
]

CHAIN_OK = frozenset({"state-of-the-art", "out-of-distribution", "end-to-end", "out-of-the-box"})


def phrase_pattern(phrase: str) -> re.Pattern[str]:
    parts = [re.escape(part) for part in phrase.split()]
    body = r"\s+".join(parts)
    return re.compile(rf"(?<![A-Za-z]){body}(?![A-Za-z])", re.I)


# --- LaTeX-aware span handling (used when latex=True) -----------------------

_LATEX_NON_PROSE_COMMANDS = {
    "cite", "citep", "citet", "Cref", "cref", "ref", "autoref", "eqref",
    "label", "result", "metric", "model", "path", "url", "includegraphics",
    "bibliography", "bibliographystyle", "begin", "end", "input", "include",
    "gls", "glspl", "Gls", "acrshort", "acrlong", "acrfull",
}

_LATEX_VERBATIM_ARG_COMMANDS = {"texttt", "verb", "lstinline", "mintinline", "code", "ttfamily"}


def _latex_spans(text: str) -> list[tuple[int, int]]:
    spans: list[tuple[int, int]] = []
    verbatim_env = re.compile(
        r"\\begin\{(?:verbatim|lstlisting|minted|Verbatim)\}.*?"
        r"\\end\{(?:verbatim|lstlisting|minted|Verbatim)\}",
        re.S,
    )
    spans.extend((m.start(), m.end()) for m in verbatim_env.finditer(text))
    spans.extend((m.start(), m.end()) for m in re.finditer(r"(?<!\\)%[^\n]*", text))
    spans.extend((m.start(), m.end()) for m in re.finditer(r"\\[A-Za-z@]+\*?", text))
    command_with_args = re.compile(r"\\([A-Za-z@]+)\*?(?:\s*\[[^\]]*\])?(?:\s*\{[^{}]*\})+")
    for m in command_with_args.finditer(text):
        if m.group(1) in _LATEX_NON_PROSE_COMMANDS or m.group(1) in _LATEX_VERBATIM_ARG_COMMANDS:
            spans.append((m.start(), m.end()))
    href_url = re.compile(r"\\href\s*\{[^{}]*\}")
    spans.extend((m.start(), m.end()) for m in href_url.finditer(text))
    inline_verb = re.compile(
        r"\\(?:verb|lstinline|mintinline)\*?(?:\[[^\]]*\])?"
        r"(?:([^A-Za-z\s*{])(?:(?!\1)[^\n])*?\1|\{[^{}]*\})"
    )
    spans.extend((m.start(), m.end()) for m in inline_verb.finditer(text))
    math = re.compile(r"(?<!\\)\$[^$]*?\$|\\\(.*?\\\)|\\\[.*?\\\]", re.S)
    spans.extend((m.start(), m.end()) for m in math.finditer(text))
    math_env = re.compile(
        r"\\begin\{(equation|align|gather|multline|eqnarray|alignat|flalign)\*?\}.*?\\end\{\1\*?\}",
        re.S,
    )
    spans.extend((m.start(), m.end()) for m in math_env.finditer(text))
    spans.extend((m.start(), m.end()) for m in re.finditer(r"\\.", text))
    return spans


def ignored_spans(text: str, suffix: str, latex: bool = False) -> list[tuple[int, int]]:
    """Spans a finding must not fire inside: frontmatter, code fences, inline
    code, Markdown link targets, bare URLs, and (latex=True) LaTeX math,
    verbatim environments, comments and non-prose commands."""
    spans: list[tuple[int, int]] = []
    if suffix in {".md", ".markdown"} and text.startswith("---\n"):
        end = text.find("\n---\n", 4)
        if end != -1:
            spans.append((0, end + 5))

    spans.extend((m.start(), m.end()) for m in re.finditer(r"```.*?```", text, re.S))
    spans.extend((m.start(), m.end()) for m in re.finditer(r"`[^`\n]+`", text))
    spans.extend((m.start(), m.end()) for m in re.finditer(r"\]\([^)\s]+\)", text))
    spans.extend((m.start(), m.end()) for m in re.finditer(r"https?://\S+", text))

    if latex and suffix in {".tex", ".latex", ".sty", ".cls"}:
        spans.extend(_latex_spans(text))
    return spans


def is_ignored(index: int, spans: list[tuple[int, int]]) -> bool:
    return any(start <= index < end for start, end in spans)


def is_table_separator_line(text: str, index: int) -> bool:
    line_start = text.rfind("\n", 0, index) + 1
    line_end = text.find("\n", index)
    if line_end == -1:
        line_end = len(text)
    line = text[line_start:line_end].strip()
    return bool(line) and set(line) <= {"|", "-", ":", " "}


def line_col(text: str, index: int) -> tuple[int, int]:
    line = text.count("\n", 0, index) + 1
    line_start = text.rfind("\n", 0, index) + 1
    return line, index - line_start + 1


def scan(
    path: Path,
    phrases: dict[str, list[str]] | None = None,
    patterns: list[tuple[str, re.Pattern[str]]] | None = None,
    latex: bool = False,
) -> list[tuple[int, int, str, str]]:
    phrases = BASE_PHRASES if phrases is None else phrases
    patterns = BASE_PATTERNS if patterns is None else patterns
    text = path.read_text(encoding="utf-8")
    spans = ignored_spans(text, path.suffix.lower(), latex=latex)
    findings: list[tuple[int, int, str, str]] = []

    for label, pattern in patterns:
        for match in pattern.finditer(text):
            if is_ignored(match.start(), spans) or is_table_separator_line(text, match.start()):
                continue
            if label.startswith("Stacked hyphen chain") and match.group(0).lower() in CHAIN_OK:
                continue
            line, col = line_col(text, match.start())
            findings.append((line, col, label, match.group(0)))

    for label, phrase_list in phrases.items():
        for phrase in phrase_list:
            for match in phrase_pattern(phrase).finditer(text):
                if is_ignored(match.start(), spans):
                    continue
                line, col = line_col(text, match.start())
                findings.append((line, col, label, match.group(0)))

    versus_positions = {(line, col) for line, col, label, _ in findings if label.startswith("'X-versus-Y'")}
    findings = [
        f for f in findings
        if not (f[2].startswith("Stacked hyphen chain") and (f[0], f[1]) in versus_positions)
    ]
    return sorted(findings)


def prose_word_count(path: Path, latex: bool = False) -> int:
    """Count condensable prose words for the condense-pass delta (before vs
    after). Code fences, inline code, frontmatter, links (and, if latex=True,
    math/verbatim/comments/non-prose commands) are blanked first. Measures
    size only; the cut decision stays with the human or agent."""
    text = path.read_text(encoding="utf-8")
    spans = ignored_spans(text, path.suffix.lower(), latex=latex)
    chars = list(text)
    for start, end in spans:
        for index in range(start, min(end, len(chars))):
            chars[index] = " "
    return len(re.findall(r"[A-Za-z]+(?:['-][A-Za-z]+)*", "".join(chars)))


def run_cli(
    description: str,
    phrases: dict[str, list[str]] | None = None,
    patterns: list[tuple[str, re.Pattern[str]]] | None = None,
    latex: bool = False,
    extra_scan: "callable | None" = None,
) -> int:
    """Shared argparse + report body. `extra_scan(path) -> list[(line, col,
    label, snippet)]` lets a domain wrapper append checks the shared engine
    does not model (e.g. engineering's curly-quote-in-code check)."""
    parser = argparse.ArgumentParser(description=description)
    parser.add_argument("paths", nargs="+", type=Path)
    parser.add_argument(
        "--wordcount",
        action="store_true",
        help="print the prose word count instead of style findings",
    )
    args = parser.parse_args()

    if args.wordcount:
        missing = False
        for path in args.paths:
            if not path.exists():
                print(f"{path}: missing")
                missing = True
                continue
            print(f"{path}: {prose_word_count(path, latex=latex)} prose words")
        return 1 if missing else 0

    any_findings = False
    for path in args.paths:
        if not path.exists():
            print(f"{path}: missing")
            any_findings = True
            continue
        findings = scan(path, phrases=phrases, patterns=patterns, latex=latex)
        if extra_scan is not None:
            findings = sorted(findings + extra_scan(path))
        if findings:
            any_findings = True
            print(f"{path}: {len(findings)} finding(s)")
            for line, col, label, snippet in findings:
                print(f"  {line}:{col}: {label}: {snippet!r}")
        else:
            print(f"{path}: no findings")
    return 1 if any_findings else 0
