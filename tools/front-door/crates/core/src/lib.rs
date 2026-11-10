//! Pure identities, paths, time values and cooperative cancellation.
#![forbid(unsafe_code)]

mod bytes;
pub mod changes;
mod refs;
pub use refs::*;
mod ids;
pub use bytes::{hex, unhex};
pub use ids::*;
mod line;
pub use line::bounded_line;
mod time;
pub use time::*;
mod work;
pub use work::*;
mod paths;
pub use paths::*;
mod roots;
pub use roots::*;
pub mod retry;
pub mod review_cover;

/// Validate a user-facing task title. The limit is measured in UTF-8 bytes.
pub fn validate_task_title(title: &str) -> Result<(), &'static str> {
    if title.is_empty() || title.len() > 256 || title.chars().any(char::is_control) {
        return Err("task title must be 1 to 256 bytes and contain no control characters");
    }
    Ok(())
}

/// Compute the Levenshtein distance between strings in Unicode scalar values.
pub fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut row: Vec<_> = (0..=right.len()).collect();
    for (i, a) in left.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, b) in right.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (row[j] + 1)
                .min(above + 1)
                .min(diagonal + usize::from(a != *b));
            diagonal = above;
        }
    }
    row[right.len()]
}

#[cfg(test)]
mod title_tests {
    use super::validate_task_title;

    #[test]
    fn task_titles_reject_controls_and_invalid_lengths() {
        for title in [
            "",
            "a\n",
            "a\t",
            "a\u{1b}",
            "a\0",
            "a\u{85}",
            &"a".repeat(257),
        ] {
            assert!(validate_task_title(title).is_err(), "{title:?}");
        }
        assert!(validate_task_title(&"a".repeat(256)).is_ok());
        assert!(validate_task_title("A useful title ☀").is_ok());
    }
}
