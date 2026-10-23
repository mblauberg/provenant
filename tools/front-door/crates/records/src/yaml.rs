use std::collections::BTreeMap;
use thiserror::Error;

const MAX_BYTES: usize = 256 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpannedYaml {
    pub value: YamlValue,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum YamlValue {
    String(String),
    Boolean(bool),
    Sequence(Vec<SpannedYaml>),
    Mapping(BTreeMap<String, SpannedYaml>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YamlErrorKind {
    MissingFrontmatter,
    UnclosedFrontmatter,
    InvalidSyntax,
    InvalidIndentation,
    DuplicateKey,
    AmbiguousScalar,
    Anchor,
    Alias,
    Tag,
    FlowMap,
    MultipleDocuments,
    Tabs,
    MultilinePlainScalar,
    UnsupportedEscape,
    LimitExceeded,
}

impl std::fmt::Display for YamlErrorKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::MissingFrontmatter => "missing frontmatter",
            Self::UnclosedFrontmatter => "unclosed frontmatter",
            Self::InvalidSyntax => "invalid syntax",
            Self::InvalidIndentation => "invalid indentation",
            Self::DuplicateKey => "duplicate key",
            Self::AmbiguousScalar => "ambiguous scalar; quote this value",
            Self::Anchor => "anchor is not supported",
            Self::Alias => "alias is not supported",
            Self::Tag => "tag is not supported",
            Self::FlowMap => "flow map is not supported",
            Self::MultipleDocuments => "multiple documents are not supported",
            Self::Tabs => "tab indentation is not supported",
            Self::MultilinePlainScalar => "multiline plain scalar is not supported",
            Self::UnsupportedEscape => "unsupported quoted-scalar escape",
            Self::LimitExceeded => "YAML limit exceeded",
        };
        formatter.write_str(label)
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("YAML {kind} at line {line}, column {column}")]
pub struct YamlError {
    pub kind: YamlErrorKind,
    pub line: usize,
    pub column: usize,
}

/// Parse the bounded YAML frontmatter at the beginning of a skill file.
/// Everything after the closing delimiter is ignored.
pub fn parse_frontmatter(source: &str) -> Result<SpannedYaml, YamlError> {
    let mut lines = source.split('\n').enumerate();
    let Some((_, first)) = lines.next() else {
        return Err(error(YamlErrorKind::MissingFrontmatter, 1, 1));
    };
    if without_cr(first) != "---" {
        return Err(error(YamlErrorKind::MissingFrontmatter, 1, 1));
    }
    let mut body = String::new();
    let mut closing_line = None;
    for (index, raw) in lines {
        let line = without_cr(raw);
        if line == "---" {
            closing_line = Some(index + 1);
            break;
        }
        if body.len().saturating_add(raw.len()).saturating_add(1) > MAX_BYTES {
            return Err(error(YamlErrorKind::LimitExceeded, index + 2, 1));
        }
        body.push_str(raw);
        body.push('\n');
    }
    if closing_line.is_none() {
        return Err(error(YamlErrorKind::UnclosedFrontmatter, 1, 1));
    }
    let mut parser = Parser::new(&body, 2)?;
    let parsed = parser.parse_document()?;
    if !matches!(parsed.value, YamlValue::Mapping(_)) {
        return Err(error(
            YamlErrorKind::InvalidSyntax,
            parsed.span.line,
            parsed.span.column,
        ));
    }
    Ok(parsed)
}

/// Parse one complete bounded sidecar document.
pub fn parse_sidecar(source: &str) -> Result<SpannedYaml, YamlError> {
    let mut parser = Parser::new(source, 1)?;
    parser.parse_document()
}

#[derive(Clone, Copy)]
struct SourceLine<'a> {
    raw: &'a str,
    text: &'a str,
    indent: usize,
    number: usize,
}

struct Parser<'a> {
    lines: Vec<SourceLine<'a>>,
    index: usize,
    nodes: usize,
    first_line: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str, first_line: usize) -> Result<Self, YamlError> {
        if source.len() > MAX_BYTES {
            return Err(error(YamlErrorKind::LimitExceeded, first_line, 1));
        }
        if source.starts_with('\u{feff}') {
            return Err(error(YamlErrorKind::InvalidSyntax, first_line, 1));
        }
        let mut lines = Vec::new();
        for (offset, raw_with_cr) in source.split('\n').enumerate() {
            let raw = without_cr(raw_with_cr);
            if let Some(column) = raw.as_bytes().iter().position(|byte| *byte == b'\t') {
                return Err(error(YamlErrorKind::Tabs, first_line + offset, column + 1));
            }
            let indent = raw.bytes().take_while(|byte| *byte == b' ').count();
            lines.push(SourceLine {
                raw,
                text: raw.get(indent..).map_or("", |value| value),
                indent,
                number: first_line + offset,
            });
        }
        Ok(Self {
            lines,
            index: 0,
            nodes: 0,
            first_line,
        })
    }

    fn parse_document(&mut self) -> Result<SpannedYaml, YamlError> {
        self.skip_trivia();
        let Some(first) = self.lines.get(self.index).copied() else {
            return Err(error(YamlErrorKind::InvalidSyntax, self.first_line, 1));
        };
        if is_document_marker(first) {
            return Err(error(
                YamlErrorKind::MultipleDocuments,
                first.number,
                first.indent + 1,
            ));
        }
        let value = self.parse_block(first.indent, 0)?;
        self.skip_trivia();
        if let Some(line) = self.lines.get(self.index).copied() {
            let kind = if is_document_marker(line) {
                YamlErrorKind::MultipleDocuments
            } else {
                YamlErrorKind::InvalidIndentation
            };
            return Err(error(kind, line.number, line.indent + 1));
        }
        Ok(value)
    }

    fn parse_block(&mut self, indent: usize, depth: usize) -> Result<SpannedYaml, YamlError> {
        self.check_depth(depth)?;
        self.skip_trivia();
        let Some(line) = self.lines.get(self.index).copied() else {
            return Err(error(YamlErrorKind::InvalidSyntax, self.first_line, 1));
        };
        if line.indent != indent {
            return Err(error(
                YamlErrorKind::InvalidIndentation,
                line.number,
                line.indent + 1,
            ));
        }
        if is_sequence_line(line.text) {
            self.parse_sequence(indent, depth, line)
        } else if split_pair(line.text).is_some() {
            self.parse_mapping(indent, depth, line)
        } else {
            self.index += 1;
            self.node(line.number, indent + 1)?;
            let value = self.parse_scalar(
                line.text,
                Span {
                    line: line.number,
                    column: indent + 1,
                },
                depth,
            )?;
            Ok(SpannedYaml {
                value,
                span: Span {
                    line: line.number,
                    column: indent + 1,
                },
            })
        }
    }

    fn parse_mapping(
        &mut self,
        indent: usize,
        depth: usize,
        first: SourceLine<'a>,
    ) -> Result<SpannedYaml, YamlError> {
        self.node(first.number, indent + 1)?;
        let root_span = Span {
            line: first.number,
            column: indent + 1,
        };
        let mut entries = BTreeMap::new();
        loop {
            self.skip_trivia();
            let Some(line) = self.lines.get(self.index).copied() else {
                break;
            };
            if line.indent < indent || (line.indent == indent && is_sequence_line(line.text)) {
                break;
            }
            if is_document_marker(line) {
                return Err(error(
                    YamlErrorKind::MultipleDocuments,
                    line.number,
                    line.indent + 1,
                ));
            }
            if line.indent > indent {
                return Err(error(
                    YamlErrorKind::MultilinePlainScalar,
                    line.number,
                    line.indent + 1,
                ));
            }
            self.parse_mapping_pair(indent, depth, line, &mut entries)?;
        }
        Ok(SpannedYaml {
            value: YamlValue::Mapping(entries),
            span: root_span,
        })
    }

    fn parse_mapping_pair(
        &mut self,
        indent: usize,
        depth: usize,
        line: SourceLine<'a>,
        entries: &mut BTreeMap<String, SpannedYaml>,
    ) -> Result<(), YamlError> {
        let Some((raw_key, raw_value, value_column)) = split_pair(line.text) else {
            return Err(error(
                YamlErrorKind::InvalidSyntax,
                line.number,
                line.indent + 1,
            ));
        };
        let key_span = Span {
            line: line.number,
            column: line.indent + 1,
        };
        let key = self.parse_key(raw_key, key_span)?;
        if entries.contains_key(&key) {
            return Err(error(
                YamlErrorKind::DuplicateKey,
                line.number,
                key_span.column,
            ));
        }
        self.member(key_span)?;
        self.index += 1;
        let value = self.parse_entry_value(
            raw_value,
            indent,
            depth + 1,
            Span {
                line: line.number,
                column: line.indent + value_column,
            },
        )?;
        entries.insert(key, value);
        Ok(())
    }

    fn parse_sequence(
        &mut self,
        indent: usize,
        depth: usize,
        first: SourceLine<'a>,
    ) -> Result<SpannedYaml, YamlError> {
        self.node(first.number, indent + 1)?;
        let root_span = Span {
            line: first.number,
            column: indent + 1,
        };
        let mut values = Vec::new();
        loop {
            self.skip_trivia();
            let Some(line) = self.lines.get(self.index).copied() else {
                break;
            };
            if line.indent < indent || (line.indent == indent && !is_sequence_line(line.text)) {
                break;
            }
            if is_document_marker(line) {
                return Err(error(
                    YamlErrorKind::MultipleDocuments,
                    line.number,
                    line.indent + 1,
                ));
            }
            if line.indent > indent {
                return Err(error(
                    YamlErrorKind::MultilinePlainScalar,
                    line.number,
                    line.indent + 1,
                ));
            }
            let Some(rest) = sequence_rest(line.text) else {
                break;
            };
            self.member(Span {
                line: line.number,
                column: line.indent + 1,
            })?;
            self.index += 1;
            let item_span = Span {
                line: line.number,
                column: line.indent + 3,
            };
            if rest.trim().is_empty() {
                let child_indent = self.next_child_indent(indent, line.number, item_span)?;
                values.push(self.parse_block(child_indent, depth + 1)?);
            } else if let Some((key_text, value_text, value_column)) = split_pair(rest) {
                let map_indent = indent.saturating_add(2);
                let key = self.parse_key(key_text, item_span)?;
                let mut map = BTreeMap::new();
                self.member(item_span)?;
                let first_value = self.parse_entry_value(
                    value_text,
                    map_indent,
                    depth + 2,
                    Span {
                        line: line.number,
                        column: item_span
                            .column
                            .saturating_add(value_column)
                            .saturating_sub(1),
                    },
                )?;
                map.insert(key, first_value);
                self.parse_mapping_continuation(map_indent, depth + 1, line.number, &mut map)?;
                values.push(SpannedYaml {
                    value: YamlValue::Mapping(map),
                    span: item_span,
                });
            } else {
                let value = self.parse_entry_value(rest, indent, depth + 1, item_span)?;
                values.push(value);
            }
        }
        Ok(SpannedYaml {
            value: YamlValue::Sequence(values),
            span: root_span,
        })
    }

    fn parse_mapping_continuation(
        &mut self,
        indent: usize,
        depth: usize,
        parent_line: usize,
        entries: &mut BTreeMap<String, SpannedYaml>,
    ) -> Result<(), YamlError> {
        loop {
            self.skip_trivia();
            let Some(line) = self.lines.get(self.index).copied() else {
                return Ok(());
            };
            if line.indent < indent || (line.indent == indent && is_sequence_line(line.text)) {
                return Ok(());
            }
            if line.indent > indent {
                return Err(error(
                    YamlErrorKind::MultilinePlainScalar,
                    line.number,
                    line.indent + 1,
                ));
            }
            if split_pair(line.text).is_none() {
                return Err(error(
                    YamlErrorKind::InvalidSyntax,
                    line.number,
                    line.indent + 1,
                ));
            }
            self.parse_mapping_pair(indent, depth, line, entries)
                .map_err(|failure| {
                    if failure.kind == YamlErrorKind::InvalidIndentation {
                        error(failure.kind, parent_line, indent + 1)
                    } else {
                        failure
                    }
                })?;
        }
    }

    fn parse_entry_value(
        &mut self,
        raw: &str,
        parent_indent: usize,
        depth: usize,
        span: Span,
    ) -> Result<SpannedYaml, YamlError> {
        let without_comment = strip_comment(raw);
        let value_text = without_comment.trim();
        if value_text.is_empty() {
            let child_indent = self.next_child_indent(parent_indent, span.line, span)?;
            return self.parse_block(child_indent, depth);
        }
        if matches!(value_text, "|" | "|-" | ">" | ">-") {
            let value = self.parse_block_scalar(parent_indent, value_text, span)?;
            return Ok(SpannedYaml {
                value: YamlValue::String(value),
                span,
            });
        }
        self.node(span.line, span.column)?;
        let value = self.parse_scalar(value_text, span, depth)?;
        Ok(SpannedYaml { value, span })
    }

    fn parse_scalar(
        &mut self,
        text: &str,
        span: Span,
        depth: usize,
    ) -> Result<YamlValue, YamlError> {
        self.check_depth(depth)?;
        let text = strip_comment(text).trim();
        if text.starts_with('{') {
            return Err(error(YamlErrorKind::FlowMap, span.line, span.column));
        }
        if text.starts_with('[') {
            return self.parse_flow_sequence(text, span, depth);
        }
        if text.starts_with('&') {
            return Err(error(YamlErrorKind::Anchor, span.line, span.column));
        }
        if text.starts_with('*') {
            return Err(error(YamlErrorKind::Alias, span.line, span.column));
        }
        if text.starts_with('!') {
            return Err(error(YamlErrorKind::Tag, span.line, span.column));
        }
        if text.starts_with('"') || text.starts_with('\'') {
            let (value, consumed) = parse_quoted(text, span)?;
            if !text
                .get(consumed..)
                .map_or("", |value| value)
                .trim()
                .is_empty()
            {
                return Err(error(
                    YamlErrorKind::InvalidSyntax,
                    span.line,
                    span.column + consumed,
                ));
            }
            return Ok(YamlValue::String(value));
        }
        if contains_yaml_directive_token(text) {
            let kind = if text.split_whitespace().any(|part| part.starts_with('&')) {
                YamlErrorKind::Anchor
            } else if text.split_whitespace().any(|part| part.starts_with('*')) {
                YamlErrorKind::Alias
            } else {
                YamlErrorKind::Tag
            };
            return Err(error(kind, span.line, span.column));
        }
        if text == "true" {
            return Ok(YamlValue::Boolean(true));
        }
        if text == "false" {
            return Ok(YamlValue::Boolean(false));
        }
        if ambiguous_word(text) || numeric_looking(text) || text == "~" {
            return Err(error(
                YamlErrorKind::AmbiguousScalar,
                span.line,
                span.column,
            ));
        }
        if text.is_empty() || text == "---" || text == "..." {
            return Err(error(
                YamlErrorKind::MultipleDocuments,
                span.line,
                span.column,
            ));
        }
        if text.starts_with('|') || text.starts_with('>') {
            return Err(error(YamlErrorKind::InvalidSyntax, span.line, span.column));
        }
        Ok(YamlValue::String(text.to_owned()))
    }

    fn parse_flow_sequence(
        &mut self,
        text: &str,
        span: Span,
        depth: usize,
    ) -> Result<YamlValue, YamlError> {
        self.check_depth(depth)?;
        self.node(span.line, span.column)?;
        let bytes = text.as_bytes();
        let mut position = 1usize;
        let mut values = Vec::new();
        loop {
            skip_spaces(bytes, &mut position);
            if bytes.get(position) == Some(&b']') {
                position += 1;
                break;
            }
            let Some(quote) = bytes.get(position).copied() else {
                return Err(error(
                    YamlErrorKind::InvalidSyntax,
                    span.line,
                    span.column + position,
                ));
            };
            if quote != b'\'' && quote != b'"' {
                return Err(error(
                    YamlErrorKind::InvalidSyntax,
                    span.line,
                    span.column + position,
                ));
            }
            let item_span = Span {
                line: span.line,
                column: span.column + position,
            };
            let suffix = text.get(position..).map_or("", |value| value);
            let (value, consumed) = parse_quoted(suffix, item_span)?;
            values.push(SpannedYaml {
                value: YamlValue::String(value),
                span: item_span,
            });
            self.member(item_span)?;
            position = position.saturating_add(consumed);
            skip_spaces(bytes, &mut position);
            match bytes.get(position) {
                Some(b',') => {
                    position += 1;
                    skip_spaces(bytes, &mut position);
                    if bytes.get(position) == Some(&b']') {
                        position += 1;
                        break;
                    }
                }
                Some(b']') => {
                    position += 1;
                    break;
                }
                _ => {
                    return Err(error(
                        YamlErrorKind::InvalidSyntax,
                        span.line,
                        span.column + position,
                    ))
                }
            }
        }
        if !text
            .get(position..)
            .map_or("", |value| value)
            .trim()
            .is_empty()
        {
            return Err(error(
                YamlErrorKind::InvalidSyntax,
                span.line,
                span.column + position,
            ));
        }
        Ok(YamlValue::Sequence(values))
    }

    fn parse_key(&mut self, raw: &str, span: Span) -> Result<String, YamlError> {
        let text = strip_comment(raw).trim();
        if text.is_empty() {
            return Err(error(YamlErrorKind::InvalidSyntax, span.line, span.column));
        }
        if text.starts_with('{') {
            return Err(error(YamlErrorKind::FlowMap, span.line, span.column));
        }
        if text.starts_with('&') {
            return Err(error(YamlErrorKind::Anchor, span.line, span.column));
        }
        if text.starts_with('*') {
            return Err(error(YamlErrorKind::Alias, span.line, span.column));
        }
        if text.starts_with('!') {
            return Err(error(YamlErrorKind::Tag, span.line, span.column));
        }
        if text.starts_with('"') || text.starts_with('\'') {
            let (key, consumed) = parse_quoted(text, span)?;
            if !text
                .get(consumed..)
                .map_or("", |value| value)
                .trim()
                .is_empty()
            {
                return Err(error(
                    YamlErrorKind::InvalidSyntax,
                    span.line,
                    span.column + consumed,
                ));
            }
            return Ok(key);
        }
        if contains_yaml_directive_token(text) {
            return Err(error(YamlErrorKind::Anchor, span.line, span.column));
        }
        if ambiguous_word(text) || numeric_looking(text) || text == "~" {
            return Err(error(
                YamlErrorKind::AmbiguousScalar,
                span.line,
                span.column,
            ));
        }
        Ok(text.to_owned())
    }

    fn parse_block_scalar(
        &mut self,
        parent_indent: usize,
        indicator: &str,
        span: Span,
    ) -> Result<String, YamlError> {
        self.node(span.line, span.column)?;
        let folded = indicator.starts_with('>');
        let strip = indicator.ends_with('-');
        let mut content = Vec::new();
        let mut content_indent: Option<usize> = None;
        while let Some(line) = self.lines.get(self.index).copied() {
            let blank = line.raw.trim().is_empty();
            if !blank && line.indent <= parent_indent {
                break;
            }
            if !blank {
                match content_indent {
                    None => content_indent = Some(line.indent),
                    Some(expected) if line.indent < expected => {
                        return Err(error(
                            YamlErrorKind::InvalidIndentation,
                            line.number,
                            line.indent + 1,
                        ));
                    }
                    _ => {}
                }
            }
            let text = if blank {
                ""
            } else {
                let trim = content_indent.map_or(line.indent, |value| value);
                line.raw.get(trim..).map_or("", |value| value)
            };
            let more_indented = !blank && text.starts_with(' ');
            content.push((text.to_owned(), more_indented));
            self.index += 1;
        }
        let mut output = String::new();
        for (index, (line, more_indented)) in content.iter().enumerate() {
            if index > 0 {
                let (previous, previous_more_indented) = content
                    .get(index - 1)
                    .map_or(("", false), |(previous, indented)| {
                        (previous.as_str(), *indented)
                    });
                if folded
                    && !previous.is_empty()
                    && !line.is_empty()
                    && !previous_more_indented
                    && !more_indented
                {
                    output.push(' ');
                } else {
                    output.push('\n');
                }
            }
            output.push_str(line);
        }
        if strip {
            while output.ends_with('\n') {
                output.pop();
            }
        } else if !content.is_empty() {
            while output.ends_with('\n') {
                output.pop();
            }
            output.push('\n');
        }
        Ok(output)
    }

    fn next_child_indent(
        &mut self,
        parent_indent: usize,
        line: usize,
        span: Span,
    ) -> Result<usize, YamlError> {
        self.skip_trivia();
        match self.lines.get(self.index).copied() {
            Some(child) if child.indent > parent_indent => Ok(child.indent),
            Some(_) | None => Err(error(YamlErrorKind::AmbiguousScalar, line, span.column)),
        }
    }

    fn skip_trivia(&mut self) {
        while let Some(line) = self.lines.get(self.index).copied() {
            let text = line.text.trim_start();
            if text.is_empty() || text.starts_with('#') {
                self.index += 1;
            } else {
                break;
            }
        }
    }

    fn check_depth(&self, depth: usize) -> Result<(), YamlError> {
        if depth > MAX_DEPTH {
            let span = self.lines.get(self.index).map_or(
                Span {
                    line: self.first_line,
                    column: 1,
                },
                |line| Span {
                    line: line.number,
                    column: line.indent + 1,
                },
            );
            Err(error(YamlErrorKind::LimitExceeded, span.line, span.column))
        } else {
            Ok(())
        }
    }

    fn node(&mut self, line: usize, column: usize) -> Result<(), YamlError> {
        self.nodes = self.nodes.saturating_add(1);
        if self.nodes > MAX_NODES {
            Err(error(YamlErrorKind::LimitExceeded, line, column))
        } else {
            Ok(())
        }
    }

    fn member(&mut self, span: Span) -> Result<(), YamlError> {
        self.node(span.line, span.column)
    }
}

fn parse_quoted(text: &str, span: Span) -> Result<(String, usize), YamlError> {
    let Some(quote) = text.as_bytes().first().copied() else {
        return Err(error(YamlErrorKind::InvalidSyntax, span.line, span.column));
    };
    if quote == b'\'' {
        parse_single_quoted(text, span)
    } else if quote == b'"' {
        parse_double_quoted(text, span)
    } else {
        Err(error(YamlErrorKind::InvalidSyntax, span.line, span.column))
    }
}

fn parse_single_quoted(text: &str, span: Span) -> Result<(String, usize), YamlError> {
    let mut output = String::new();
    let mut position = 1usize;
    while let Some(byte) = text.as_bytes().get(position).copied() {
        if byte == b'\'' {
            if text.as_bytes().get(position + 1) == Some(&b'\'') {
                output.push('\'');
                position += 2;
            } else {
                return Ok((output, position + 1));
            }
        } else {
            let Some(character) = text.get(position..).and_then(|tail| tail.chars().next()) else {
                return Err(error(
                    YamlErrorKind::InvalidSyntax,
                    span.line,
                    span.column + position,
                ));
            };
            output.push(character);
            position += character.len_utf8();
        }
    }
    Err(error(YamlErrorKind::InvalidSyntax, span.line, span.column))
}

fn parse_double_quoted(text: &str, span: Span) -> Result<(String, usize), YamlError> {
    let mut output = String::new();
    let mut position = 1usize;
    while let Some(character) = text.get(position..).and_then(|tail| tail.chars().next()) {
        match character {
            '"' => return Ok((output, position + 1)),
            '\\' => {
                position += 1;
                let Some(escape) = text.get(position..).and_then(|tail| tail.chars().next()) else {
                    return Err(error(
                        YamlErrorKind::InvalidSyntax,
                        span.line,
                        span.column + position,
                    ));
                };
                position += escape.len_utf8();
                let decoded = match escape {
                    '0' => '\0',
                    'a' => '\u{0007}',
                    'b' => '\u{0008}',
                    't' => '\t',
                    'n' => '\n',
                    'v' => '\u{000b}',
                    'f' => '\u{000c}',
                    'r' => '\r',
                    'e' => '\u{001b}',
                    ' ' => ' ',
                    '"' => '"',
                    '/' => '/',
                    '\\' => '\\',
                    'N' => '\u{0085}',
                    '_' => '\u{00a0}',
                    'L' => '\u{2028}',
                    'P' => '\u{2029}',
                    'x' => parse_hex_escape(text, &mut position, 2, span)?,
                    'u' => parse_hex_escape(text, &mut position, 4, span)?,
                    'U' => parse_hex_escape(text, &mut position, 8, span)?,
                    _ => {
                        return Err(error(
                            YamlErrorKind::UnsupportedEscape,
                            span.line,
                            span.column + position,
                        ))
                    }
                };
                output.push(decoded);
            }
            character if character <= '\u{001f}' => {
                return Err(error(
                    YamlErrorKind::InvalidSyntax,
                    span.line,
                    span.column + position,
                ));
            }
            character => {
                output.push(character);
                position += character.len_utf8();
            }
        }
    }
    Err(error(YamlErrorKind::InvalidSyntax, span.line, span.column))
}

fn parse_hex_escape(
    text: &str,
    position: &mut usize,
    digits: usize,
    span: Span,
) -> Result<char, YamlError> {
    let Some(slice) = text.get(*position..position.saturating_add(digits)) else {
        return Err(error(
            YamlErrorKind::UnsupportedEscape,
            span.line,
            span.column + *position,
        ));
    };
    let mut value = 0u32;
    for byte in slice.bytes() {
        let nibble = match byte {
            b'0'..=b'9' => u32::from(byte - b'0'),
            b'a'..=b'f' => u32::from(byte - b'a' + 10),
            b'A'..=b'F' => u32::from(byte - b'A' + 10),
            _ => {
                return Err(error(
                    YamlErrorKind::UnsupportedEscape,
                    span.line,
                    span.column + *position,
                ))
            }
        };
        value = value.saturating_mul(16).saturating_add(nibble);
    }
    *position = position.saturating_add(digits);
    char::from_u32(value).ok_or_else(|| {
        error(
            YamlErrorKind::UnsupportedEscape,
            span.line,
            span.column + *position,
        )
    })
}

fn split_pair(text: &str) -> Option<(&str, &str, usize)> {
    let bytes = text.as_bytes();
    let leading = text.len().saturating_sub(text.trim_start().len());
    let quoted_key = matches!(bytes.get(leading), Some(b'"' | b'\''));
    let mut position = 0usize;
    let mut quote = None;
    while let Some(byte) = bytes.get(position).copied() {
        if !quoted_key {
            if byte == b':' && bytes.get(position + 1).is_none_or(u8::is_ascii_whitespace) {
                let key = text.get(..position)?.trim();
                let raw = text.get(position + 1..)?;
                let value_column = position.saturating_add(2);
                return Some((key, raw, value_column));
            }
            position += 1;
            continue;
        }
        match (quote, byte) {
            (Some(b'"'), b'\\') => position = position.saturating_add(2),
            (Some(active), value) if active == value => {
                if active == b'\'' && bytes.get(position + 1) == Some(&b'\'') {
                    position = position.saturating_add(2);
                } else {
                    quote = None;
                    position += 1;
                }
            }
            (Some(_), _) => position += 1,
            (None, b'"' | b'\'') => {
                quote = Some(byte);
                position += 1;
            }
            (None, b':') if bytes.get(position + 1).is_none_or(u8::is_ascii_whitespace) => {
                let key = text.get(..position)?.trim();
                let raw = text.get(position + 1..)?;
                let value_column = position.saturating_add(2);
                return Some((key, raw, value_column));
            }
            (None, _) => position += 1,
        }
    }
    None
}

fn strip_comment(text: &str) -> &str {
    let bytes = text.as_bytes();
    let leading = text.len().saturating_sub(text.trim_start().len());
    let scalar = text.get(leading..).map_or("", |value| value);
    let flow_sequence = scalar.starts_with('[');
    let quoted_scalar = matches!(scalar.as_bytes().first(), Some(b'"' | b'\''));
    let mut position = 0usize;
    let mut quote = None;
    while let Some(byte) = bytes.get(position).copied() {
        if !flow_sequence && !quoted_scalar {
            if byte == b'#'
                && position > 0
                && bytes
                    .get(position - 1)
                    .is_some_and(u8::is_ascii_whitespace)
            {
                return text.get(..position).map_or("", |value| value);
            }
            position += 1;
            continue;
        }
        match (quote, byte) {
            (Some(b'"'), b'\\') => position = position.saturating_add(2),
            (Some(active), value) if active == value => {
                if active == b'\'' && bytes.get(position + 1) == Some(&b'\'') {
                    position = position.saturating_add(2);
                } else {
                    quote = None;
                    position += 1;
                }
            }
            (Some(_), _) => position += 1,
            (None, b'"' | b'\'') => {
                quote = Some(byte);
                position += 1;
            }
            (None, b'#')
                if position == 0
                    || bytes.get(position - 1).is_some_and(u8::is_ascii_whitespace) =>
            {
                return text.get(..position).map_or("", |value| value);
            }
            (None, _) => position += 1,
        }
    }
    text
}

fn sequence_rest(text: &str) -> Option<&str> {
    if text == "-" {
        Some("")
    } else if text.starts_with("- ") {
        text.get(2..)
    } else {
        None
    }
}

fn is_sequence_line(text: &str) -> bool {
    sequence_rest(text).is_some()
}

fn is_document_marker(line: SourceLine<'_>) -> bool {
    line.indent == 0 && matches!(line.text, "---" | "...")
}

fn contains_yaml_directive_token(text: &str) -> bool {
    text.split_whitespace()
        .any(|part| part.starts_with('&') || part.starts_with('*') || part.starts_with('!'))
}

fn ambiguous_word(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "yes" | "no" | "on" | "off" | "y" | "n" | "null" | "none" | "true" | "false"
    ) && !matches!(text, "true" | "false")
}

fn numeric_looking(text: &str) -> bool {
    let bytes = text.as_bytes();
    match bytes.first().copied() {
        Some(b'0'..=b'9') => true,
        Some(b'.') => matches!(bytes.get(1), Some(b'0'..=b'9' | b'i' | b'I' | b'n' | b'N')),
        Some(b'+' | b'-') => matches!(bytes.get(1), Some(b'0'..=b'9' | b'.')),
        _ => false,
    }
}

fn skip_spaces(bytes: &[u8], position: &mut usize) {
    while bytes.get(*position).is_some_and(u8::is_ascii_whitespace) {
        *position = position.saturating_add(1);
    }
}

fn without_cr(line: &str) -> &str {
    line.strip_suffix('\r').map_or(line, |value| value)
}

fn error(kind: YamlErrorKind, line: usize, column: usize) -> YamlError {
    YamlError { kind, line, column }
}
