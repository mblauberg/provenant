//! CPython Unicode 16.0 properties, independent of Rust's Unicode version.
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_properties_match_all_python_scalar_values() {
        let Some(expected) = crate::test_support::python(
            "",
            r#"
import unicodedata
if unicodedata.unidata_version != '16.0.0':
    print('Unicode 16.0.0 oracle required',file=sys.stderr)
    sys.exit(77)
def flags(c):
    return int(c.isprintable()) | int(c.isalpha())<<1 | int(c.isdigit())<<2 | int(c.isdecimal())<<3 | int(c.isnumeric())<<4 | int(c.isspace())<<5
out=[]; start=None; last=0
for code in range(0x110001):
    f=flags(chr(code)) if code<0x110000 and not 0xd800<=code<=0xdfff else 0
    if f != last:
        if last: out.append([start,code-1,last])
        start=code; last=f
print(json.dumps([out, [[i,unicodedata.decimal(chr(i))] for i in range(0x110000) if chr(i).isdecimal()]]))
"#,
            &[],
        ) else {
            return;
        };
        let mut actual = Vec::new();
        let mut start = 0;
        let mut last = 0;
        for code in 0..=0x110000 {
            let flags = char::from_u32(code).map_or(0, |c| {
                u8::from(printable(c))
                    | u8::from(python_alpha(&c.to_string())) << 1
                    | u8::from(python_digit(c)) << 2
                    | u8::from(decimal_digit(c).is_some()) << 3
                    | u8::from(python_numeric(c)) << 4
                    | u8::from(python_whitespace(c)) << 5
            });
            if flags != last {
                if last != 0 {
                    actual.push([start, code - 1, u32::from(last)]);
                }
                start = code;
                last = flags;
            }
        }
        assert_eq!(serde_json::json!(actual), expected[0]);
        let digits: Vec<_> = (0..=0x10ffff)
            .filter_map(|code| {
                char::from_u32(code)
                    .and_then(decimal_digit)
                    .map(|digit| [code, u32::from(digit)])
            })
            .collect();
        assert_eq!(serde_json::json!(digits), expected[1]);
    }
    #[test]
    fn digits_words_and_alpha_are_python_properties() {
        assert_eq!(decimal_digit('９'), Some(9));
        assert_eq!(decimal_digit('𝟠'), Some(8));
        assert_eq!(decimal_digit('²'), None);
        assert!(python_digit('²'));
        assert!(!python_digit('Ⅷ'));
        assert!(python_word('Ⅷ'));
        assert!(python_word('_'));
        assert!(!python_word('\u{301}'));
        assert!(!python_alpha(""));
        assert!(!python_alpha("a\u{345}"));
        assert!(python_alpha("日本"));
    }
}

#[path = "unicode_data.rs"]
mod data;

fn properties(c: char) -> (u32, u8) {
    let code = u32::from(c);
    let index = data::PROPERTIES.partition_point(|&(_, end, _)| end < code);
    data::PROPERTIES
        .get(index)
        .filter(|&&(start, _, _)| start <= code)
        .map_or((code, 0), |&(start, _, flags)| (start, flags))
}

pub fn printable(c: char) -> bool {
    properties(c).1 & 1 != 0
}
pub fn python_alpha(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| properties(c).1 & 2 != 0)
}
pub fn python_digit(c: char) -> bool {
    properties(c).1 & 4 != 0
}
pub fn decimal_digit(c: char) -> Option<u8> {
    let (start, flags) = properties(c);
    (flags & 8 != 0).then(|| ((u32::from(c) - start) % 10) as u8)
}
pub fn python_numeric(c: char) -> bool {
    properties(c).1 & 16 != 0
}
pub fn python_whitespace(c: char) -> bool {
    properties(c).1 & 32 != 0
}
/// Python re's Unicode \\w: letters, numeric characters and underscore.
pub fn python_word(c: char) -> bool {
    c == '_' || properties(c).1 & (2 | 16) != 0
}
