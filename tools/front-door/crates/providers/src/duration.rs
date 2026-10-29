//! Duration suffixes shared by provider reset messages, quota windows and
//! route pacing.

/// Seconds in a duration suffix such as `30m`, `7h23m` or `2d 5m 30s`.
///
/// Each component is digits followed by `d`, `h`, `m`, `s` or `w`; components
/// may be separated by whitespace. The whole string must be consumed. Empty
/// input, a component without a suffix and arithmetic overflow are rejected.
/// Callers apply their own positive or range checks.
pub fn suffix_seconds(text: &str) -> Option<u64> {
    let mut rest = text.trim_start();
    if rest.is_empty() {
        return None;
    }
    let mut total = 0_u64;
    while !rest.is_empty() {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let scale = match rest.as_bytes().get(digits).filter(|_| digits > 0)? {
            b'd' => 86_400,
            b'h' => 3_600,
            b'm' => 60,
            b's' => 1,
            b'w' => 604_800,
            _ => return None,
        };
        let amount = rest[..digits].parse::<u64>().ok()?;
        total = total.checked_add(amount.checked_mul(scale)?)?;
        rest = rest[digits + 1..].trim_start();
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::suffix_seconds;

    #[test]
    fn suffix_seconds_reads_every_spelling_the_three_callers_accepted() {
        for (text, seconds) in [
            ("30m", 1_800),
            ("1s", 1),
            ("2h", 7_200),
            ("1d", 86_400),
            ("2w", 1_209_600),
            ("7h23m", 26_580),
            ("2d 5m 30s", 173_130),
            ("1w 1d 1h 1m 1s", 694_861),
            ("0m", 0),
            (" 15m ", 900),
        ] {
            assert_eq!(suffix_seconds(text), Some(seconds), "{text:?}");
        }
    }

    #[test]
    fn suffix_seconds_rejects_partial_or_overflowing_text() {
        for text in [
            "",
            " ",
            "15",
            "h",
            "15x",
            "7h23",
            "5m later",
            "1h-2m",
            "1.5h",
            "+5m",
            "18446744073709551615w",
            "18446744073709551615s1s",
        ] {
            assert_eq!(suffix_seconds(text), None, "{text:?}");
        }
    }
}
