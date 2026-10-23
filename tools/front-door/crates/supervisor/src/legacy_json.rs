//! Minimal strict JSON reader, enough for the product-root pointer file.
//! Strict means no `NaN`, no trailing commas and no control characters in
//! strings; duplicate keys keep the last value, as Python's `json` does.
#![forbid(unsafe_code)]

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// An integer literal (no fraction or exponent) that fits in `i64`.
    Int(i64),
    /// Any other number, including integers too large for `i64`.
    Float(f64),
    Str(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    /// The value for `key` when `self` is an object; the last duplicate wins.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(members) => members.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

const MAX_DEPTH: usize = 128;

pub fn parse(text: &str) -> Result<Value, String> {
    let mut reader = Reader {
        bytes: text.as_bytes(),
        at: 0,
    };
    reader.skip_space();
    let value = reader.value(0)?;
    reader.skip_space();
    if reader.at != reader.bytes.len() {
        return Err(reader.fail("extra data"));
    }
    Ok(value)
}

/// Replace only the named top-level member's value spans. All other bytes,
/// including numeric spelling and whitespace, stay unchanged. Duplicate
/// members are all redacted so an ignored earlier value cannot leak secrets.
pub fn redact_member(text: &str, member: &str, replacement: &str) -> Result<String, String> {
    parse(text)?;
    parse(replacement)?;
    let mut reader = Reader {
        bytes: text.as_bytes(),
        at: 0,
    };
    reader.skip_space();
    if !reader.eat(b'{') {
        return Err("expected object".into());
    }
    let mut out = String::new();
    let mut copied = 0;
    reader.skip_space();
    if !reader.eat(b'}') {
        loop {
            reader.skip_space();
            let key = reader.string()?;
            reader.skip_space();
            reader.eat(b':');
            reader.skip_space();
            let start = reader.at;
            reader.value(1)?;
            if key == member {
                out.push_str(&text[copied..start]);
                out.push_str(replacement);
                copied = reader.at;
            }
            reader.skip_space();
            if reader.eat(b'}') {
                break;
            }
            reader.eat(b',');
        }
    }
    out.push_str(&text[copied..]);
    Ok(out)
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn fail(&self, what: &str) -> String {
        format!("{what}: byte {}", self.at)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn skip_space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        let found = self.peek() == Some(byte);
        if found {
            self.at += 1;
        }
        found
    }

    fn literal(&mut self, word: &str, value: Value) -> Result<Value, String> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(self.fail("expected value"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > MAX_DEPTH {
            return Err(self.fail("nesting too deep"));
        }
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Value::Str),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'n') => self.literal("null", Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.fail("expected value")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, String> {
        self.at += 1;
        let mut members = Vec::new();
        self.skip_space();
        if self.eat(b'}') {
            return Ok(Value::Object(members));
        }
        loop {
            self.skip_space();
            if self.peek() != Some(b'"') {
                return Err(self.fail("expected property name"));
            }
            let key = self.string()?;
            self.skip_space();
            if !self.eat(b':') {
                return Err(self.fail("expected ':'"));
            }
            self.skip_space();
            members.push((key, self.value(depth + 1)?));
            self.skip_space();
            if self.eat(b'}') {
                return Ok(Value::Object(members));
            }
            if !self.eat(b',') {
                return Err(self.fail("expected ',' or '}'"));
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, String> {
        self.at += 1;
        let mut items = Vec::new();
        self.skip_space();
        if self.eat(b']') {
            return Ok(Value::Array(items));
        }
        loop {
            self.skip_space();
            items.push(self.value(depth + 1)?);
            self.skip_space();
            if self.eat(b']') {
                return Ok(Value::Array(items));
            }
            if !self.eat(b',') {
                return Err(self.fail("expected ',' or ']'"));
            }
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.at;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        self.at - start
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.at;
        self.eat(b'-');
        match self.peek() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(self.fail("invalid number")),
        }
        let mut integer = true;
        if self.eat(b'.') {
            integer = false;
            if self.digits() == 0 {
                return Err(self.fail("invalid number"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            integer = false;
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if self.digits() == 0 {
                return Err(self.fail("invalid number"));
            }
        }
        let literal = std::str::from_utf8(&self.bytes[start..self.at]).unwrap_or_default();
        match literal.parse::<i64>() {
            Ok(n) if integer => Ok(Value::Int(n)),
            _ => Ok(Value::Float(
                literal
                    .parse::<f64>()
                    .map_err(|_| self.fail("invalid number"))?,
            )),
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self
            .bytes
            .get(self.at..self.at + 4)
            .ok_or_else(|| self.fail("bad \\u escape"))?;
        let text = std::str::from_utf8(digits).map_err(|_| self.fail("bad \\u escape"))?;
        let code = u32::from_str_radix(text, 16).map_err(|_| self.fail("bad \\u escape"))?;
        self.at += 4;
        Ok(code)
    }

    fn unicode_escape(&mut self) -> Result<char, String> {
        let first = self.hex4()?;
        let code = if (0xD800..0xDC00).contains(&first) && self.bytes[self.at..].starts_with(b"\\u")
        {
            let mark = self.at;
            self.at += 2;
            match self.hex4()? {
                low @ 0xDC00..=0xDFFF => 0x10000 + ((first - 0xD800) << 10) + (low - 0xDC00),
                _ => {
                    self.at = mark;
                    first
                }
            }
        } else {
            first
        };
        // A lone surrogate has no `char`; Python keeps it, we substitute.
        Ok(char::from_u32(code).unwrap_or('\u{FFFD}'))
    }

    fn string(&mut self) -> Result<String, String> {
        self.at += 1;
        let mut out = String::new();
        loop {
            let run = self.at;
            while !matches!(self.peek(), None | Some(b'"' | b'\\' | 0..=0x1f)) {
                self.at += 1;
            }
            // The input is a &str and the run stops on ASCII, so this is valid UTF-8.
            out.push_str(std::str::from_utf8(&self.bytes[run..self.at]).unwrap_or_default());
            match self.peek() {
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    let escape = self
                        .peek()
                        .ok_or_else(|| self.fail("unterminated string"))?;
                    self.at += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape()?),
                        _ => return Err(self.fail("invalid escape")),
                    }
                }
                Some(_) => return Err(self.fail("control character in string")),
                None => return Err(self.fail("unterminated string")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pointer(text: &str) -> Value {
        parse(text).unwrap()
    }

    #[test]
    fn redaction_preserves_opaque_bytes_and_all_duplicate_members() {
        let input = r#"{ "env": {"OLD":"secret"}, "opaque": 1.250e+2, "nested":{"env":"untouched"}, "env" : {"NEW":"secret2"} }"#;
        let redacted = redact_member(input, "env", r#"["NEW"]"#).unwrap();
        assert_eq!(
            redacted,
            r#"{ "env": ["NEW"], "opaque": 1.250e+2, "nested":{"env":"untouched"}, "env" : ["NEW"] }"#
        );
    }
    #[test]
    fn reads_the_pointer_shape() {
        let value = pointer("{\"schema_version\": 1, \"product_root\": \"/a/b\"}\n");
        assert_eq!(value.get("schema_version"), Some(&Value::Int(1)));
        assert_eq!(value.get("product_root"), Some(&Value::Str("/a/b".into())));
    }

    #[test]
    fn distinguishes_integers_from_other_numbers_and_types() {
        assert_eq!(pointer("1"), Value::Int(1));
        assert_eq!(pointer("-0"), Value::Int(0));
        assert_eq!(pointer("1.0"), Value::Float(1.0));
        assert_eq!(pointer("1e0"), Value::Float(1.0));
        assert_eq!(pointer("99999999999999999999"), Value::Float(1e20));
        assert_eq!(pointer("true"), Value::Bool(true));
        assert_eq!(pointer("null"), Value::Null);
        assert_eq!(pointer("\"1\""), Value::Str("1".into()));
    }

    #[test]
    fn last_duplicate_key_wins_and_extra_keys_are_kept() {
        let value = pointer("{\"a\": 1, \"b\": [1, {\"c\": null}], \"a\": 2}");
        assert_eq!(value.get("a"), Some(&Value::Int(2)));
        assert!(matches!(value.get("b"), Some(Value::Array(items)) if items.len() == 2));
    }

    #[test]
    fn decodes_escapes_and_surrogate_pairs() {
        assert_eq!(
            pointer(r#""a\n\u00e9\ud83d\ude00\/""#),
            Value::Str("a\n\u{e9}\u{1f600}/".into())
        );
        assert_eq!(pointer(r#""\ud800""#), Value::Str("\u{FFFD}".into()));
    }

    #[test]
    fn rejects_what_python_rejects_and_nan() {
        for bad in [
            "",
            "   ",
            "{",
            "{\"a\":}",
            "[1,]",
            "{\"a\":1,}",
            "NaN",
            "Infinity",
            "-Infinity",
            "01",
            "1.",
            ".5",
            "\"a\nb\"",
            "\"\\x\"",
            "{'a': 1}",
            "1 2",
            "\u{feff}{}",
            "\"abc",
            "tru",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn rejects_runaway_nesting() {
        assert!(parse(&"[".repeat(1000)).is_err());
    }
}
