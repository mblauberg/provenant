//! A plain-text line diff of two checkpoints, in unified form. Bounded work:
//! the common head and tail are trimmed first, and a middle too large to align
//! line by line is shown as a replacement rather than searched.

/// Alignment table cells allowed (4 bytes each), about 16 MiB.
const TABLE_CELLS: usize = 4_000_000;
const CONTEXT: usize = 3;

#[derive(Clone, Copy)]
enum Op {
    Same(usize, usize),
    Del(usize),
    Ins(usize),
}

fn operations(old: &[&str], new: &[&str]) -> Vec<Op> {
    let head = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let tail = old[head..]
        .iter()
        .rev()
        .zip(new[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (a, b) = (&old[head..old.len() - tail], &new[head..new.len() - tail]);
    let mut ops: Vec<Op> = (0..head).map(|i| Op::Same(i, i)).collect();
    if (a.len() + 1).saturating_mul(b.len() + 1) <= TABLE_CELLS {
        // lcs[i][j]: the longest common run of a[i..] and b[j..].
        let width = b.len() + 1;
        let mut lcs = vec![0u32; (a.len() + 1) * width];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                lcs[i * width + j] = if a[i] == b[j] {
                    lcs[(i + 1) * width + j + 1] + 1
                } else {
                    lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < a.len() || j < b.len() {
            if i < a.len() && j < b.len() && a[i] == b[j] {
                ops.push(Op::Same(head + i, head + j));
                i += 1;
                j += 1;
            } else if i < a.len()
                && (j == b.len() || lcs[(i + 1) * width + j] >= lcs[i * width + j + 1])
            {
                ops.push(Op::Del(head + i));
                i += 1;
            } else {
                ops.push(Op::Ins(head + j));
                j += 1;
            }
        }
    } else {
        ops.extend((0..a.len()).map(|i| Op::Del(head + i)));
        ops.extend((0..b.len()).map(|j| Op::Ins(head + j)));
    }
    ops.extend((0..tail).map(|k| Op::Same(old.len() - tail + k, new.len() - tail + k)));
    ops
}

/// One diff row. A final line with no newline is marked as `diff -u` does, so
/// text that differs only in its trailing newline still shows a difference.
fn line(body: &mut String, sign: char, text: &str) {
    body.push(sign);
    body.push_str(text.strip_suffix('\n').unwrap_or(text));
    body.push('\n');
    if !text.ends_with('\n') {
        body.push_str("\\ No newline at end of file\n");
    }
}

/// The unified diff from `old` to `new`, or an empty string when they match.
/// Lines are compared with their terminators.
pub fn unified(old: &str, new: &str, old_label: &str, new_label: &str) -> String {
    let (old_lines, new_lines): (Vec<&str>, Vec<&str>) = (
        old.split_inclusive('\n').collect(),
        new.split_inclusive('\n').collect(),
    );
    let ops = operations(&old_lines, &new_lines);
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, op)| !matches!(op, Op::Same(..)))
        .map(|(index, _)| index)
        .collect();
    if changed.is_empty() {
        return String::new();
    }
    let mut out = format!("--- {old_label}\n+++ {new_label}\n");
    let mut cursor = 0;
    while cursor < changed.len() {
        // A hunk runs from the first change to the last one within reach of
        // the next, with context either side.
        let mut last = cursor;
        while last + 1 < changed.len() && changed[last + 1] - changed[last] <= 2 * CONTEXT + 1 {
            last += 1;
        }
        let start = changed[cursor].saturating_sub(CONTEXT);
        let end = (changed[last] + CONTEXT + 1).min(ops.len());
        let (mut old_start, mut new_start, mut old_len, mut new_len) = (None, None, 0, 0);
        let mut body = String::new();
        for op in &ops[start..end] {
            match *op {
                Op::Same(i, j) => {
                    old_start.get_or_insert(i);
                    new_start.get_or_insert(j);
                    old_len += 1;
                    new_len += 1;
                    line(&mut body, ' ', old_lines[i]);
                }
                Op::Del(i) => {
                    old_start.get_or_insert(i);
                    old_len += 1;
                    line(&mut body, '-', old_lines[i]);
                }
                Op::Ins(j) => {
                    new_start.get_or_insert(j);
                    new_len += 1;
                    line(&mut body, '+', new_lines[j]);
                }
            }
        }
        // An empty side starts after the line before it, as `diff -u` writes it.
        let first = |start: Option<usize>, len: usize| match (start, len) {
            (Some(start), _) => start + 1,
            (None, _) => 0,
        };
        out.push_str(&format!(
            "@@ -{},{old_len} +{},{new_len} @@\n{body}",
            first(old_start, old_len),
            first(new_start, new_len)
        ));
        cursor = last + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::unified;

    #[test]
    fn identical_text_has_no_diff() {
        assert_eq!(unified("a\nb\n", "a\nb\n", "x", "y"), "");
    }

    #[test]
    fn a_changed_line_is_shown_with_its_context() {
        let old = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\n";
        let new = "one\ntwo\nthree\nFOUR\nfive\nsix\nseven\neight\n";
        assert_eq!(
            unified(old, new, "rev 1", "rev 2"),
            "--- rev 1\n+++ rev 2\n@@ -1,7 +1,7 @@\n one\n two\n three\n-four\n+FOUR\n five\n six\n seven\n"
        );
    }

    #[test]
    fn added_and_removed_lines_and_distant_changes_make_separate_hunks() {
        let old: String = (1..=20).map(|n| format!("l{n}\n")).collect();
        let new = old.replace("l2\n", "").replace("l19\n", "l19\nextra\n");
        let diff = unified(&old, &new, "a", "b");
        assert_eq!(diff.matches("@@ ").count(), 2, "{diff}");
        assert!(diff.contains("-l2\n"), "{diff}");
        assert!(diff.contains("+extra\n"), "{diff}");
    }

    #[test]
    fn an_unalignable_middle_is_a_replacement() {
        let old: String = (0..3000).map(|n| format!("old {n}\n")).collect();
        let new: String = (0..3000).map(|n| format!("new {n}\n")).collect();
        let diff = unified(&old, &new, "a", "b");
        assert_eq!(diff.lines().filter(|l| l.starts_with('-')).count(), 3001);
        assert_eq!(diff.lines().filter(|l| l.starts_with('+')).count(), 3001);
    }

    #[test]
    fn a_newline_only_change_is_a_difference_with_a_note() {
        assert_eq!(
            unified("a\n", "a", "x", "y"),
            "--- x\n+++ y\n@@ -1,1 +1,1 @@\n-a\n+a\n\\ No newline at end of file\n"
        );
        assert_eq!(
            unified("a", "a\n", "x", "y"),
            "--- x\n+++ y\n@@ -1,1 +1,1 @@\n-a\n\\ No newline at end of file\n+a\n"
        );
        // Unchanged text without a final newline is still no difference, and a
        // context line that lacks one carries the note too.
        assert_eq!(unified("a\nb", "a\nb", "x", "y"), "");
        assert_eq!(
            unified("a\nb", "A\nb", "x", "y"),
            "--- x\n+++ y\n@@ -1,2 +1,2 @@\n-a\n+A\n b\n\\ No newline at end of file\n"
        );
    }

    #[test]
    fn starting_from_nothing_adds_every_line() {
        assert_eq!(
            unified("", "a\nb\n", "none", "rev 1"),
            "--- none\n+++ rev 1\n@@ -0,0 +1,2 @@\n+a\n+b\n"
        );
    }
}
