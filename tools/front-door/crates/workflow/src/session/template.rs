//! The checkpoint template contract: the shape a save validates against and
//! the scaffold `--from-template` builds. The Markdown template lives at
//! `skills/session/templates/checkpoint.template.md`; the machine-readable
//! shape is kept here so a save never reads an installed skill at runtime.
//!
//! Validation is advisory: a save names each gap and still stores the text.
//! `provenant_standing` is the typed record a hook restores; `## Intent` is the
//! chair's current understanding of the live rulings. The body ceiling is a
//! template budget, separate from the configured, 8 KiB-floored soft budget.
use super::standing::{STANDING_KEY, parse_standing};
use super::summary::header_end;

/// A body larger than this is a gap against the template. It is measured
/// after frontmatter, so a typed standing record never counts as body text.
pub const TEMPLATE_BODY_BYTES: usize = 2 * 1024;

/// The `##` headings of `skills/session/templates/checkpoint.template.md`,
/// in the order the template lists them. The H1 title is handled separately.
pub const TEMPLATE_HEADINGS: [&str; 5] = [
    "## Goal",
    "## Direction",
    "## Intent",
    "## Process",
    "## Plan",
];

const INTENT: &str = "## Intent";
const TITLE: &str = "# Session checkpoint";

/// The frontmatter a first save gets when neither the input nor the previous
/// revision carries a typed standing record. Empty lists are explicit: an
/// omitted `waiting_on_owner` must never read as "nothing waits".
const STANDING_YAML: &str = "provenant_standing:\n  schema_version: 1\n  goal: \"\"\n  grants: []\n  obligations: []\n  waiting_on_owner: []\n";
const EMPTY_STANDING: &str = "---\nprovenant_standing:\n  schema_version: 1\n  goal: \"\"\n  grants: []\n  obligations: []\n  waiting_on_owner: []\n---\n";

fn body(markdown: &str) -> &str {
    let end = header_end(markdown).ok().flatten().unwrap_or(0);
    &markdown[end..]
}

fn has_heading(markdown: &str, heading: &str) -> bool {
    body(markdown)
        .lines()
        .any(|line| line.trim_end_matches('\r') == heading)
}

/// One advisory per gap, each naming what the save is missing. Absent or
/// invalid standing frontmatter is one gap: a hook restores no grant from
/// either, so both read as "no typed record".
pub fn template_gaps(markdown: &str) -> Vec<String> {
    let mut gaps = Vec::new();
    if !matches!(parse_standing(markdown), Ok(Some(_))) {
        gaps.push(format!(
            "checkpoint has no {STANDING_KEY} frontmatter; add the typed standing record with its grants' sources and expiries"
        ));
    }
    if !has_heading(markdown, INTENT) {
        gaps.push(format!(
            "checkpoint has no {INTENT} section; record the live ruling, its reason and the reading you are unsure of"
        ));
    }
    let size = body(markdown).len();
    if size > TEMPLATE_BODY_BYTES {
        gaps.push(format!(
            "checkpoint body is {size} bytes (template budget {TEMPLATE_BODY_BYTES}); rewrite current state rather than appending history"
        ));
    }
    gaps
}

/// A checkpoint split into its frontmatter and its headed sections. Each
/// section keeps its heading line and body; a repeated heading replaces the
/// earlier one, so later text always wins when sections are merged.
fn split(markdown: &str) -> (Option<String>, Vec<(String, String)>) {
    let end = header_end(markdown).ok().flatten().unwrap_or(0);
    let frontmatter = (end > 0).then(|| markdown[..end].to_owned());
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut current = None;
    for line in body(markdown).split_inclusive('\n') {
        let heading = line.trim_end_matches(['\r', '\n']);
        let is_h = (heading.starts_with("# ") && !heading.starts_with("## "))
            || (heading.starts_with("## ") && !heading.starts_with("### "));
        if is_h {
            let key = heading.trim_end_matches('\r').to_owned();
            if let Some(index) = sections.iter().position(|(name, _)| *name == key) {
                sections[index].1.clear();
                current = Some(index);
            } else {
                sections.push((key, String::new()));
                current = Some(sections.len() - 1);
            }
        }
        if let Some(index) = current {
            sections[index].1.push_str(line);
        }
    }
    (frontmatter, sections)
}

fn section<'a>(sections: &'a [(String, String)], key: &str) -> Option<&'a str> {
    sections
        .iter()
        .find(|(name, text)| name == key && !text.trim().is_empty())
        .map(|(_, text)| text.as_str())
}

/// Add an empty typed standing record to a frontmatter the input supplied,
/// keeping its other keys (a structured summary, say) intact. A stale,
/// invalid `provenant_standing` line stays in place and is still named as a
/// gap; the save never silently rewrites an owner's frontmatter.
fn with_standing(header: &str) -> String {
    let Some(end) = header_end(header).ok().flatten() else {
        return EMPTY_STANDING.to_owned();
    };
    let prefix = if header.starts_with("---\r\n") { 5 } else { 4 };
    let closing = header[..end].trim_end_matches(['\r', '\n']);
    let stop = closing.rfind('\n').map_or(prefix, |index| index + 1);
    let content = if stop >= prefix {
        &header[prefix..stop]
    } else {
        ""
    };
    let mut out = String::from("---\n");
    out.push_str(STANDING_YAML);
    out.push_str(content);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("---\n");
    out
}

/// The standing record the scaffold starts from: the input's when it has a
/// valid one, then the previous revision's, then the empty record. A header
/// that lacks a usable record is kept and given one rather than discarded.
fn merged_frontmatter(input: Option<&str>, previous: Option<&str>) -> String {
    if let Some(header) = input.filter(|text| matches!(parse_standing(text), Ok(Some(_)))) {
        return header.to_owned();
    }
    if let Some(header) = previous.filter(|text| matches!(parse_standing(text), Ok(Some(_)))) {
        return header.to_owned();
    }
    input
        .filter(|text| !text.trim().is_empty())
        .map_or_else(|| EMPTY_STANDING.to_owned(), with_standing)
}

fn push_section(out: &mut String, text: &str) {
    out.push_str(text.trim_start_matches('\n'));
    if !out.ends_with('\n') {
        out.push('\n');
    }
}

/// Build a template-shaped checkpoint from the new input, prefilled from the
/// current revision. Each section from `--file` wins over the same section in
/// the previous revision; a section present in neither is scaffolded empty.
/// The typed standing record carries forward unless the input supplies one.
/// Unknown sections are appended so a scaffold never drops owner content.
/// A first save has no previous revision and gets the empty scaffold.
pub fn scaffold_checkpoint(previous: Option<&str>, input: &str) -> String {
    let (previous_frontmatter, previous_sections) =
        previous.map(split).unwrap_or((None, Vec::new()));
    let (input_frontmatter, input_sections) = split(input);
    let mut out = String::new();
    push_section(
        &mut out,
        &merged_frontmatter(
            input_frontmatter.as_deref(),
            previous_frontmatter.as_deref(),
        ),
    );
    if let Some(title) = input_sections
        .iter()
        .find(|(name, _)| name.starts_with("# ") && !name.starts_with("## "))
        .or_else(|| {
            previous_sections
                .iter()
                .find(|(name, _)| name.starts_with("# ") && !name.starts_with("## "))
        })
        .map(|(_, text)| text.as_str())
    {
        push_section(&mut out, title);
    } else {
        out.push_str(TITLE);
        out.push_str("\n\n");
    }
    for heading in TEMPLATE_HEADINGS {
        match section(&input_sections, heading).or_else(|| section(&previous_sections, heading)) {
            Some(text) => push_section(&mut out, text),
            None => {
                out.push_str(heading);
                out.push_str("\n\n");
            }
        }
    }
    for (name, text) in previous_sections.iter().chain(input_sections.iter()) {
        if name.is_empty()
            || name.starts_with("# ")
            || TEMPLATE_HEADINGS.contains(&name.as_str())
            || out.contains(&format!("\n{name}\n"))
        {
            continue;
        }
        push_section(&mut out, text);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const REV5: &str = "# Session checkpoint\n\n## Goal\nLand the review fixes.\n\n## Plan\nResume on the fleet.\n";

    #[test]
    fn rev5_gaps_name_standing_and_intent_and_not_a_small_body() {
        let gaps = template_gaps(REV5);
        assert_eq!(gaps.len(), 2, "{gaps:?}");
        assert!(gaps[0].contains(STANDING_KEY));
        assert!(gaps[1].contains(INTENT));
        assert!(gaps.iter().all(|gap| !gap.contains("bytes")));
    }

    #[test]
    fn body_is_measured_after_frontmatter_and_names_the_gap() {
        let standing = EMPTY_STANDING;
        let template = format!(
            "{standing}# Session checkpoint\n\n## Intent\nKeep the console useful.\n\n## Goal\n{}\n",
            "x".repeat(TEMPLATE_BODY_BYTES)
        );
        let gaps = template_gaps(&template);
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert!(gaps[0].contains("body is"));
        assert!(gaps[0].contains("template budget 2048"));
        assert!(
            template_gaps(&format!(
                "{standing}# Session checkpoint\n\n## Intent\n{}\n",
                "x".repeat(TEMPLATE_BODY_BYTES - 64)
            ))
            .is_empty()
        );
    }

    #[test]
    fn invalid_standing_counts_as_a_gap_not_a_valid_record() {
        let invalid = "---\nprovenant_standing:\n  schema_version: 9\n---\n# T\n## Intent\nwhy\n";
        assert_eq!(template_gaps(invalid).len(), 1);
    }

    #[test]
    fn first_save_scaffolds_every_template_section() {
        let scaffold = scaffold_checkpoint(None, "## Goal\nLand #4029.\n");
        assert!(scaffold.starts_with(EMPTY_STANDING));
        for heading in TEMPLATE_HEADINGS {
            assert!(scaffold.contains(heading), "{scaffold}");
        }
        assert!(scaffold.contains("Land #4029."));
        assert!(template_gaps(&scaffold).is_empty(), "{scaffold}");
    }

    #[test]
    fn scaffold_prefills_from_previous_and_lets_input_win_per_section() {
        let previous = "---\nprovenant_standing:\n  schema_version: 1\n  goal: \"Land #4029\"\n  grants:\n    - scope: \"Land lane PRs after green CI\"\n      source: \"rule:GLOBAL-AGENTS.md#git\"\n      expires: none\n  obligations: []\n  waiting_on_owner: []\n---\n# Session checkpoint\n\n## Goal\nOld goal.\n\n## Intent\nOld reading.\n\n## Plan\nOld plan.\n\n## Background\nKeep this.\n";
        let scaffold = scaffold_checkpoint(
            Some(previous),
            "## Goal\nNew goal.\n\n## Direction\nNow named.\n",
        );
        assert!(scaffold.contains("Land lane PRs after green CI"));
        assert!(scaffold.contains("New goal."));
        assert!(!scaffold.contains("Old goal."));
        assert!(scaffold.contains("Old reading."));
        assert!(scaffold.contains("Old plan."));
        assert!(scaffold.contains("Now named."));
        assert!(scaffold.contains("Keep this."));
        assert!(template_gaps(&scaffold).is_empty(), "{scaffold}");
    }

    #[test]
    fn scaffold_keeps_other_frontmatter_keys_when_it_adds_a_standing_record() {
        let input = "---\nprovenant_summary:\n  schema_version: 1\n  now: \"Keep me\"\n---\n# Title\n\n## Intent\nA because B.\n";
        let scaffold = scaffold_checkpoint(None, input);
        assert!(scaffold.contains("Keep me"), "{scaffold}");
        assert!(template_gaps(&scaffold).is_empty(), "{scaffold}");
    }

    #[test]
    fn scaffold_accepts_stdin_shaped_input_without_losing_its_sections() {
        let scaffold = scaffold_checkpoint(None, "# Other title\n\n## Intent\nA because B.\n");
        assert!(scaffold.contains("# Other title"));
        assert!(scaffold.contains("A because B."));
        assert!(template_gaps(&scaffold).is_empty());
    }
}
