//! Bounded skill-owned prose rules. Findings are evidence, not filing approval.
use provenant_supervisor::fs::FsError;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use thiserror::Error;
mod files;
mod spans;

pub const MAX_RULE_BYTES: usize = 64 * 1024;
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MAX_RULES: usize = 256;
const MAX_FINDINGS: usize = 65_536;
#[derive(Debug, Error)]
pub enum WritingError {
    #[error("invalid writing input: {0}")]
    Invalid(&'static str),
    #[error("writing rules exceed their size or complexity bound")]
    RulesLimit,
    #[error("invalid or unsupported writing rule data")]
    Rules,
    #[error("writing evidence exceeds its size or count bound")]
    EvidenceLimit,
    #[error("writing evidence is not UTF-8")]
    Encoding,
    #[error("writing evidence is not a regular confined file")]
    Evidence,
    #[error(transparent)]
    Io(#[from] FsError),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleSet {
    General,
    Academic,
    Legal,
}
impl RuleSet {
    fn name(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Academic => "academic",
            Self::Legal => "legal",
        }
    }
    fn asset(self) -> &'static str {
        match self {
            Self::Legal => "skills/legal-writing/lint-rules.json",
            _ => "skills/natural-writing/lint-rules.json",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Warning,
    Blocker,
}
#[derive(Debug, Serialize)]
pub struct Finding {
    pub rule: String,
    pub severity: Severity,
    pub line: usize,
    pub column: usize,
    pub start: usize,
    pub end: usize,
    pub message: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    version: u64,
    exclusions: Exclusions,
    shared: Vec<Rule>,
    sets: BTreeMap<String, Set>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Set {
    latex: bool,
    #[serde(default)]
    quoted_example_paths: Vec<String>,
    #[serde(default)]
    skip_files: Vec<String>,
    rules: Vec<Rule>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Exclusions {
    regex: Vec<String>,
    latex_commands: Vec<String>,
    latex_environments: Vec<String>,
    verbatim_commands: Vec<String>,
    math_delimiters: Vec<[String; 2]>,
}
#[derive(Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Literal,
    Regex,
    CommentRegex,
    HeadingGap,
    SentenceWords,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    id: String,
    message: String,
    severity: Severity,
    kind: Kind,
    #[serde(default)]
    pattern: String,
    #[serde(default)]
    case_insensitive: bool,
    #[serde(default)]
    word_boundary: bool,
    #[serde(default)]
    except_matches: Vec<String>,
    #[serde(default)]
    reject_adjacent: Option<char>,
    #[serde(default)]
    words: usize,
}
struct CompiledRule {
    data: Rule,
    pattern: Option<Regex>,
    exceptions: Vec<Regex>,
}
pub struct Matcher {
    rules: Vec<CompiledRule>,
    exclusions: Exclusions,
    excluded_patterns: Vec<Regex>,
    latex: bool,
    quoted_paths: Vec<String>,
    skip_files: Vec<String>,
}
fn compile(pattern: &str, insensitive: bool) -> Result<Regex, WritingError> {
    if pattern.len() > 4096 {
        return Err(WritingError::RulesLimit);
    }
    RegexBuilder::new(pattern)
        .case_insensitive(insensitive)
        .multi_line(true)
        .size_limit(1024 * 1024)
        .dfa_size_limit(1024 * 1024)
        .nest_limit(64)
        .build()
        .map_err(|_| WritingError::Rules)
}
impl Matcher {
    /// Compile once per inspection; no filesystem or process access during scan.
    pub fn from_json(bytes: &[u8], selected: RuleSet) -> Result<Self, WritingError> {
        if bytes.len() > MAX_RULE_BYTES {
            return Err(WritingError::RulesLimit);
        }
        let mut bundle: Bundle = serde_json::from_slice(bytes).map_err(|_| WritingError::Rules)?;
        if bundle.version != 1 || bundle.sets.len() > 3 {
            return Err(WritingError::Rules);
        }
        let set = bundle
            .sets
            .remove(selected.name())
            .ok_or(WritingError::Rules)?;
        let all_rules = bundle
            .shared
            .into_iter()
            .chain(set.rules)
            .collect::<Vec<_>>();
        if all_rules.is_empty() || all_rules.len() > MAX_RULES {
            return Err(WritingError::RulesLimit);
        }
        let ex = &bundle.exclusions;
        if ex.regex.len() > 32
            || ex.latex_commands.len() > 128
            || ex.latex_environments.len() > 32
            || ex.verbatim_commands.len() > 32
            || ex.math_delimiters.len() > 16
            || set.quoted_example_paths.len() > 32
            || set.skip_files.len() > 32
        {
            return Err(WritingError::RulesLimit);
        }
        if ex
            .latex_commands
            .iter()
            .chain(&ex.latex_environments)
            .chain(&ex.verbatim_commands)
            .any(|s| s.is_empty() || s.len() > 64 || !s.chars().all(|c| c.is_ascii_alphabetic()))
            || ex
                .math_delimiters
                .iter()
                .flatten()
                .any(|s| s.is_empty() || s.len() > 16)
            || set
                .quoted_example_paths
                .iter()
                .any(|s| s.is_empty() || s.starts_with('/') || s.split('/').any(|s| s == ".."))
        {
            return Err(WritingError::Rules);
        }
        let excluded_patterns = ex
            .regex
            .iter()
            .map(|s| compile(s, false))
            .collect::<Result<_, _>>()?;
        let mut ids = BTreeSet::new();
        let mut rules = Vec::new();
        for data in all_rules {
            if data.id.is_empty()
                || data.id.len() > 80
                || !data
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c))
                || !ids.insert(data.id.clone())
                || data.message.is_empty()
                || data.message.len() > 512
                || data.except_matches.len() > 16
            {
                return Err(WritingError::Rules);
            }
            let pattern = match data.kind {
                Kind::Literal if !data.pattern.trim().is_empty() => Some(compile(
                    &data
                        .pattern
                        .split_whitespace()
                        .map(regex::escape)
                        .collect::<Vec<_>>()
                        .join(r"\s+"),
                    data.case_insensitive,
                )?),
                Kind::Regex | Kind::CommentRegex if !data.pattern.is_empty() => {
                    Some(compile(&data.pattern, data.case_insensitive)?)
                }
                Kind::HeadingGap if data.pattern.is_empty() => None,
                Kind::SentenceWords
                    if data.pattern.is_empty() && (1..=1000).contains(&data.words) =>
                {
                    None
                }
                _ => return Err(WritingError::Rules),
            };
            if pattern.as_ref().is_some_and(|p| p.is_match("")) {
                return Err(WritingError::Rules);
            }
            let exceptions = data
                .except_matches
                .iter()
                .map(|s| compile(s, false))
                .collect::<Result<_, _>>()?;
            rules.push(CompiledRule {
                data,
                pattern,
                exceptions,
            });
        }
        Ok(Self {
            rules,
            exclusions: bundle.exclusions,
            excluded_patterns,
            latex: set.latex,
            quoted_paths: set.quoted_example_paths,
            skip_files: set.skip_files,
        })
    }
    pub fn scan(&self, path: &Path, text: &str) -> Result<Vec<Finding>, WritingError> {
        if text.len() > MAX_TEXT_BYTES {
            return Err(WritingError::EvidenceLimit);
        }
        let path = path
            .to_str()
            .ok_or(WritingError::Invalid("path is not UTF-8"))?;
        if self.skip_files.iter().any(|s| {
            Path::new(path)
                .file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case(s))
        }) {
            return Ok(Vec::new());
        }
        let quoted = self.quoted_paths.iter().any(|p| path.starts_with(p));
        let latex = self.latex
            && matches!(
                Path::new(path).extension().and_then(|x| x.to_str()),
                Some("tex" | "latex" | "sty" | "cls")
            );
        let excluded = spans::excluded(self, text, latex, quoted);
        let mut findings = Vec::new();
        let line_starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(i, _)| i + 1))
            .collect::<Vec<_>>();
        for rule in &self.rules {
            let mut hits = Vec::new();
            match rule.data.kind {
                Kind::HeadingGap => {
                    let mut previous = "";
                    let mut offset = 0;
                    for line in text.split_inclusive('\n') {
                        let trimmed = line.trim_start_matches(' ');
                        let hashes = trimmed.bytes().take_while(|b| *b == b'#').count();
                        if line.len() - trimmed.len() <= 3
                            && (1..=6).contains(&hashes)
                            && trimmed
                                .as_bytes()
                                .get(hashes)
                                .is_some_and(u8::is_ascii_whitespace)
                            && !previous.trim().is_empty()
                        {
                            hits.push(
                                offset + line.len() - trimmed.len()
                                    ..offset + line.len() - trimmed.len() + hashes,
                            );
                        }
                        previous = line;
                        offset += line.len();
                    }
                }
                Kind::SentenceWords => {
                    hits = spans::long_sentences(text, &excluded, rule.data.words)
                }
                Kind::CommentRegex if quoted => {}
                Kind::CommentRegex => {
                    let mut offset = 0;
                    while let Some(start) = text[offset..].find("<!--") {
                        let start = offset + start + 4;
                        let end = text[start..].find("-->").map_or(text.len(), |i| start + i);
                        if let Some(pattern) = &rule.pattern {
                            hits.extend(
                                pattern
                                    .find_iter(&text[start..end])
                                    .map(|m| start + m.start()..start + m.end()),
                            );
                        }
                        offset = (end + 3).min(text.len());
                    }
                }
                _ => {
                    if let Some(pattern) = &rule.pattern {
                        hits.extend(pattern.find_iter(text).map(|m| m.range()));
                    }
                }
            }
            for range in hits {
                if rule.data.kind != Kind::SentenceWords
                    && excluded[range.clone()].iter().any(|b| *b)
                {
                    continue;
                }
                let matched = &text[range.clone()];
                if rule.exceptions.iter().any(|p| p.is_match(matched)) {
                    continue;
                }
                let before = text[..range.start].chars().next_back();
                let after = text[range.end..].chars().next();
                if rule.data.word_boundary
                    && (before.is_some_and(|c| c.is_ascii_alphabetic())
                        || after.is_some_and(|c| c.is_ascii_alphabetic()))
                {
                    continue;
                }
                if rule
                    .data
                    .reject_adjacent
                    .is_some_and(|c| before == Some(c) || after == Some(c))
                {
                    continue;
                }
                if findings.len() >= MAX_FINDINGS {
                    return Err(WritingError::EvidenceLimit);
                }
                let index = line_starts.partition_point(|start| *start <= range.start) - 1;
                findings.push(Finding {
                    rule: rule.data.id.clone(),
                    severity: rule.data.severity,
                    line: index + 1,
                    column: text[line_starts[index]..range.start].chars().count() + 1,
                    start: range.start,
                    end: range.end,
                    message: rule.data.message.clone(),
                });
            }
        }
        findings.sort_by(|a, b| (a.start, a.end, &a.rule).cmp(&(b.start, b.end, &b.rule)));
        Ok(findings)
    }
}
/// Evidence bytes bind pagination to exactly the inspected rules and files.
pub struct Inspection {
    pub findings: Vec<FileFinding>,
    pub evidence: Vec<(PathBuf, Vec<u8>)>,
}
#[derive(Serialize)]
pub struct FileFinding {
    pub path: PathBuf,
    #[serde(flatten)]
    pub finding: Finding,
}
pub use files::inspect;
#[cfg(test)]
mod tests;
