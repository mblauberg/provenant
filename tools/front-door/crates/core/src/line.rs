//! One-line excerpts of untrusted text.

/// `text` on one line, at most `max_chars` characters. Runs of whitespace,
/// control characters and invisible format characters (such as bidirectional
/// overrides) collapse to one space. Longer text is cut to `max_chars - 1`
/// characters and ends with `…`, so the result never exceeds `max_chars`.
///
/// This does not redact. External publication must apply its destination's
/// disclosure projection to the complete content before formatting excerpts.
pub fn bounded_line(text: &str, max_chars: usize) -> String {
    let line = one_line(text);
    if line.chars().count() <= max_chars {
        return line;
    }
    let mut cut: String = line.chars().take(max_chars.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// `text` on one line like [`bounded_line`], but a longer text keeps its end:
/// the result starts with `…` and holds the last `max_chars - 1` characters.
/// A stream's failure is usually its last line, so a tail excerpt keeps it.
pub fn bounded_tail(text: &str, max_chars: usize) -> String {
    let line = one_line(text);
    let count = line.chars().count();
    if count <= max_chars {
        return line;
    }
    let keep = max_chars.saturating_sub(1);
    let mut cut = String::from('…');
    cut.extend(line.chars().skip(count - keep));
    cut
}

fn one_line(text: &str) -> String {
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
    spaced.split_whitespace().collect::<Vec<_>>().join(" ")
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
    use super::{bounded_line, bounded_tail};

    #[test]
    fn the_tail_excerpt_keeps_the_end_within_the_limit() {
        assert_eq!(bounded_tail("a \n b", 6), "a b");
        assert_eq!(bounded_tail("abcdefg", 6), "…cdefg");
        assert_eq!(bounded_tail("ééééééé", 4), "…ééé");
        assert_eq!(
            bounded_tail("noise\n\nfatal: the real failure", 24),
            "…fatal: the real failure"
        );
    }

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
