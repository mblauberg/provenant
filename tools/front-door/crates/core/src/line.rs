//! One-line excerpts of untrusted text.

/// `text` on one line, at most `max_chars` characters. Runs of whitespace,
/// control characters and invisible format characters (such as bidirectional
/// overrides) collapse to one space. Longer text is cut to `max_chars - 1`
/// characters and ends with `…`, so the result never exceeds `max_chars`.
///
/// This does not redact. Use `provenant_confinement::redact::bounded_line`
/// for text that came from a lane or a server.
pub fn bounded_line(text: &str, max_chars: usize) -> String {
    let spaced: String = text
        .chars()
        .map(|c| {
            if c.is_control() || is_format_character(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let line = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= max_chars {
        return line;
    }
    let mut cut: String = line.chars().take(max_chars.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

fn is_format_character(character: char) -> bool {
    matches!(
        character as u32,
        0x00AD
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x180E
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x2064
            | 0x2066..=0x206F
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0001
            | 0xE0020..=0xE007F
    )
}

#[cfg(test)]
mod tests {
    use super::bounded_line;

    #[test]
    fn collapses_whitespace_controls_and_format_characters() {
        assert_eq!(bounded_line("a \n\t b\u{202e}c\u{0}  d ", 40), "a b c d");
    }

    #[test]
    fn truncates_with_one_ellipsis_within_the_limit() {
        assert_eq!(bounded_line("abcdef", 6), "abcdef");
        assert_eq!(bounded_line("abcdefg", 6), "abcde…");
        assert_eq!(bounded_line("ééééééé", 4).chars().count(), 4);
    }
}
