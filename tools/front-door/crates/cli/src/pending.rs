//! Private compatibility primitives awaiting their owning crate.
mod python_unicode;

// port: belongs in provenant-core
// Unicode decimal digits, as accepted by Python re \d and int.
pub(crate) fn decimal_digit(c: char) -> Option<u8> {
    let code = u32::from(c);
    // port: belongs in provenant-core
    const ZEROS: &[u32] = &[
        0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
        0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
        0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0,
        0xff10, 0x104a0, 0x10d30, 0x10d40, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450,
        0x114d0, 0x11650, 0x116c0, 0x116d0, 0x116da, 0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50,
        0x11d50, 0x11da0, 0x11f50, 0x16130, 0x16a60, 0x16ac0, 0x16b50, 0x16d70, 0x1ccf0, 0x1d7ce,
        0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e5f1, 0x1e950, 0x1fbf0,
    ];
    ZEROS
        .iter()
        .find_map(|&zero| code.checked_sub(zero).filter(|&d| d < 10).map(|d| d as u8))
}

// port: belongs in provenant-core
pub(crate) fn python_digit(c: char) -> bool {
    decimal_digit(c).is_some()
        || matches!(
            u32::from(c),
            0xb2 | 0xb3
                | 0xb9
                | 0x1369
                | 0x136a
                | 0x136b
                | 0x136c
                | 0x136d
                | 0x136e
                | 0x136f
                | 0x1370
                | 0x1371
                | 0x19da
                | 0x2070
                | 0x2074
                | 0x2075
                | 0x2076
                | 0x2077
                | 0x2078
                | 0x2079
                | 0x2080
                | 0x2081
                | 0x2082
                | 0x2083
                | 0x2084
                | 0x2085
                | 0x2086
                | 0x2087
                | 0x2088
                | 0x2089
                | 0x2460
                | 0x2461
                | 0x2462
                | 0x2463
                | 0x2464
                | 0x2465
                | 0x2466
                | 0x2467
                | 0x2468
                | 0x2474
                | 0x2475
                | 0x2476
                | 0x2477
                | 0x2478
                | 0x2479
                | 0x247a
                | 0x247b
                | 0x247c
                | 0x2488
                | 0x2489
                | 0x248a
                | 0x248b
                | 0x248c
                | 0x248d
                | 0x248e
                | 0x248f
                | 0x2490
                | 0x24ea
                | 0x24f5
                | 0x24f6
                | 0x24f7
                | 0x24f8
                | 0x24f9
                | 0x24fa
                | 0x24fb
                | 0x24fc
                | 0x24fd
                | 0x24ff
                | 0x2776
                | 0x2777
                | 0x2778
                | 0x2779
                | 0x277a
                | 0x277b
                | 0x277c
                | 0x277d
                | 0x277e
                | 0x2780
                | 0x2781
                | 0x2782
                | 0x2783
                | 0x2784
                | 0x2785
                | 0x2786
                | 0x2787
                | 0x2788
                | 0x278a
                | 0x278b
                | 0x278c
                | 0x278d
                | 0x278e
                | 0x278f
                | 0x2790
                | 0x2791
                | 0x2792
                | 0x10a40
                | 0x10a41
                | 0x10a42
                | 0x10a43
                | 0x10e60
                | 0x10e61
                | 0x10e62
                | 0x10e63
                | 0x10e64
                | 0x10e65
                | 0x10e66
                | 0x10e67
                | 0x10e68
                | 0x11052
                | 0x11053
                | 0x11054
                | 0x11055
                | 0x11056
                | 0x11057
                | 0x11058
                | 0x11059
                | 0x1105a
                | 0x1f100
                | 0x1f101
                | 0x1f102
                | 0x1f103
                | 0x1f104
                | 0x1f105
                | 0x1f106
                | 0x1f107
                | 0x1f108
                | 0x1f109
                | 0x1f10a
        )
}

#[cfg(test)]
mod tests {
    use provenant_core::py::{python_repr, python_strip};

    #[test]
    fn repr_and_strip_match_python_on_control_and_unicode_boundaries() {
        let mut values: Vec<String> = (0..=255)
            .chain(0x2000..=0x2070)
            .filter_map(char::from_u32)
            .map(String::from)
            .collect();
        values.extend(
            [
                "a'b",
                "a\"b",
                "a'\"b",
                "\n\x1c text \x1f",
                "é",
                "🦀",
                "\u{10ffff}",
            ]
            .map(str::to_owned),
        );
        let Some(expected): Option<Vec<(String, String)>> = crate::oracle::python(
            "import json,sys; print(json.dumps([(repr(s),s.strip()) for s in json.load(sys.stdin)]))", &serde_json::json!(values)
        ) else { return; };
        for (value, (repr, strip)) in values.iter().zip(expected) {
            assert_eq!(python_repr(value), repr, "{value:?}");
            assert_eq!(python_strip(value), strip, "{value:?}");
        }
    }
}

// port: belongs in provenant-core
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IntegerError {
    #[error("Exceeds the limit (4300 digits) for integer string conversion: value has {digits} digits; use sys.set_int_max_str_digits() to increase the limit")]
    DigitLimit { digits: usize },
    #[error("integer outside i64/u64 range")]
    NumberOutOfRange,
}

// port: belongs in provenant-core
pub(crate) fn decimal_integer(text: &str) -> Result<u64, IntegerError> {
    let digits = text.chars().count();
    if digits > 4300 {
        return Err(IntegerError::DigitLimit { digits });
    }
    text.chars().try_fold(0u64, |n, c| {
        let d = decimal_digit(c).ok_or(IntegerError::NumberOutOfRange)?;
        n.checked_mul(10)
            .and_then(|n| n.checked_add(u64::from(d)))
            .ok_or(IntegerError::NumberOutOfRange)
    })
}

// port: belongs in provenant-core
pub(crate) fn python_splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(
            c,
            '\n' | '\r'
                | '\u{b}'
                | '\u{c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
            lines.push(&text[start..i]);
            start = i + c.len_utf8();
            if c == '\r' && chars.peek().is_some_and(|(_, c)| *c == '\n') {
                chars.next();
                start += 1;
            }
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

// port: belongs in provenant-core
pub(crate) fn python_word_character(c: char) -> bool {
    c == '_' || python_unicode::alphanumeric(c)
}
