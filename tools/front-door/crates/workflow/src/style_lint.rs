//! Shared prose style rules and span-aware scanning for the workflow crate.
//!
//! The source is `skills/natural-writing/scripts/style_lint.py` through
//! `prose_word_count` (unit-01). Regex matches use byte offsets internally,
//! then convert them to Python-compatible Unicode scalar offsets for findings.

use regex::{Regex, RegexBuilder};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const TIER2_PUFFERY_WORDS: &[&str] = &[
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
];

pub const TIER2_INFLATION_WORDS: &[&str] = &[
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
];

pub const COPULA_AVOIDANCE_WORDS: &[&str] = &[
    "serves as",
    "stands as",
    "boasts",
    "plays a role",
    "plays a vital role",
    "plays a key role",
    "plays a crucial role",
];

pub const INTERPRETATION_SMUGGLING_WORDS: &[&str] = &[
    "showcases",
    "underscores",
    "serves as a testament",
    "a testament to",
    "highlights the importance",
];

pub const VAGUE_BENEFIT_PHRASES: &[&str] = &[
    "enhances reliability",
    "enhances performance",
    "ensures reliability",
    "supports scalability",
    "drives impact",
    "streamlines",
];

pub const US_SPELLING_WORDS: &[&str] = &[
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
];

pub const WORDY_FILLERS: &[&str] = &[
    "utilise",
    "utilised",
    "utilising",
    "prior to",
    "in order to",
    "aforementioned",
    "facilitate",
    "facilitates",
];

pub const LY_ADVERBS: &str = concat!(
    "highly|clearly|fully|newly|rapidly|widely|closely|broadly|tightly|loosely",
    "|poorly|badly|heavily|finely|partly|wholly|purely|simply|directly|carefully",
    "|randomly|statistically|formally|naturally|locally|globally|recently|previously",
    "|increasingly|deeply|nearly|largely|mostly|readily|equally|strongly|weakly",
    "|densely|sparsely|manually|automatically|empirically|theoretically|significantly",
    "|substantially|relatively|inherently|explicitly|implicitly|publicly|properly",
);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhraseGroup {
    pub label: String,
    pub phrases: Vec<String>,
}

impl PhraseGroup {
    pub fn new(
        label: impl Into<String>,
        phrases: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            label: label.into(),
            phrases: phrases.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PatternRule {
    label: String,
    matcher: RuleMatcher,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PatternFlags {
    pub case_insensitive: bool,
    pub multiline: bool,
    pub dot_all: bool,
}

#[derive(Clone, Debug)]
enum RuleMatcher {
    Builtin(BuiltinRule),
    Regex(Regex),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BuiltinRule {
    EmDash,
    NegativeParallelism,
    ParagraphOpener,
    LyHyphen,
    ClosedCompound,
    StackedHyphen,
    VersusStack,
    AcademicTripleHyphen,
    AcademicUnescapedPercent,
}

const PYTHON_LOOKAROUND_OVERLAYS: &[(&str, BuiltinRule)] = &[
    ("(?<!-)---(?!-)", BuiltinRule::AcademicTripleHyphen),
    (r"(?<!\\)\d+%", BuiltinRule::AcademicUnescapedPercent),
];

#[derive(Clone, Copy)]
enum Boundary {
    None,
    PythonWordBoth,
    PythonWordEnd,
    NoAdjacentHyphen,
    NotEscaped,
}

#[derive(Debug, Error)]
pub enum StyleError {
    #[error("invalid style regex {expression:?}: {source}")]
    Regex {
        expression: String,
        #[source]
        source: regex::Error,
    },
    #[error("cannot read style input {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot decode style input {path} as UTF-8: {source}")]
    UnicodeDecode {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub type Finding = (usize, usize, String, String);

/// Domain wrappers may add checks after the shared scan. The trait keeps that
/// source variation at the caller seam instead of coupling this engine to a
/// particular writing domain.
pub trait ExtraScanner {
    fn scan(&self, path: &Path) -> Result<Vec<Finding>, StyleError>;
}

impl<F> ExtraScanner for F
where
    F: Fn(&Path) -> Result<Vec<Finding>, StyleError>,
{
    fn scan(&self, path: &Path) -> Result<Vec<Finding>, StyleError> {
        self(path)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl PatternRule {
    /// Compile a Rust-regex overlay. Built-in Python rules are represented by
    /// `base_patterns`; overlays that need Python-only lookaround syntax must
    /// express the equivalent boundary check in their caller.
    pub fn regex(label: impl Into<String>, expression: &str) -> Result<Self, StyleError> {
        Ok(Self {
            label: label.into(),
            matcher: RuleMatcher::Regex(compile(expression, false, false)?),
        })
    }

    /// Compile a typed Python-style overlay from pattern text and flags.
    ///
    /// The allowlisted `regex` crate supports the common Python-compatible
    /// subset used by overlays. Python-only lookaround and backreference
    /// constructs return a typed compile error and remain outside this API.
    pub fn python_regex(
        label: impl Into<String>,
        expression: &str,
        flags: PatternFlags,
    ) -> Result<Self, StyleError> {
        let label = label.into();
        let academic_rule = PYTHON_LOOKAROUND_OVERLAYS
            .iter()
            .find(|(pattern, _)| *pattern == expression)
            .map(|(_, rule)| *rule);
        if let Some(rule) = academic_rule {
            return Ok(Self {
                label,
                matcher: RuleMatcher::Builtin(rule),
            });
        }
        let translated = if flags.case_insensitive {
            python_ignorecase_expression(expression)
        } else {
            expression.to_owned()
        };
        let regex = RegexBuilder::new(&translated)
            .case_insensitive(flags.case_insensitive)
            .multi_line(flags.multiline)
            .dot_matches_new_line(flags.dot_all)
            .build()
            .map_err(|source| StyleError::Regex {
                expression: expression.to_owned(),
                source,
            })?;
        Ok(Self {
            label,
            matcher: RuleMatcher::Regex(regex),
        })
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

pub fn base_phrases() -> Vec<PhraseGroup> {
    vec![
        PhraseGroup::new(
            "AI-style or inflated phrasing",
            TIER2_PUFFERY_WORDS
                .iter()
                .chain(TIER2_INFLATION_WORDS)
                .chain(INTERPRETATION_SMUGGLING_WORDS)
                .copied(),
        ),
        PhraseGroup::new("Vague benefit claim", VAGUE_BENEFIT_PHRASES.iter().copied()),
        PhraseGroup::new(
            "Copula avoidance or inflated verb",
            COPULA_AVOIDANCE_WORDS.iter().copied(),
        ),
        PhraseGroup::new("US spelling candidate", US_SPELLING_WORDS.iter().copied()),
        PhraseGroup::new(
            "Complex word (prefer a simpler one)",
            WORDY_FILLERS.iter().copied(),
        ),
    ]
}

pub fn base_patterns() -> Vec<PatternRule> {
    [
        ("Em dash", BuiltinRule::EmDash),
        (
            "Negative-parallelism padding",
            BuiltinRule::NegativeParallelism,
        ),
        (
            "Overall as a paragraph opener",
            BuiltinRule::ParagraphOpener,
        ),
        (
            "Adverb-LY hyphen (AU: no hyphen after an -ly adverb)",
            BuiltinRule::LyHyphen,
        ),
        (
            "Should-be-closed compound (modern AU)",
            BuiltinRule::ClosedCompound,
        ),
        (
            "Stacked hyphen chain of 4+ words (consider recasting)",
            BuiltinRule::StackedHyphen,
        ),
        (
            "'X-versus-Y' hyphen stack (recast as a phrase)",
            BuiltinRule::VersusStack,
        ),
    ]
    .into_iter()
    .map(|(label, matcher)| PatternRule {
        label: label.to_owned(),
        matcher: RuleMatcher::Builtin(matcher),
    })
    .collect()
}

fn compile(expression: &str, case_insensitive: bool, dot_all: bool) -> Result<Regex, StyleError> {
    RegexBuilder::new(expression)
        .case_insensitive(case_insensitive)
        .dot_matches_new_line(dot_all)
        .build()
        .map_err(|source| StyleError::Regex {
            expression: expression.to_owned(),
            source,
        })
}

fn python_ignorecase_expression(expression: &str) -> String {
    let mut output = String::with_capacity(expression.len());
    let mut characters = expression.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\\' {
            output.push(character);
            if let Some(escaped) = characters.next() {
                output.push(escaped);
            }
            continue;
        }
        if character == '[' {
            let mut class = String::from("[");
            let mut closed = false;
            while let Some(member) = characters.next() {
                class.push(member);
                if member == '\\' {
                    if let Some(escaped) = characters.next() {
                        class.push(escaped);
                    }
                } else if member == ']' {
                    closed = true;
                    break;
                }
            }
            if closed && (class.contains("a-z") || class.contains("A-Z")) {
                class.insert_str(class.len() - 1, "İıſK");
            }
            output.push_str(&class);
            continue;
        }
        output.push_str(match character {
            'i' | 'I' => "[iIİı]",
            's' | 'S' => "[sSſ]",
            'k' | 'K' => "[kKK]",
            _ => {
                output.push(character);
                continue;
            }
        });
    }
    output
}

fn compile_python_ignorecase(expression: &str, dot_all: bool) -> Result<Regex, StyleError> {
    let expression = python_ignorecase_expression(expression);
    compile(&expression, true, dot_all)
}

fn builtin_expression(rule: BuiltinRule) -> (String, bool, bool, Boundary) {
    match rule {
        BuiltinRule::EmDash => ("—".to_owned(), false, false, Boundary::None),
        BuiltinRule::NegativeParallelism => (
            r"not (?:only|just|merely)".to_owned(),
            true,
            false,
            Boundary::PythonWordBoth,
        ),
        BuiltinRule::ParagraphOpener => (
            r"(?m)^[\s\x{001c}-\x{001f}]*(?:[Oo]verall|In conclusion|In summary|Ultimately)"
                .to_owned(),
            false,
            false,
            Boundary::PythonWordEnd,
        ),
        BuiltinRule::LyHyphen => (
            format!("(?:{LY_ADVERBS})-(?P<tail>[a-z]+)"),
            true,
            false,
            Boundary::PythonWordBoth,
        ),
        BuiltinRule::ClosedCompound => (
            concat!(
                r"(?:data-set|hyper-parameter|pre-processing|pre-process(?:ed|ing)|on-line|e-mail",
                r"|co-ordinat[a-z]*|co-operat[a-z]*|web-site|data-base|hyper-link)"
            )
            .to_owned(),
            true,
            false,
            Boundary::PythonWordBoth,
        ),
        BuiltinRule::StackedHyphen => (
            r"[A-Za-z]+(?:-[A-Za-z]+){3,}".to_owned(),
            false,
            false,
            Boundary::PythonWordBoth,
        ),
        BuiltinRule::VersusStack => (
            r"[A-Za-z]+-versus-[A-Za-z-]+".to_owned(),
            true,
            false,
            Boundary::PythonWordBoth,
        ),
        BuiltinRule::AcademicTripleHyphen => {
            ("---".to_owned(), false, false, Boundary::NoAdjacentHyphen)
        }
        BuiltinRule::AcademicUnescapedPercent => {
            (r"\d+%".to_owned(), false, false, Boundary::NotEscaped)
        }
    }
}

fn byte_to_char(text: &str, byte: usize) -> usize {
    text.get(..byte)
        .map_or_else(|| text.chars().count(), |prefix| prefix.chars().count())
}

fn char_to_byte(text: &str, character: usize) -> usize {
    text.char_indices()
        .nth(character)
        .map_or(text.len(), |(byte, _)| byte)
}

fn normalize_python_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn adjacent_chars(text: &str, byte: usize) -> (Option<char>, Option<char>) {
    let before = text
        .get(..byte)
        .and_then(|prefix| prefix.chars().next_back());
    let after = text.get(byte..).and_then(|suffix| suffix.chars().next());
    (before, after)
}

fn python_word(c: char) -> bool {
    provenant_core::py::python_word(c)
}

fn python_word_boundary(text: &str, byte: usize) -> bool {
    let (before, after) = adjacent_chars(text, byte);
    before.is_some_and(python_word) != after.is_some_and(python_word)
}

fn ascii_letter_start_boundary(text: &str, byte: usize) -> bool {
    let (before, _) = adjacent_chars(text, byte);
    !before.is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, 'İ' | 'ı' | 'ſ' | 'K'))
}

fn ascii_letter_end_boundary(text: &str, byte: usize) -> bool {
    let (_, after) = adjacent_chars(text, byte);
    !after.is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, 'İ' | 'ı' | 'ſ' | 'K'))
}

fn next_char_byte(text: &str, byte: usize) -> Option<usize> {
    text.get(byte..)?
        .chars()
        .next()
        .map(|character| byte + character.len_utf8())
}

fn boundary_matches(text: &str, start: usize, end: usize, boundary: Boundary) -> bool {
    match boundary {
        Boundary::None => true,
        Boundary::PythonWordBoth => {
            python_word_boundary(text, start) && python_word_boundary(text, end)
        }
        Boundary::PythonWordEnd => python_word_boundary(text, end),
        Boundary::NoAdjacentHyphen => {
            adjacent_chars(text, start)
                .0
                .is_none_or(|character| character != '-')
                && adjacent_chars(text, end)
                    .1
                    .is_none_or(|character| character != '-')
        }
        Boundary::NotEscaped => adjacent_chars(text, start)
            .0
            .is_none_or(|character| character != '\\'),
    }
}

fn is_ignored_byte(text: &str, byte: usize, spans: &[(usize, usize)]) -> bool {
    let index = byte_to_char(text, byte);
    is_ignored(index, spans)
}

fn pattern_matches<'a>(
    text: &'a str,
    rule: &PatternRule,
) -> Result<Vec<(usize, usize, &'a str)>, StyleError> {
    match &rule.matcher {
        RuleMatcher::Regex(regex) => Ok(regex
            .find_iter(text)
            .map(|matched| (matched.start(), matched.end(), matched.as_str()))
            .collect()),
        RuleMatcher::Builtin(kind) => {
            let (expression, insensitive, dot_all, boundary) = builtin_expression(*kind);
            let regex = if insensitive {
                compile_python_ignorecase(&expression, dot_all)?
            } else {
                compile(&expression, false, dot_all)?
            };
            let mut matches = Vec::new();
            let mut cursor = 0;
            while let Some(matched) = regex.find_at(text, cursor) {
                let mut accepted = boundary_matches(text, matched.start(), matched.end(), boundary);
                if accepted && *kind == BuiltinRule::LyHyphen {
                    accepted = matched
                        .as_str()
                        .split_once('-')
                        .map(|(_, tail)| {
                            tail.replace(['İ', 'ı'], "i")
                                .replace('ſ', "s")
                                .replace('K', "k")
                                .to_lowercase()
                        })
                        .is_some_and(|tail| {
                            !tail.starts_with("fledged") && !tail.starts_with("fashioned")
                        });
                }
                if accepted {
                    matches.push((matched.start(), matched.end(), matched.as_str()));
                    if matched.end() > matched.start() {
                        cursor = matched.end();
                    } else if let Some(next) = next_char_byte(text, matched.start()) {
                        cursor = next;
                    } else {
                        break;
                    }
                } else if let Some(next) = next_char_byte(text, matched.start()) {
                    cursor = next;
                } else {
                    break;
                }
            }
            Ok(matches)
        }
    }
}

fn regex_spans(
    text: &str,
    expression: &str,
    dot_all: bool,
) -> Result<Vec<(usize, usize)>, StyleError> {
    let regex = compile(expression, false, dot_all)?;
    Ok(regex
        .find_iter(text)
        .map(|matched| {
            (
                byte_to_char(text, matched.start()),
                byte_to_char(text, matched.end()),
            )
        })
        .collect())
}

fn push_regex_spans(
    spans: &mut Vec<(usize, usize)>,
    text: &str,
    expression: &str,
    dot_all: bool,
) -> Result<(), StyleError> {
    spans.extend(regex_spans(text, expression, dot_all)?);
    Ok(())
}

fn latex_comment_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = text.get(cursor..).and_then(|rest| rest.find('%')) {
        let byte = cursor + relative;
        let previous = text[..byte].chars().next_back();
        if previous != Some('\\') {
            let end = text[byte..]
                .find('\n')
                .map_or(text.len(), |relative_end| byte + relative_end);
            spans.push((byte_to_char(text, byte), byte_to_char(text, end)));
            cursor = end;
        } else {
            cursor = byte + 1;
        }
    }
    spans
}

fn latex_inline_verb_spans(text: &str) -> Result<Vec<(usize, usize)>, StyleError> {
    let prefix = compile(
        r"\\(?:verb|lstinline|mintinline)\*?(?:\[[^\x{005d}]*\])?",
        false,
        false,
    )?;
    let mut spans = Vec::new();
    for command in prefix.find_iter(text) {
        let mut chars = text[command.end()..].char_indices();
        let Some((_, first)) = chars.next() else {
            continue;
        };
        if first == '{' {
            let after_open = command.end() + 1;
            let Some((close_relative, '}')) = text[after_open..]
                .char_indices()
                .find(|(_, character)| matches!(*character, '{' | '}'))
            else {
                continue;
            };
            let end_byte = after_open + close_relative + 1;
            spans.push((
                byte_to_char(text, command.start()),
                byte_to_char(text, end_byte),
            ));
            continue;
        }
        if first.is_ascii_alphabetic()
            || provenant_core::py::python_whitespace(first)
            || matches!(first, '*' | '{')
        {
            continue;
        }
        let delimiter = first;
        let content_start = command.end() + first.len_utf8();
        let Some((relative, _)) = text[content_start..]
            .char_indices()
            .take_while(|(_, character)| *character != '\n')
            .find(|(_, character)| *character == delimiter)
        else {
            continue;
        };
        let end_byte = content_start + relative + delimiter.len_utf8();
        spans.push((
            byte_to_char(text, command.start()),
            byte_to_char(text, end_byte),
        ));
    }
    Ok(spans)
}

fn latex_math_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut cursor = 0;
    while cursor < text.len() {
        let Some((relative, character)) = text[cursor..].char_indices().next() else {
            break;
        };
        let start = cursor + relative;
        let end = if character == '$' {
            let previous = text[..start].chars().next_back();
            if previous == Some('\\') {
                cursor = start + character.len_utf8();
                continue;
            }
            text[start + 1..]
                .find('$')
                .map(|distance| start + 1 + distance + 1)
        } else if character == '\\' {
            let remainder = &text[start..];
            let delimiter = if remainder.starts_with(r"\(") {
                Some(r"\)")
            } else if remainder.starts_with(r"\[") {
                Some(r"\]")
            } else {
                None
            };
            delimiter.and_then(|closing| {
                text[start + 2..]
                    .find(closing)
                    .map(|distance| start + 2 + distance + 2)
            })
        } else {
            None
        };
        if let Some(end) = end {
            spans.push((byte_to_char(text, start), byte_to_char(text, end)));
            cursor = end;
        } else {
            cursor = start + character.len_utf8();
        }
    }
    spans
}

fn latex_math_environment_spans(text: &str) -> Result<Vec<(usize, usize)>, StyleError> {
    let begin = compile(
        r"\\begin\{(?P<env>equation|align|gather|multline|eqnarray|alignat|flalign)\*?\}",
        false,
        false,
    )?;
    let end = compile(
        r"\\end\{(?P<env>equation|align|gather|multline|eqnarray|alignat|flalign)\*?\}",
        false,
        false,
    )?;
    let mut spans = Vec::new();
    let mut cursor = 0;
    for opening in begin.captures_iter(text) {
        let Some(opening_match) = opening.get(0) else {
            continue;
        };
        if opening_match.start() < cursor {
            continue;
        }
        let Some(environment) = opening.name("env").map(|capture| capture.as_str()) else {
            continue;
        };
        let suffix = &text[opening_match.end()..];
        let closing = end.captures_iter(suffix).find(|candidate| {
            candidate
                .name("env")
                .is_some_and(|capture| capture.as_str() == environment)
        });
        let Some(closing_match) = closing.and_then(|capture| capture.get(0)) else {
            continue;
        };
        let end_byte = opening_match.end() + closing_match.end();
        spans.push((
            byte_to_char(text, opening_match.start()),
            byte_to_char(text, end_byte),
        ));
        cursor = end_byte;
    }
    Ok(spans)
}

fn latex_command_argument_spans(text: &str) -> Result<Vec<(usize, usize)>, StyleError> {
    const NON_PROSE: &[&str] = &[
        "cite",
        "citep",
        "citet",
        "Cref",
        "cref",
        "ref",
        "autoref",
        "eqref",
        "label",
        "result",
        "metric",
        "model",
        "path",
        "url",
        "includegraphics",
        "bibliography",
        "bibliographystyle",
        "begin",
        "end",
        "input",
        "include",
        "gls",
        "glspl",
        "Gls",
        "acrshort",
        "acrlong",
        "acrfull",
    ];
    const VERBATIM_ARGUMENT: &[&str] = &[
        "texttt",
        "verb",
        "lstinline",
        "mintinline",
        "code",
        "ttfamily",
    ];
    let regex = compile(
        r"\\(?P<command>[A-Za-z@]+)\*?(?:(?:\s|\x{001c}|\x{001d}|\x{001e}|\x{001f})*\[[^\x{005d}]*\])?(?:(?:\s|\x{001c}|\x{001d}|\x{001e}|\x{001f})*\{[^{}]*\})+",
        false,
        false,
    )?;
    Ok(regex
        .captures_iter(text)
        .filter_map(|captures| {
            let matched = captures.get(0)?;
            let command = captures.name("command")?.as_str();
            (NON_PROSE.contains(&command) || VERBATIM_ARGUMENT.contains(&command)).then_some((
                byte_to_char(text, matched.start()),
                byte_to_char(text, matched.end()),
            ))
        })
        .collect())
}

fn latex_spans(text: &str) -> Result<Vec<(usize, usize)>, StyleError> {
    let mut spans = Vec::new();
    push_regex_spans(
        &mut spans,
        text,
        r"\\begin\{(?:verbatim|lstlisting|minted|Verbatim)\}.*?\\end\{(?:verbatim|lstlisting|minted|Verbatim)\}",
        true,
    )?;
    spans.extend(latex_comment_spans(text));
    push_regex_spans(&mut spans, text, r"\\[A-Za-z@]+\*?", false)?;
    spans.extend(latex_command_argument_spans(text)?);
    push_regex_spans(
        &mut spans,
        text,
        r"\\href(?:\s|\x{001c}|\x{001d}|\x{001e}|\x{001f})*\{[^{}]*\}",
        false,
    )?;
    spans.extend(latex_inline_verb_spans(text)?);
    spans.extend(latex_math_spans(text));
    spans.extend(latex_math_environment_spans(text)?);
    push_regex_spans(&mut spans, text, r"\\[^\n]", false)?;
    Ok(spans)
}

/// Return source-compatible ignored spans as Unicode scalar offsets.
pub fn ignored_spans(
    text: &str,
    suffix: &str,
    latex: bool,
) -> Result<Vec<(usize, usize)>, StyleError> {
    let mut spans = Vec::new();
    if matches!(suffix, ".md" | ".markdown") && text.starts_with("---\n") {
        if let Some(end) = text
            .get(4..)
            .and_then(|remainder| remainder.find("\n---\n"))
        {
            let byte_end = 4 + end + 5;
            spans.push((0, byte_to_char(text, byte_end)));
        }
    }
    push_regex_spans(&mut spans, text, r"```.*?```", true)?;
    push_regex_spans(&mut spans, text, r"`[^`\n]+`", false)?;
    push_regex_spans(
        &mut spans,
        text,
        r"\]\([^\x{0029}\s\x{001c}-\x{001f}]+\)",
        false,
    )?;
    push_regex_spans(&mut spans, text, r"https?://[^\s\x{001c}-\x{001f}]+", false)?;
    if latex && matches!(suffix, ".tex" | ".latex" | ".sty" | ".cls") {
        spans.extend(latex_spans(text)?);
    }
    Ok(spans)
}

pub fn is_ignored(index: usize, spans: &[(usize, usize)]) -> bool {
    spans
        .iter()
        .any(|(start, end)| *start <= index && index < *end)
}

pub fn is_table_separator_line(text: &str, index: usize) -> bool {
    let byte = char_to_byte(text, index);
    let before = &text[..byte];
    let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
    let after = &text[byte..];
    let line_end = after
        .find('\n')
        .map_or(text.len(), |newline| byte + newline);
    let line = provenant_core::py::python_strip(&text[line_start..line_end]);
    !line.is_empty()
        && line
            .chars()
            .all(|character| matches!(character, '|' | '-' | ':' | ' '))
}

/// Compute one-based Python-style line and Unicode scalar column coordinates.
pub fn line_col(text: &str, index: usize) -> (usize, usize) {
    let byte = char_to_byte(text, index);
    let prefix = &text[..byte];
    let line = prefix
        .chars()
        .filter(|character| *character == '\n')
        .count()
        + 1;
    let column = prefix
        .rsplit('\n')
        .next()
        .map_or(1, |last_line| last_line.chars().count() + 1);
    (line, column)
}

/// Scan text with the default rules, or replace either rule collection with an
/// ordered caller-supplied overlay. Pattern overlays use Rust regex syntax.
pub fn scan_text(
    text: &str,
    suffix: &str,
    phrases: Option<&[PhraseGroup]>,
    patterns: Option<&[PatternRule]>,
    latex: bool,
) -> Result<Vec<(usize, usize, String, String)>, StyleError> {
    let normalized;
    let text = if text.contains('\r') {
        normalized = normalize_python_newlines(text);
        normalized.as_str()
    } else {
        text
    };
    let spans = ignored_spans(text, suffix, latex)?;
    let default_phrases;
    let phrases = match phrases {
        Some(phrases) => phrases,
        None => {
            default_phrases = base_phrases();
            &default_phrases
        }
    };
    let default_patterns;
    let patterns = match patterns {
        Some(patterns) => patterns,
        None => {
            default_patterns = base_patterns();
            &default_patterns
        }
    };
    let mut findings: Vec<(usize, usize, String, String)> = Vec::new();
    for rule in patterns {
        for (start, _end, snippet) in pattern_matches(text, rule)? {
            if is_ignored_byte(text, start, &spans)
                || is_table_separator_line(text, byte_to_char(text, start))
            {
                continue;
            }
            if rule.label.starts_with("Stacked hyphen chain")
                && matches!(
                    snippet.to_lowercase().as_str(),
                    "state-of-the-art" | "out-of-distribution" | "end-to-end" | "out-of-the-box"
                )
            {
                continue;
            }
            let (line, column) = line_col(text, byte_to_char(text, start));
            findings.push((line, column, rule.label.clone(), snippet.to_owned()));
        }
    }
    for group in phrases {
        for phrase in &group.phrases {
            let pattern = phrase_pattern(phrase)?;
            let mut cursor = 0;
            while let Some(matched) = pattern.find_at(text, cursor) {
                let start = matched.start();
                let end = matched.end();
                if ascii_letter_start_boundary(text, start)
                    && ascii_letter_end_boundary(text, end)
                    && !is_ignored_byte(text, start, &spans)
                {
                    let start_index = byte_to_char(text, start);
                    let (line, column) = line_col(text, start_index);
                    findings.push((
                        line,
                        column,
                        group.label.clone(),
                        matched.as_str().to_owned(),
                    ));
                    if end > start {
                        cursor = end;
                    } else if let Some(next) = next_char_byte(text, start) {
                        cursor = next;
                    } else {
                        break;
                    }
                } else if let Some(next) = next_char_byte(text, start) {
                    cursor = next;
                } else {
                    break;
                }
            }
        }
    }
    let versus_positions: Vec<(usize, usize)> = findings
        .iter()
        .filter(|finding| finding.2.starts_with("'X-versus-Y'"))
        .map(|finding| (finding.0, finding.1))
        .collect();
    findings.retain(|finding| {
        !(finding.2.starts_with("Stacked hyphen chain")
            && versus_positions.contains(&(finding.0, finding.1)))
    });
    findings.sort();
    Ok(findings)
}

fn phrase_pattern(phrase: &str) -> Result<Regex, StyleError> {
    let parts: Vec<String> = phrase
        .split(provenant_core::py::python_whitespace)
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut escaped = String::new();
            for character in part.chars() {
                let literal = regex::escape(&character.to_string());
                escaped.push_str(&python_ignorecase_expression(&literal));
            }
            escaped
        })
        .collect();
    let body = parts.join(r"(?:\s|\x{001c}|\x{001d}|\x{001e}|\x{001f})+");
    compile(&body, true, false)
}

pub fn scan(
    path: impl AsRef<Path>,
    phrases: Option<&[PhraseGroup]>,
    patterns: Option<&[PatternRule]>,
    latex: bool,
) -> Result<Vec<(usize, usize, String, String)>, StyleError> {
    let path = path.as_ref();
    let text = read_utf8(path)?;
    let suffix = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .filter(|extension| !extension.is_empty())
        .map_or_else(String::new, |extension| format!(".{extension}"));
    scan_text(&text, &suffix, phrases, patterns, latex)
}

/// Count ASCII prose words after blanking every ignored scalar span.
pub fn prose_word_count_text(text: &str, suffix: &str, latex: bool) -> Result<usize, StyleError> {
    let normalized;
    let text = if text.contains('\r') {
        normalized = normalize_python_newlines(text);
        normalized.as_str()
    } else {
        text
    };
    let spans = ignored_spans(text, suffix, latex)?;
    let mut characters: Vec<char> = text.chars().collect();
    for (start, end) in spans {
        for index in start..end.min(characters.len()) {
            if let Some(character) = characters.get_mut(index) {
                *character = ' ';
            }
        }
    }
    let blanked: String = characters.into_iter().collect();
    let words = compile(r"[A-Za-z]+(?:['-][A-Za-z]+)*", false, false)?;
    Ok(words.find_iter(&blanked).count())
}

pub fn prose_word_count(path: impl AsRef<Path>, latex: bool) -> Result<usize, StyleError> {
    let path = path.as_ref();
    let text = read_utf8(path)?;
    let suffix = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .filter(|extension| !extension.is_empty())
        .map_or_else(String::new, |extension| format!(".{extension}"));
    prose_word_count_text(&text, &suffix, latex)
}

fn read_utf8(path: &Path) -> Result<String, StyleError> {
    std::fs::read_to_string(path).map_err(|source| {
        let path = path.to_path_buf();
        if source.kind() == std::io::ErrorKind::InvalidData {
            StyleError::UnicodeDecode { path, source }
        } else {
            StyleError::Read { path, source }
        }
    })
}

/// Run the shared style command against explicit argv, returning its output
/// and status for the binary edge to render. `argv` includes the program name,
/// as `argparse`'s `sys.argv` does.
pub fn run_cli(
    argv: &[OsString],
    description: &str,
    phrases: Option<&[PhraseGroup]>,
    patterns: Option<&[PatternRule]>,
    latex: bool,
    extra_scan: Option<&dyn ExtraScanner>,
) -> Result<CliOutput, StyleError> {
    let program = argv.first().map_or_else(
        || "-".to_owned(),
        |value| {
            value
                .to_string_lossy()
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_owned()
        },
    );
    let args = argv.get(1..).unwrap_or_default();
    let mut paths = Vec::new();
    let mut wordcount = false;
    let mut positional_only = false;
    let mut positional_closed = false;
    let mut unknown = Vec::new();

    for argument in args {
        let Some(text) = argument.to_str() else {
            if positional_closed {
                unknown.push(argument.to_string_lossy().into_owned());
            } else if positional_only || !argument.to_string_lossy().starts_with('-') {
                paths.push(PathBuf::from(argument));
            } else {
                positional_closed = !paths.is_empty();
                unknown.push(argument.to_string_lossy().into_owned());
            }
            continue;
        };
        if positional_only {
            if positional_closed {
                unknown.push(text.to_owned());
            } else {
                paths.push(PathBuf::from(argument));
            }
        } else if text == "--" {
            positional_only = true;
        } else if text == "-h"
            || text == "--help"
            || (text.starts_with("--") && "--help".starts_with(text))
            || (text.starts_with("-h") && !text.starts_with("--") && !text.starts_with("-h="))
        {
            return Ok(CliOutput {
                exit_code: 0,
                stdout: style_help(&program, description),
                stderr: String::new(),
            });
        } else if text == "--wordcount"
            || (text.starts_with("--") && "--wordcount".starts_with(text) && text.len() > 2)
        {
            positional_closed |= !paths.is_empty();
            wordcount = true;
        } else if text == "-" || is_negative_number_argument(text) {
            if positional_closed {
                unknown.push(text.to_owned());
            } else {
                paths.push(PathBuf::from(argument));
            }
        } else if let Some((option, value)) = text.split_once('=') {
            if option == "--" {
                return Ok(cli_parse_error(
                    &program,
                    &format!("ambiguous option: {text} could match --help, --wordcount"),
                ));
            }
            let canonical = if option == "-h" || "--help".starts_with(option) {
                Some("-h/--help")
            } else if option.starts_with("--") && "--wordcount".starts_with(option) {
                Some("--wordcount")
            } else {
                None
            };
            if let Some(canonical) = canonical {
                return Ok(cli_parse_error(
                    &program,
                    &format!(
                        "argument {canonical}: ignored explicit argument {}",
                        provenant_core::py::python_repr(value)
                    ),
                ));
            }
            positional_closed |= !paths.is_empty();
            unknown.push(text.to_owned());
        } else if text.starts_with('-') {
            positional_closed |= !paths.is_empty();
            unknown.push(text.to_owned());
        } else if positional_closed {
            unknown.push(text.to_owned());
        } else {
            paths.push(PathBuf::from(argument));
        }
    }

    if paths.is_empty() {
        return Ok(cli_parse_error(
            &program,
            "the following arguments are required: paths",
        ));
    }
    if !unknown.is_empty() {
        return Ok(cli_parse_error(
            &program,
            &format!("unrecognized arguments: {}", unknown.join(" ")),
        ));
    }

    let mut stdout = String::new();
    if wordcount {
        let mut missing = false;
        for path in paths {
            if !path.exists() {
                stdout.push_str(&format!("{}: missing\n", path.display()));
                missing = true;
                continue;
            }
            let count = prose_word_count(&path, latex)?;
            stdout.push_str(&format!("{}: {count} prose words\n", path.display()));
        }
        return Ok(CliOutput {
            exit_code: if missing { 1 } else { 0 },
            stdout,
            stderr: String::new(),
        });
    }

    let mut any_findings = false;
    for path in paths {
        if !path.exists() {
            stdout.push_str(&format!("{}: missing\n", path.display()));
            any_findings = true;
            continue;
        }
        let mut findings = scan(&path, phrases, patterns, latex)?;
        if let Some(extra_scan) = extra_scan {
            findings.extend(extra_scan.scan(&path)?);
            findings.sort();
        }
        if findings.is_empty() {
            stdout.push_str(&format!("{}: no findings\n", path.display()));
            continue;
        }
        any_findings = true;
        stdout.push_str(&format!(
            "{}: {} finding(s)\n",
            path.display(),
            findings.len()
        ));
        for (line, column, label, snippet) in findings {
            stdout.push_str(&format!(
                "  {line}:{column}: {label}: {}\n",
                provenant_core::py::python_repr(&snippet)
            ));
        }
    }
    Ok(CliOutput {
        exit_code: if any_findings { 1 } else { 0 },
        stdout,
        stderr: String::new(),
    })
}

fn cli_parse_error(program: &str, message: &str) -> CliOutput {
    CliOutput {
        exit_code: 2,
        stdout: String::new(),
        stderr: format!(
            "usage: {program} [-h] [--wordcount] paths [paths ...]\n{program}: error: {message}\n"
        ),
    }
}

fn style_help(program: &str, description: &str) -> String {
    let usage = format!("usage: {program} [-h] [--wordcount] paths [paths ...]");
    let description = wrap_description(description);
    let mut output = usage;
    if !description.is_empty() {
        output.push_str("\n\n");
        output.push_str(&description);
    }
    output.push_str(
        "\n\npositional arguments:\n  paths\n\noptions:\n  -h, --help   show this help message and exit\n  --wordcount  print the prose word count instead of style findings\n",
    );
    output
}

fn wrap_description(description: &str) -> String {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in description.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > 80 {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines.join("\n")
}

fn is_negative_number_argument(text: &str) -> bool {
    // CPython 3.14's argparse matcher is `-\\.?\\d` and accepts a numeric
    // prefix even when the rest of the positional path is not numeric.
    let Some(number) = text.strip_prefix('-') else {
        return false;
    };
    number
        .strip_prefix('.')
        .unwrap_or(number)
        .chars()
        .next()
        .is_some_and(|character| provenant_core::py::decimal_digit(character).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::collections::BTreeSet;
    use std::ffi::OsString;

    fn actual(case: &Value) -> Value {
        let text = case["text"].as_str().unwrap_or_default();
        let suffix = case["suffix"].as_str().unwrap_or(".md");
        let latex = case["latex"].as_bool().unwrap_or(false);
        match case["op"].as_str().unwrap_or("scan") {
            "spans" => json!(ignored_spans(text, suffix, latex).expect("static patterns compile")),
            "wordcount" => {
                json!(prose_word_count_text(text, suffix, latex).expect("static patterns compile"))
            }
            _ => {
                json!(scan_text(text, suffix, None, None, latex).expect("static patterns compile"))
            }
        }
    }

    fn actual_cli(case: &Value) -> Value {
        let directory = tempfile::tempdir().expect("fixture directory");
        let root = directory.path();
        if let Some(files) = case["files"].as_object() {
            for (name, text) in files {
                let path = root.join(name);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).expect("fixture parent");
                }
                std::fs::write(path, text.as_str().unwrap_or_default()).expect("fixture file");
            }
        }
        if let Some(files) = case["binary_files"].as_object() {
            for (name, contents) in files {
                let bytes = contents
                    .as_array()
                    .expect("binary fixture is a byte list")
                    .iter()
                    .map(|byte| {
                        u8::try_from(byte.as_u64().expect("binary fixture byte is unsigned"))
                            .expect("binary fixture byte is in range")
                    })
                    .collect::<Vec<_>>();
                let path = root.join(name);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).expect("fixture parent");
                }
                std::fs::write(path, bytes).expect("binary fixture file");
            }
        }
        let argv: Vec<OsString> = case["argv"]
            .as_array()
            .expect("argv list")
            .iter()
            .map(|argument| {
                let argument = argument.as_str().expect("argv string");
                if let Some(name) = argument.strip_prefix("<file>/") {
                    root.join(name).into_os_string()
                } else if let Some(name) = argument.strip_prefix("<missing>/") {
                    root.join(name).into_os_string()
                } else {
                    OsString::from(argument)
                }
            })
            .collect();
        let result = run_cli(
            &argv,
            case["description"].as_str().unwrap_or(""),
            fixture_phrases(case).as_deref(),
            fixture_patterns(case).as_deref(),
            case["latex"].as_bool().unwrap_or(false),
            None,
        );
        let result = match result {
            Ok(result) => result,
            Err(StyleError::UnicodeDecode { .. }) => {
                return json!({"error_type":"UnicodeDecodeError"});
            }
            Err(error) => panic!("fixture CLI input failed unexpectedly: {error}"),
        };
        let temporary_path = root.to_string_lossy();
        json!({
            "exit_code": result.exit_code,
            "stdout": result.stdout.replace(temporary_path.as_ref(), "<tmp>"),
            "stderr": result.stderr.replace(temporary_path.as_ref(), "<tmp>"),
        })
    }

    fn fixture_phrases(case: &Value) -> Option<Vec<PhraseGroup>> {
        case["phrases"].as_array().map(|groups| {
            groups
                .iter()
                .map(|group| {
                    PhraseGroup::new(
                        group["label"].as_str().unwrap_or_default(),
                        group["phrases"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str),
                    )
                })
                .collect()
        })
    }

    fn fixture_patterns(case: &Value) -> Option<Vec<PatternRule>> {
        case["patterns"].as_array().map(|rules| {
            rules
                .iter()
                .map(|rule| {
                    let bits = rule["flags"].as_u64().unwrap_or_default();
                    PatternRule::python_regex(
                        rule["label"].as_str().unwrap_or_default(),
                        rule["expression"].as_str().unwrap_or_default(),
                        PatternFlags {
                            case_insensitive: bits & 2 != 0,
                            multiline: bits & 8 != 0,
                            dot_all: bits & 16 != 0,
                        },
                    )
                    .expect("fixture regex uses supported Python syntax")
                })
                .collect()
        })
    }

    fn actual_overlay(case: &Value) -> Value {
        let phrases = fixture_phrases(case);
        let patterns = fixture_patterns(case);
        json!(scan_text(
            case["text"].as_str().unwrap_or_default(),
            case["suffix"].as_str().unwrap_or(".md"),
            phrases.as_deref(),
            patterns.as_deref(),
            case["latex"].as_bool().unwrap_or(false),
        )
        .expect("overlay scans"))
    }

    #[test]
    fn python_boundaries_and_lookahead_unicode_match_oracle() {
        let cases = json!([
            {"suffix":".md","text":"ırobust ſrobust Krobust İrobust highly-faſhioned highly-fashıoned"},
            {"suffix":".md","text":"\u{345}not only highly-rated\u{345} data-set\u{5b0}"}
        ]);
        let actual_results: Vec<Value> = cases.as_array().unwrap().iter().map(actual).collect();
        if let Some(expected) =
            crate::test_support::oracle_script("oracle_style_authority.py", "style_lint", &cases)
        {
            assert_eq!(json!(actual_results), expected);
        }
    }

    #[test]
    fn default_rules_and_spans_match_python_fixtures() {
        let cases = json!([
            {
                "op":"scan", "suffix":".md", "latex":false,
                "text":"The robust pipeline leverages a comprehensive, cutting-edge and holistic approach.\nOverall it behaves not only seamlessly but clearly.\n"
            },
            {
                "op":"scan", "suffix":".md", "latex":false,
                "text":"---\ntitle: robust\n---\nA robust result in `robust` [link](https://example.test/robust) and https://example.test/robust.\n```rust\nrobust\n```\n"
            },
            {
                "op":"scan", "suffix":".md", "latex":false,
                "text":"é robust cafe\u{0301} robust\nfoo-versus-bar-baz and state-of-the-art; highly-fashioned highly-fledgling.\n"
            },
            {
                "op":"scan", "suffix":".md", "latex":false,
                "text":"a\u{0301}not only; a\u{001c}robust phrase\u{001f};\n| --- | : --- : |\n"
            },
            {
                "op":"scan", "suffix":".md", "latex":false,
                "text":"in\u{001c}the\u{001f}realm of; \u{001c}| --- |\u{001f}\n"
            },
            {
                "op":"scan", "suffix":".md", "latex":false,
                "text":"éfoo-bar-baz-qux-quux; ſeamless; utilıse;\n"
            },
            {
                "op":"scan", "suffix":".md", "latex":false,
                "text":"First line\r\nrobust second line\rThird line"
            },
            {
                "op":"spans", "suffix":".md", "latex":false,
                "text":"---\ntitle: robust\n---\n`code` [label](https://example.test/a) https://example.test/b.\n```text\ncode\n```\n"
            },
            {
                "op":"scan", "suffix":".tex", "latex":true,
                "text":"This robust prose cites \\cite{robust}. \\textbf{robust} stays prose. $robust$ and \\(robust\\) survive nowhere.\n\\begin{verbatim}\nrobust\n\\end{verbatim}\n50% robust % robust\n\\verb|robust| robust.\n\\begin{align*}robust\\end{align*}\n"
            },
            {
                "op":"spans", "suffix":".tex", "latex":true,
                "text":"\\cite{robust} \\textbf{robust} \\href{https://example.test}{label} 50% first % second \\verb|code| $math$ \\begin{align*}x\\end{align*}\n"
            },
            {
                "op":"wordcount", "suffix":".md", "latex":false,
                "text":"A don't-cut word-count test `hidden words` [display](https://host/hidden) and code:\n```txt\nmore hidden\n```\n"
            },
            {
                "op":"wordcount", "suffix":".tex", "latex":true,
                "text":"Count this \\cite{not counted} plus $not counted$ and \\verb|not counted|.\n"
            },
            {"op":"spans", "suffix":".tex", "latex":true, "text":"\\verb!a! \\lstinline[language=Rust]@b@ \\mintinline{c}|x|"}
        ]);
        let actual_results: Vec<Value> = cases.as_array().unwrap().iter().map(actual).collect();
        if let Some(expected) =
            crate::test_support::oracle_script("oracle_style_authority.py", "style_lint", &cases)
        {
            assert_eq!(json!(actual_results), expected);
        }
    }

    #[test]
    fn coordinates_and_boundary_helpers_use_python_character_offsets() {
        assert_eq!(line_col("😀x\nrobust", 3), (2, 1));
        assert_eq!(line_col("😀x\nrobust", 1), (1, 2));
        assert!(is_table_separator_line("| --- |\ntext", 2));
        assert!(!is_table_separator_line("| --- |\ntext", 9));
        assert!(is_ignored(1, &[(0, 2)]));
        assert!(!is_ignored(2, &[(0, 2)]));
    }

    #[test]
    fn pattern_overlay_replaces_defaults_and_phrase_group_order_is_retained() {
        let phrases = vec![
            PhraseGroup::new("second", ["robust"]),
            PhraseGroup::new("first", ["robust"]),
        ];
        let patterns = vec![PatternRule::regex("custom", "robust").expect("valid test regex")];
        let findings = scan_text("robust", ".md", Some(&phrases), Some(&patterns), false)
            .expect("valid rules");
        assert_eq!(findings.len(), 3);
        assert_eq!(findings[0].2, "custom");
        assert_eq!(findings[1].2, "first");
        assert_eq!(findings[2].2, "second");
    }

    #[test]
    fn typed_python_overlay_flags_match_the_source_subset() {
        let cases = json!([
            {
                "op":"scan", "suffix":".md", "latex":false,
                "text":"ROBUST, robustly, not robust?\n",
                "phrases":[],
                "patterns":[{"label":"custom robust", "expression":"\\brobust\\b", "flags":2}]
            }
        ]);
        let expected =
            crate::test_support::oracle_script("oracle_style_authority.py", "style_lint", &cases);
        if let Some(expected) = expected {
            let actual: Vec<Value> = cases
                .as_array()
                .expect("overlay cases")
                .iter()
                .map(actual_overlay)
                .collect();
            assert_eq!(json!(actual), expected);
        }
    }

    #[test]
    fn academic_wrapper_patterns_match_python_boundaries_and_unicode_digits() {
        let cases = json!([
            {
                "op":"scan", "suffix":".tex", "latex":true, "phrases":[],
                "text":"x---y ---- z---w ----- p---q",
                "patterns":[{
                    "label":"LaTeX prose em dash marker",
                    "expression":"(?<!-)---(?!-)"
                }]
            },
            {
                "op":"scan", "suffix":".tex", "latex":true, "phrases":[],
                "text":"5% \\5% \\\\5% １２% ١٢%",
                "patterns":[{
                    "label":"Likely unescaped percent in prose",
                    "expression":r"(?<!\\)\d+%"
                }]
            }
        ]);
        let expected =
            crate::test_support::oracle_script("oracle_style_authority.py", "style_lint", &cases);
        if let Some(expected) = expected {
            let actual: Vec<Value> = cases
                .as_array()
                .expect("academic overlay cases")
                .iter()
                .map(actual_overlay)
                .collect();
            assert_eq!(json!(actual), expected);
        }
    }

    #[test]
    fn bad_path_is_a_typed_error() {
        let result = scan("/definitely/missing/style-lint-input.md", None, None, false);
        assert!(matches!(result, Err(StyleError::Read { .. })));
    }

    #[test]
    fn shared_cli_matches_python_outputs_and_statuses() {
        let cases = json!([
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "<file>/report.md"],
                "files":{"report.md":"A robust result.\n"}
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "--wordcount", "<file>/report.md"],
                "files":{"report.md":"Count these words and `hide these`.\n"}
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "<file>/first.md", "<file>/second.md", "--wordcount", "--wordcount"],
                "files":{"first.md":"Count the first file.\n", "second.md":"Count the second file.\n"}
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "<file>/first.md", "--wordcount", "<file>/second.md"],
                "files":{"first.md":"Count the first file.\n", "second.md":"Count the second file.\n"}
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "<missing>/gone.md"],
                "files":{}
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "--wordcount", "<missing>/gone.md"],
                "files":{}
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "--help"], "files":{}
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py"], "files":{}
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "--wordcount=false", "<file>/report.md"],
                "files":{"report.md":"Count these words.\n"}
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "-1"], "files":{}
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "--wordcount", "-.5"], "files":{}
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "-1e3"], "files":{}
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "-٠١"], "files":{}
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "--wo", "<file>/report.md"],
                "files":{"report.md":"Count these words.\n"}
            },
            {
                "op":"cli", "description":"one\nline\n\nanother paragraph with more words.",
                "argv":["skills/natural-writing/scripts/style_lint.py", "--h"]
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "-hfoo"]
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "--", "<file>/--wordcount"],
                "files":{"--wordcount":"A robust result.\n"}
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "--unknown"]
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "--=value"]
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "--unknown", "<file>/report.md"],
                "files":{"report.md":"Plain prose.\n"}
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "<file>/report.md", "--unknown", "<file>/second.md"],
                "files":{"report.md":"Plain prose.\n", "second.md":"Still unrecognised.\n"}
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "<file>/résumé.md", "--wordc"],
                "files":{"résumé.md":"Count these words.\n"}
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "<file>/report.md"],
                "files":{"report.md":"A robust result.\n"},
                "phrases":[{"label":"custom phrase", "phrases":["robust"]}],
                "patterns":[]
            },
            {
                "op":"cli", "description":"Small command.",
                "argv":["style_lint.py", "<file>/broken.md"],
                "binary_files":{"broken.md":[255]}
            }
        ]);
        let expected =
            crate::test_support::oracle_script("oracle_style_authority.py", "style_lint", &cases);
        if let Some(expected) = expected {
            let actual: Vec<Value> = cases
                .as_array()
                .expect("CLI cases")
                .iter()
                .map(actual_cli)
                .collect();
            let expected = expected.as_array().expect("oracle CLI result list");
            let cases = cases.as_array().expect("CLI case list");
            for (case, (actual, expected)) in cases.iter().zip(actual.iter().zip(expected.iter())) {
                if has_help_shape(actual) || has_help_shape(expected) {
                    assert_eq!(help_shape(actual), help_shape(expected));
                } else {
                    assert_eq!(actual, expected, "CLI case: {case}");
                }
            }
        }
    }

    fn has_help_shape(value: &Value) -> bool {
        value["stdout"]
            .as_str()
            .is_some_and(|output| output.contains("options:") && output.contains("paths"))
    }

    fn help_shape(value: &Value) -> Value {
        let output = value["stdout"].as_str().unwrap_or_default();
        let options = output
            .lines()
            .skip_while(|line| line.trim() != "options:")
            .skip(1)
            .take_while(|line| !line.trim().is_empty())
            .flat_map(|line| line.split_whitespace())
            .map(|word| word.trim_end_matches(','))
            .filter(|word| word.starts_with('-'))
            .collect::<BTreeSet<_>>();
        let positionals = output.contains("positional arguments:") && output.contains("  paths");
        json!({
            "exit_code": value["exit_code"],
            "positionals": if positionals { vec!["paths"] } else { Vec::new() },
            "options": options,
        })
    }

    #[test]
    fn cli_extra_scan_is_an_explicit_extension_seam() {
        let directory = tempfile::tempdir().expect("fixture directory");
        let path = directory.path().join("report.md");
        std::fs::write(&path, "Plain prose.\n").expect("fixture text");
        let scanner =
            |_path: &Path| Ok(vec![(2, 1, "domain rule".to_owned(), "special".to_owned())]);
        let result = run_cli(
            &[
                OsString::from("style_lint.py"),
                path.clone().into_os_string(),
            ],
            "Small command.",
            None,
            None,
            false,
            Some(&scanner),
        )
        .expect("fixture CLI scan succeeds");
        assert_eq!(result.exit_code, 1);
        assert!(result.stdout.contains("2:1: domain rule: 'special'"));
    }
}
