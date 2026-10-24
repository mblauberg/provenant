//! Byte-preserving JSON/JSONC selector edits for harness configuration files.
//!
//! The records crate validates the complete bounded document and retains source
//! spans. This module edits only the selected value/member/element and reparses
//! the result before returning it.

use crate::InstallError;
use provenant_records::{JsoncDocument, JsoncToken, JsoncTokenKind, Limits};
use serde_json::{Map, Value};
use std::io::Cursor;

const OUTPUT_LIMIT: usize = 1_048_576;

/// Read a selector from a bounded JSON or JSONC document.
pub(crate) fn get(source: &[u8], path: &[&str]) -> Result<Option<Value>, InstallError> {
    let document = read_document(source)?;
    let value: Value = document.decode()?;
    Ok(value_at_path(&value, path).cloned())
}

/// Set, insert, append to, or remove one selector while retaining unrelated
/// source bytes, comments, and member/element order.
///
/// Object path components are literal keys. When the current value is an array,
/// a component containing a decimal `usize` selects an element; the index equal
/// to the array length appends on a set. Removing a missing selector is a no-op.
pub(crate) fn set(
    source: &[u8],
    path: &[&str],
    replacement: Option<&Value>,
) -> Result<Vec<u8>, InstallError> {
    if path.is_empty() || path.len() > Limits::CONFIG.max_depth {
        return Err(invalid("JSONC selector path is empty or too deep"));
    }
    let document = read_document(source)?;
    let decoded: Value = document.decode()?;

    if let Some(replacement) = replacement
        && value_at_path(&decoded, path) == Some(replacement)
    {
        return Ok(source.to_vec());
    }

    let source = document.source().as_bytes();
    let root = TreeParser::new(&document).parse()?;
    let edits = edit_path(&root, path, replacement, source)?;
    let edited = apply_edits(source, edits)?;
    // The edit plan is intentionally small and span-based; this second strict
    // bounded read proves that no malformed or duplicate selector was produced.
    let _ = read_document(&edited)?;
    Ok(edited)
}

fn read_document(source: &[u8]) -> Result<JsoncDocument, InstallError> {
    Ok(JsoncDocument::read(Cursor::new(source), Limits::CONFIG)?)
}

fn invalid(reason: &'static str) -> InstallError {
    InstallError::InvalidConfig(reason)
}

fn value_at_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for component in path {
        current = match current {
            Value::Object(object) => object.get(*component)?,
            Value::Array(array) => array.get(parse_index(component).ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

#[derive(Clone, Copy, Debug)]
struct Span {
    start: usize,
    end: usize,
}

#[derive(Debug)]
struct Member {
    key: String,
    key_span: Span,
    colon_span: Span,
    value: Node,
    comma: Option<Span>,
}

#[derive(Debug)]
struct Element {
    value: Node,
    comma: Option<Span>,
}

#[derive(Debug)]
enum NodeKind {
    Scalar,
    Object {
        open_end: usize,
        close_start: usize,
        members: Vec<Member>,
    },
    Array {
        open_end: usize,
        close_start: usize,
        elements: Vec<Element>,
    },
}

#[derive(Debug)]
struct Node {
    span: Span,
    kind: NodeKind,
}

struct TreeParser<'a> {
    tokens: &'a [JsoncToken],
    projection: &'a [u8],
    cursor: usize,
}

impl<'a> TreeParser<'a> {
    fn new(document: &'a JsoncDocument) -> Self {
        Self {
            tokens: document.tokens(),
            projection: document.projection(),
            cursor: 0,
        }
    }

    fn parse(mut self) -> Result<Node, InstallError> {
        self.skip_comments();
        let root = self.parse_value(0)?;
        self.skip_comments();
        if self.cursor != self.tokens.len() {
            return Err(invalid("JSONC token tree has trailing content"));
        }
        Ok(root)
    }

    fn parse_value(&mut self, depth: usize) -> Result<Node, InstallError> {
        if depth > Limits::CONFIG.max_depth {
            return Err(invalid("JSONC token tree exceeds the depth limit"));
        }
        let token = self
            .tokens
            .get(self.cursor)
            .copied()
            .ok_or_else(|| invalid("JSONC token tree ended before a value"))?;
        match token.kind {
            JsoncTokenKind::ObjectStart => self.parse_object(token, depth),
            JsoncTokenKind::ArrayStart => self.parse_array(token, depth),
            JsoncTokenKind::String
            | JsoncTokenKind::Number
            | JsoncTokenKind::True
            | JsoncTokenKind::False
            | JsoncTokenKind::Null => {
                self.cursor = self.cursor.saturating_add(1);
                Ok(Node {
                    span: Span {
                        start: token.start,
                        end: token.end,
                    },
                    kind: NodeKind::Scalar,
                })
            }
            _ => Err(invalid("JSONC token tree expected a value")),
        }
    }

    fn parse_object(&mut self, open: JsoncToken, depth: usize) -> Result<Node, InstallError> {
        self.cursor = self.cursor.saturating_add(1);
        self.skip_comments();
        let mut members = Vec::new();
        loop {
            let token = self
                .tokens
                .get(self.cursor)
                .copied()
                .ok_or_else(|| invalid("JSONC object has no closing brace"))?;
            if token.kind == JsoncTokenKind::ObjectEnd {
                self.cursor = self.cursor.saturating_add(1);
                return Ok(Node {
                    span: Span {
                        start: open.start,
                        end: token.end,
                    },
                    kind: NodeKind::Object {
                        open_end: open.end,
                        close_start: token.start,
                        members,
                    },
                });
            }
            if token.kind != JsoncTokenKind::String {
                return Err(invalid("JSONC object member has no string key"));
            }
            let key_span = Span {
                start: token.start,
                end: token.end,
            };
            let key_bytes = self
                .projection
                .get(key_span.start..key_span.end)
                .ok_or_else(|| invalid("JSONC object key span is out of bounds"))?;
            let key: String = serde_json::from_slice(key_bytes)
                .map_err(|_| invalid("JSONC object key span is invalid"))?;
            self.cursor = self.cursor.saturating_add(1);
            self.skip_comments();
            let colon = self
                .tokens
                .get(self.cursor)
                .copied()
                .ok_or_else(|| invalid("JSONC object member has no colon"))?;
            if colon.kind != JsoncTokenKind::Colon {
                return Err(invalid("JSONC object member has no colon"));
            }
            self.cursor = self.cursor.saturating_add(1);
            self.skip_comments();
            let value = self.parse_value(depth.saturating_add(1))?;
            self.skip_comments();
            let next = self
                .tokens
                .get(self.cursor)
                .copied()
                .ok_or_else(|| invalid("JSONC object has no closing brace"))?;
            let comma = match next.kind {
                JsoncTokenKind::Comma | JsoncTokenKind::TrailingComma => {
                    self.cursor = self.cursor.saturating_add(1);
                    Some(Span {
                        start: next.start,
                        end: next.end,
                    })
                }
                _ => None,
            };
            members.push(Member {
                key,
                key_span,
                colon_span: Span {
                    start: colon.start,
                    end: colon.end,
                },
                value,
                comma,
            });
            if comma.is_some() {
                self.skip_comments();
                let after_comma = self
                    .tokens
                    .get(self.cursor)
                    .copied()
                    .ok_or_else(|| invalid("JSONC object has no closing brace"))?;
                if after_comma.kind == JsoncTokenKind::ObjectEnd {
                    self.cursor = self.cursor.saturating_add(1);
                    return Ok(Node {
                        span: Span {
                            start: open.start,
                            end: after_comma.end,
                        },
                        kind: NodeKind::Object {
                            open_end: open.end,
                            close_start: after_comma.start,
                            members,
                        },
                    });
                }
            } else {
                let close = self
                    .tokens
                    .get(self.cursor)
                    .copied()
                    .ok_or_else(|| invalid("JSONC object has no closing brace"))?;
                if close.kind != JsoncTokenKind::ObjectEnd {
                    return Err(invalid("JSONC object member separator is ambiguous"));
                }
                self.cursor = self.cursor.saturating_add(1);
                return Ok(Node {
                    span: Span {
                        start: open.start,
                        end: close.end,
                    },
                    kind: NodeKind::Object {
                        open_end: open.end,
                        close_start: close.start,
                        members,
                    },
                });
            }
        }
    }

    fn parse_array(&mut self, open: JsoncToken, depth: usize) -> Result<Node, InstallError> {
        self.cursor = self.cursor.saturating_add(1);
        self.skip_comments();
        let mut elements = Vec::new();
        loop {
            let token = self
                .tokens
                .get(self.cursor)
                .copied()
                .ok_or_else(|| invalid("JSONC array has no closing bracket"))?;
            if token.kind == JsoncTokenKind::ArrayEnd {
                self.cursor = self.cursor.saturating_add(1);
                return Ok(Node {
                    span: Span {
                        start: open.start,
                        end: token.end,
                    },
                    kind: NodeKind::Array {
                        open_end: open.end,
                        close_start: token.start,
                        elements,
                    },
                });
            }
            let value = self.parse_value(depth.saturating_add(1))?;
            self.skip_comments();
            let next = self
                .tokens
                .get(self.cursor)
                .copied()
                .ok_or_else(|| invalid("JSONC array has no closing bracket"))?;
            let comma = match next.kind {
                JsoncTokenKind::Comma | JsoncTokenKind::TrailingComma => {
                    self.cursor = self.cursor.saturating_add(1);
                    Some(Span {
                        start: next.start,
                        end: next.end,
                    })
                }
                _ => None,
            };
            elements.push(Element { value, comma });
            if comma.is_some() {
                self.skip_comments();
                let after_comma = self
                    .tokens
                    .get(self.cursor)
                    .copied()
                    .ok_or_else(|| invalid("JSONC array has no closing bracket"))?;
                if after_comma.kind == JsoncTokenKind::ArrayEnd {
                    self.cursor = self.cursor.saturating_add(1);
                    return Ok(Node {
                        span: Span {
                            start: open.start,
                            end: after_comma.end,
                        },
                        kind: NodeKind::Array {
                            open_end: open.end,
                            close_start: after_comma.start,
                            elements,
                        },
                    });
                }
            } else {
                let close = self
                    .tokens
                    .get(self.cursor)
                    .copied()
                    .ok_or_else(|| invalid("JSONC array has no closing bracket"))?;
                if close.kind != JsoncTokenKind::ArrayEnd {
                    return Err(invalid("JSONC array element separator is ambiguous"));
                }
                self.cursor = self.cursor.saturating_add(1);
                return Ok(Node {
                    span: Span {
                        start: open.start,
                        end: close.end,
                    },
                    kind: NodeKind::Array {
                        open_end: open.end,
                        close_start: close.start,
                        elements,
                    },
                });
            }
        }
    }

    fn skip_comments(&mut self) {
        while self.tokens.get(self.cursor).is_some_and(|token| {
            matches!(
                token.kind,
                JsoncTokenKind::LineComment | JsoncTokenKind::BlockComment
            )
        }) {
            self.cursor = self.cursor.saturating_add(1);
        }
    }
}

#[derive(Debug)]
struct Edit {
    span: Span,
    replacement: Vec<u8>,
}

fn edit_path(
    node: &Node,
    path: &[&str],
    replacement: Option<&Value>,
    source: &[u8],
) -> Result<Vec<Edit>, InstallError> {
    let component = path
        .first()
        .copied()
        .ok_or_else(|| invalid("JSONC selector path ended unexpectedly"))?;
    match &node.kind {
        NodeKind::Object {
            open_end,
            close_start,
            members,
        } => {
            let found = members.iter().position(|member| member.key == component);
            if path.len() == 1 {
                return match (found, replacement) {
                    (Some(index), Some(value)) => {
                        let member = members
                            .get(index)
                            .ok_or_else(|| invalid("JSONC object member index is invalid"))?;
                        Ok(vec![Edit {
                            span: member.value.span,
                            replacement: encode_value(value)?,
                        }])
                    }
                    (Some(index), None) => remove_member(members, index),
                    (None, Some(value)) => {
                        let added = encode_value(value)?;
                        Ok(vec![object_insert(
                            source,
                            *open_end,
                            *close_start,
                            members,
                            component,
                            &added,
                        )?])
                    }
                    (None, None) => Ok(Vec::new()),
                };
            }
            match found {
                Some(index) => {
                    let member = members
                        .get(index)
                        .ok_or_else(|| invalid("JSONC object member index is invalid"))?;
                    edit_path(&member.value, tail(path)?, replacement, source)
                }
                None => match replacement {
                    Some(value) => {
                        let nested = nested_value(tail(path)?, value)?;
                        let added = encode_value(&nested)?;
                        Ok(vec![object_insert(
                            source,
                            *open_end,
                            *close_start,
                            members,
                            component,
                            &added,
                        )?])
                    }
                    None => Ok(Vec::new()),
                },
            }
        }
        NodeKind::Array {
            open_end,
            close_start,
            elements,
        } => {
            let index = parse_index(component)
                .map_err(|_| invalid("JSONC array selector must be a decimal index"))?;
            if index < elements.len() {
                let element = elements
                    .get(index)
                    .ok_or_else(|| invalid("JSONC array element index is invalid"))?;
                if path.len() == 1 {
                    return match replacement {
                        Some(value) => Ok(vec![Edit {
                            span: element.value.span,
                            replacement: encode_value(value)?,
                        }]),
                        None => remove_element(elements, index),
                    };
                }
                return edit_path(&element.value, tail(path)?, replacement, source);
            }
            if index > elements.len() {
                return Err(invalid(
                    "JSONC array selector is beyond the append position",
                ));
            }
            match replacement {
                None => Ok(Vec::new()),
                Some(value) => {
                    let appended = if path.len() == 1 {
                        value.clone()
                    } else {
                        nested_value(tail(path)?, value)?
                    };
                    let encoded = encode_value(&appended)?;
                    Ok(vec![array_insert(
                        source,
                        *open_end,
                        *close_start,
                        elements,
                        &encoded,
                    )?])
                }
            }
        }
        NodeKind::Scalar => Err(invalid("JSONC selector crosses a scalar value")),
    }
}

fn remove_member(members: &[Member], index: usize) -> Result<Vec<Edit>, InstallError> {
    let member = members
        .get(index)
        .ok_or_else(|| invalid("JSONC object member index is invalid"))?;
    let comma = if index + 1 < members.len() {
        member.comma
    } else if index > 0 {
        members.get(index - 1).and_then(|previous| previous.comma)
    } else {
        member.comma
    };
    let mut edits = vec![
        Edit {
            span: member.key_span,
            replacement: Vec::new(),
        },
        Edit {
            span: member.colon_span,
            replacement: Vec::new(),
        },
        Edit {
            span: member.value.span,
            replacement: Vec::new(),
        },
    ];
    if let Some(comma) = comma {
        edits.push(Edit {
            span: comma,
            replacement: Vec::new(),
        });
    }
    Ok(edits)
}

fn remove_element(elements: &[Element], index: usize) -> Result<Vec<Edit>, InstallError> {
    let element = elements
        .get(index)
        .ok_or_else(|| invalid("JSONC array element index is invalid"))?;
    let comma = if index + 1 < elements.len() {
        element.comma
    } else if index > 0 {
        elements.get(index - 1).and_then(|previous| previous.comma)
    } else {
        element.comma
    };
    let mut edits = vec![Edit {
        span: element.value.span,
        replacement: Vec::new(),
    }];
    if let Some(comma) = comma {
        edits.push(Edit {
            span: comma,
            replacement: Vec::new(),
        });
    }
    Ok(edits)
}

fn nested_value(path: &[&str], replacement: &Value) -> Result<Value, InstallError> {
    if path.len() > Limits::CONFIG.max_depth {
        return Err(invalid("JSONC selector path is too deep"));
    }
    let mut value = replacement.clone();
    for component in path.iter().rev() {
        let mut object = Map::new();
        object.insert((*component).to_string(), value);
        value = Value::Object(object);
    }
    Ok(value)
}

fn encode_value(value: &Value) -> Result<Vec<u8>, InstallError> {
    serde_json::to_vec(value).map_err(|_| invalid("JSONC replacement value cannot be encoded"))
}

fn object_insert(
    source: &[u8],
    open_end: usize,
    close_start: usize,
    members: &[Member],
    key: &str,
    value: &[u8],
) -> Result<Edit, InstallError> {
    let encoded_key =
        serde_json::to_vec(key).map_err(|_| invalid("JSONC replacement key cannot be encoded"))?;
    let mut member_bytes = Vec::new();
    member_bytes.extend_from_slice(&encoded_key);
    member_bytes.extend_from_slice(b": ");
    member_bytes.extend_from_slice(value);
    let multiline = has_newline(source, open_end, close_start)?;
    let parent_indent = line_indent(source, members.first().map_or(0, |m| m.key_span.start))?;
    let child_indent = if let Some(first) = members.first() {
        let observed = line_indent(source, first.key_span.start)?;
        if observed.len() > parent_indent.len() && observed.starts_with(&parent_indent) {
            observed
        } else {
            default_child_indent(&parent_indent)
        }
    } else {
        default_child_indent(&line_indent(source, open_end.saturating_sub(1))?)
    };

    let (at, bytes) = if let Some(last) = members.last() {
        let mut bytes = Vec::new();
        if last.comma.is_some() {
            bytes.extend_from_slice(if multiline { newline_for(source) } else { b" " });
            if multiline {
                bytes.extend_from_slice(&child_indent);
            }
            bytes.extend_from_slice(&member_bytes);
            (
                last.comma.map_or(last.value.span.end, |span| span.end),
                bytes,
            )
        } else {
            bytes.push(b',');
            bytes.extend_from_slice(if multiline { newline_for(source) } else { b" " });
            if multiline {
                bytes.extend_from_slice(&child_indent);
            }
            bytes.extend_from_slice(&member_bytes);
            (last.value.span.end, bytes)
        }
    } else {
        let at = trailing_whitespace_start(source, open_end, close_start)?;
        let interior = source
            .get(open_end..at)
            .ok_or_else(|| invalid("JSONC empty object span is invalid"))?;
        let mut bytes = Vec::new();
        if multiline {
            bytes.extend_from_slice(newline_for(source));
            bytes.extend_from_slice(&child_indent);
            bytes.extend_from_slice(&member_bytes);
            let suffix = source
                .get(at..close_start)
                .ok_or_else(|| invalid("JSONC empty object suffix span is invalid"))?;
            if !suffix.contains(&b'\n') {
                bytes.extend_from_slice(newline_for(source));
                bytes.extend_from_slice(&line_indent(source, open_end.saturating_sub(1))?);
            }
        } else {
            if !interior.is_empty() && !interior.iter().all(u8::is_ascii_whitespace) {
                bytes.push(b' ');
            }
            bytes.extend_from_slice(&member_bytes);
        }
        (at, bytes)
    };
    Ok(Edit {
        span: Span { start: at, end: at },
        replacement: bytes,
    })
}

fn array_insert(
    source: &[u8],
    open_end: usize,
    close_start: usize,
    elements: &[Element],
    value: &[u8],
) -> Result<Edit, InstallError> {
    let multiline = has_newline(source, open_end, close_start)?;
    let parent_indent = line_indent(source, elements.first().map_or(0, |e| e.value.span.start))?;
    let child_indent = if let Some(first) = elements.first() {
        let observed = line_indent(source, first.value.span.start)?;
        if observed.len() > parent_indent.len() && observed.starts_with(&parent_indent) {
            observed
        } else {
            default_child_indent(&parent_indent)
        }
    } else {
        default_child_indent(&line_indent(source, open_end.saturating_sub(1))?)
    };
    if let Some(last) = elements.last() {
        let mut bytes = Vec::new();
        let at = if let Some(comma) = last.comma {
            comma.end
        } else {
            bytes.push(b',');
            last.value.span.end
        };
        bytes.extend_from_slice(if multiline { newline_for(source) } else { b" " });
        if multiline {
            bytes.extend_from_slice(&child_indent);
        }
        bytes.extend_from_slice(value);
        Ok(Edit {
            span: Span { start: at, end: at },
            replacement: bytes,
        })
    } else {
        let at = trailing_whitespace_start(source, open_end, close_start)?;
        let interior = source
            .get(open_end..at)
            .ok_or_else(|| invalid("JSONC empty array span is invalid"))?;
        let mut bytes = Vec::new();
        if multiline {
            bytes.extend_from_slice(newline_for(source));
            bytes.extend_from_slice(&child_indent);
            bytes.extend_from_slice(value);
            let suffix = source
                .get(at..close_start)
                .ok_or_else(|| invalid("JSONC empty array suffix span is invalid"))?;
            if !suffix.contains(&b'\n') {
                bytes.extend_from_slice(newline_for(source));
                bytes.extend_from_slice(&line_indent(source, open_end.saturating_sub(1))?);
            }
        } else {
            if !interior.is_empty() && !interior.iter().all(u8::is_ascii_whitespace) {
                bytes.push(b' ');
            }
            bytes.extend_from_slice(value);
        }
        Ok(Edit {
            span: Span { start: at, end: at },
            replacement: bytes,
        })
    }
}

fn apply_edits(source: &[u8], mut edits: Vec<Edit>) -> Result<Vec<u8>, InstallError> {
    edits.sort_by_key(|edit| (edit.span.start, edit.span.end));
    let mut previous_end = 0usize;
    let mut final_len = source.len();
    for edit in &edits {
        if edit.span.start > edit.span.end
            || edit.span.end > source.len()
            || edit.span.start < previous_end
        {
            return Err(invalid("JSONC edit spans overlap or exceed the source"));
        }
        previous_end = edit.span.end;
        final_len = final_len
            .checked_sub(edit.span.end.saturating_sub(edit.span.start))
            .and_then(|size| size.checked_add(edit.replacement.len()))
            .ok_or_else(|| invalid("JSONC edited size overflowed"))?;
    }
    if final_len > OUTPUT_LIMIT {
        return Err(InstallError::Manifest(
            "edited JSONC configuration exceeds 1 MiB",
        ));
    }
    let mut output = Vec::with_capacity(final_len);
    let mut cursor = 0usize;
    for edit in edits {
        let prefix = source
            .get(cursor..edit.span.start)
            .ok_or_else(|| invalid("JSONC edit prefix span is invalid"))?;
        output.extend_from_slice(prefix);
        output.extend_from_slice(&edit.replacement);
        cursor = edit.span.end;
    }
    let suffix = source
        .get(cursor..)
        .ok_or_else(|| invalid("JSONC edit suffix span is invalid"))?;
    output.extend_from_slice(suffix);
    if output.len() != final_len {
        return Err(invalid("JSONC edit length accounting failed"));
    }
    Ok(output)
}

fn tail<'a, 'b>(path: &'a [&'b str]) -> Result<&'a [&'b str], InstallError> {
    path.get(1..)
        .ok_or_else(|| invalid("JSONC selector path ended unexpectedly"))
}

fn parse_index(component: &str) -> Result<usize, ()> {
    if component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    component.parse::<usize>().map_err(|_| ())
}

fn has_newline(source: &[u8], start: usize, end: usize) -> Result<bool, InstallError> {
    let bytes = source
        .get(start..end)
        .ok_or_else(|| invalid("JSONC container span is invalid"))?;
    Ok(bytes.contains(&b'\n'))
}

fn newline_for(source: &[u8]) -> &'static [u8] {
    if source.windows(2).any(|pair| pair == b"\r\n") {
        b"\r\n"
    } else {
        b"\n"
    }
}

fn line_indent(source: &[u8], offset: usize) -> Result<Vec<u8>, InstallError> {
    let prefix = source
        .get(..offset)
        .ok_or_else(|| invalid("JSONC indentation offset is invalid"))?;
    let line_start = prefix
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |position| position.saturating_add(1));
    let line_prefix = prefix
        .get(line_start..)
        .ok_or_else(|| invalid("JSONC indentation span is invalid"))?;
    if line_prefix
        .iter()
        .all(|byte| matches!(byte, b' ' | b'\t' | b'\r'))
    {
        Ok(line_prefix.to_vec())
    } else {
        Ok(Vec::new())
    }
}

fn default_child_indent(parent: &[u8]) -> Vec<u8> {
    let mut child = parent.to_vec();
    child.extend_from_slice(b"  ");
    child
}

fn trailing_whitespace_start(
    source: &[u8],
    start: usize,
    end: usize,
) -> Result<usize, InstallError> {
    let bytes = source
        .get(start..end)
        .ok_or_else(|| invalid("JSONC container span is invalid"))?;
    let suffix = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(0, |position| position.saturating_add(1));
    start
        .checked_add(suffix)
        .ok_or_else(|| invalid("JSONC insertion offset overflowed"))
}

#[cfg(test)]
mod tests {
    use super::{get, set};
    use crate::InstallError;
    use serde_json::json;

    #[test]
    fn reads_nested_jsonc_values_and_array_indices() {
        let source = br#"{"mcpServers":{"fabric":{"args":["fabric", "mcp",],}},}"#;
        assert_eq!(
            get(source, &["mcpServers", "fabric", "args", "1"]).expect("valid JSONC"),
            Some(json!("mcp"))
        );
    }

    #[test]
    fn replaces_only_the_selected_value_and_is_a_noop_when_already_equal() {
        let source =
            b"{\n  // keep me\n  \"foreign\": { \"n\": 1 },\n  \"owned\": [\"old\",],\n}\n";
        let edited = set(source, &["owned", "0"], Some(&json!("new")))
            .expect("span replacement should succeed");
        assert_eq!(
            edited,
            b"{\n  // keep me\n  \"foreign\": { \"n\": 1 },\n  \"owned\": [\"new\",],\n}\n"
        );
        assert_eq!(
            set(&edited, &["owned", "0"], Some(&json!("new"))).expect("equal selector is a no-op"),
            edited
        );
    }

    #[test]
    fn creates_missing_nested_objects_without_reserializing_foreign_bytes() {
        let source = b"{\n  // foreign comment\n  \"foreign\": 1,\n}\n";
        let edited = set(
            source,
            &["mcpServers", "fabric", "command"],
            Some(&json!("/stable/bin/provenant")),
        )
        .expect("nested object insertion should succeed");
        let text = std::str::from_utf8(&edited).expect("valid UTF-8");
        assert!(text.contains("// foreign comment"));
        assert!(text.contains("\"foreign\": 1"));
        assert_eq!(
            get(&edited, &["mcpServers", "fabric", "command"])
                .expect("edited document remains valid"),
            Some(json!("/stable/bin/provenant"))
        );
    }

    #[test]
    fn removes_exact_object_member_and_retains_other_comments_and_members() {
        let source = b"{\n  // before a\n  \"a\": 1, // between a and b\n  \"b\": {\"nested\": true},\n  \"c\": 3,\n}\n";
        let edited = set(source, &["b"], None).expect("member removal should succeed");
        let text = std::str::from_utf8(&edited).expect("valid UTF-8");
        assert!(text.contains("// before a"));
        assert!(text.contains("// between a and b"));
        assert!(text.contains("\"a\": 1"));
        assert!(text.contains("\"c\": 3"));
        assert_eq!(get(&edited, &["b"]).expect("valid document"), None);
    }

    #[test]
    fn removes_and_appends_array_values_with_trailing_comma_support() {
        let source = br#"{"items":["a", /* retain */ "b",],}"#;
        let removed = set(source, &["items", "0"], None).expect("array removal");
        assert_eq!(
            get(&removed, &["items", "0"]).expect("valid document"),
            Some(json!("b"))
        );
        let appended = set(&removed, &["items", "1"], Some(&json!("c"))).expect("array append");
        assert_eq!(
            get(&appended, &["items", "1"]).expect("valid document"),
            Some(json!("c"))
        );
        assert!(
            std::str::from_utf8(&appended)
                .expect("valid UTF-8")
                .contains("/* retain */")
        );
    }

    #[test]
    fn refuses_malformed_duplicate_and_ambiguous_paths() {
        assert!(matches!(
            set(b"{\"a\": 1, \"a\": 2}", &["a"], Some(&json!(3))),
            Err(InstallError::Decode(_))
        ));
        assert!(matches!(
            set(b"{\"a\": [1,]", &["a"], Some(&json!(3))),
            Err(InstallError::Decode(_))
        ));
        assert!(matches!(
            set(b"{\"a\": 1}", &["a", "nested"], Some(&json!(3))),
            Err(InstallError::InvalidConfig(_))
        ));
        assert!(matches!(
            set(b"{\"a\": [1]}", &["a", "3"], Some(&json!(3))),
            Err(InstallError::InvalidConfig(_))
        ));
    }

    #[test]
    fn rejects_oversized_input_before_span_editing() {
        let mut source = b"{\"a\":\"".to_vec();
        source.resize(1_048_577, b'x');
        assert!(matches!(
            get(&source, &["a"]),
            Err(InstallError::Decode(
                provenant_records::DecodeError::TooLarge { limit: 1_048_576 }
            ))
        ));
    }
}
