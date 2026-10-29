//! A pull request body built from receipts: the repository's template with its
//! guidance comments removed and its sections filled from recorded facts.
//! The template keeps owning the shape; only the sections named here are
//! filled, and missing sections are appended from the bundled template.
use crate::run::AttemptFacts;
use provenant_core::GitOid;

/// What the body states beyond the attempts.
pub struct Body<'a> {
    pub summary: &'a str,
    pub issues: &'a [u64],
    pub base: &'a str,
    pub base_sha: Option<&'a str>,
    pub branch: &'a str,
    pub head: GitOid,
    pub author: &'a AttemptFacts,
    pub reviews: &'a [AttemptFacts],
}

const SECTIONS: [&str; 5] = [
    "Summary",
    "Decision requested",
    "Risk and rollback",
    "Evidence",
    "Independent review",
];
const TITLE_LIMIT: usize = 100;

/// The pull request title and the summary text without a title heading. A first
/// line that is a Markdown heading is the title; otherwise it is also summary.
pub fn split_summary(summary: &str) -> (String, String) {
    let trimmed = summary.trim();
    let first = trimmed.lines().next().unwrap_or_default();
    let heading = first.starts_with('#');
    let title: String = first
        .trim_start_matches('#')
        .trim()
        .chars()
        .take(TITLE_LIMIT)
        .collect();
    let body = if heading {
        trimmed[first.len()..].trim().to_owned()
    } else {
        trimmed.to_owned()
    };
    (title, body)
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}
fn collapse_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank = 0;
    for line in text.lines() {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
            out.push('\n');
        } else {
            blank = 0;
            out.push_str(line.trim_end());
            out.push('\n');
        }
    }
    out.trim_end().to_owned() + "\n"
}
fn code(text: &str) -> String {
    format!("`{}`", text.replace('`', "'"))
}
fn exit_text(facts: &AttemptFacts) -> String {
    match facts.exit_code {
        Some(code) => format!("{} exit {code}", facts.state),
        None => facts.state.clone(),
    }
}
fn head_text(facts: &AttemptFacts) -> String {
    facts
        .head
        .map_or_else(|| "unknown".into(), |head| head.to_string())
}

/// Fill the template. Every filled value is a recorded fact or says that none
/// was recorded.
pub fn fill(template: &str, body: &Body<'_>) -> Result<String, String> {
    let template = strip_comments(template);
    let mut sections: Vec<(Option<String>, Vec<&str>)> = vec![(None, Vec::new())];
    for line in template.lines() {
        if let Some(name) = line.strip_prefix("## ") {
            sections.push((Some(name.trim().to_owned()), Vec::new()));
        }
        if let Some((_, lines)) = sections.last_mut() {
            lines.push(line);
        }
    }
    let bundled = strip_comments(include_str!(
        "../../../../../.github/pull_request_template.md"
    ));
    for wanted in SECTIONS {
        if !sections
            .iter()
            .any(|(name, _)| name.as_deref() == Some(wanted))
        {
            let heading = format!("## {wanted}");
            let lines = bundled
                .lines()
                .skip_while(|line| *line != heading)
                .enumerate()
                .take_while(|(index, line)| *index == 0 || !line.starts_with("## "))
                .map(|(_, line)| line)
                .collect();
            sections.push((Some(wanted.to_owned()), lines));
        }
    }
    let mut out = String::new();
    for (name, lines) in &sections {
        let text = match name.as_deref() {
            Some("Summary") => summary(body),
            Some("Decision requested") => decision(body),
            Some("Risk and rollback") => risk(body),
            Some("Evidence") => evidence(lines, body),
            Some("Independent review") => review(lines, body),
            _ => lines.join("\n") + "\n",
        };
        out.push_str(&text);
        out.push('\n');
    }
    Ok(collapse_blank_lines(&out))
}

fn summary(body: &Body<'_>) -> String {
    let mut text = format!("## Summary\n\n{}\n", body.summary.trim());
    if !body.issues.is_empty() {
        text.push('\n');
        for issue in body.issues {
            text.push_str(&format!("Closes #{issue}\n"));
        }
    }
    text
}
fn decision(body: &Body<'_>) -> String {
    format!(
        "## Decision requested\n\nApprove merge of {} to {}. Open gates: none recorded.\n",
        code(&body.head.to_string()),
        code(body.base)
    )
}
fn risk(body: &Body<'_>) -> String {
    let tier = body.author.risk.as_deref().map_or_else(
        || "not recorded (the attempt has no delivery receipt)".to_owned(),
        |tier| format!("{tier} (delivery receipt)"),
    );
    format!(
        "## Risk and rollback\n\nRisk tier: {tier}. Rollback: revert the merge commit on {}.\n",
        code(body.base)
    )
}

/// Keep the template's prose and gate rows; fill each row's empty cells and add
/// a row for the attempt and each review.
fn evidence(lines: &[&str], body: &Body<'_>) -> String {
    let head = body.head.to_string();
    let mut out = String::from("## Evidence\n\n");
    out.push_str(&format!(
        "Base: {} at {}. Head under review: {} at {}.\n",
        code(body.base),
        code(body.base_sha.unwrap_or("unknown")),
        code(body.branch),
        code(&head)
    ));
    for line in &lines[1..] {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with('|')
            || line.starts_with("Base:")
            || line.starts_with("## ")
        {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
    let mut rows = lines
        .iter()
        .map(|line| line.trim())
        .filter(|line| line.starts_with('|'));
    let (Some(header), Some(rule)) = (rows.next(), rows.next()) else {
        return out;
    };
    out.push_str(header);
    out.push('\n');
    out.push_str(rule);
    out.push('\n');
    for row in rows {
        let mut cells: Vec<String> = row
            .trim_matches('|')
            .split('|')
            .map(|cell| cell.trim().to_owned())
            .collect();
        cells.resize(5, String::new());
        if cells[1].is_empty() {
            cells[1] = "not recorded".into();
        }
        if cells[2].is_empty() {
            cells[2] = "pending".into();
        }
        if cells[3].is_empty() {
            cells[3] = code(&head);
        }
        if cells[4].is_empty() {
            cells[4] = "none".into();
        }
        out.push_str(&format!("| {} |\n", cells.join(" | ")));
    }
    let attempt_row = |label: &str, facts: &AttemptFacts| {
        format!(
            "| {label} | {} | {}; route {} | {} | none |\n",
            code(&format!(
                "provenant output {} --part result",
                facts.attempt_id
            )),
            exit_text(facts),
            facts.route,
            code(&head_text(facts)),
        )
    };
    out.push_str(&attempt_row("Writer attempt", body.author));
    for review in body.reviews {
        out.push_str(&attempt_row("Independent review", review));
    }
    out
}

fn review(lines: &[&str], body: &Body<'_>) -> String {
    let author = format!(
        "{} (receipt {})",
        code(&body.author.route),
        code(&body.author.receipt_path)
    );
    let join = |each: &dyn Fn(&AttemptFacts) -> String, none: &str| {
        if body.reviews.is_empty() {
            none.to_owned()
        } else {
            body.reviews.iter().map(each).collect::<Vec<_>>().join("; ")
        }
    };
    let head = body.head.to_string();
    let values = [
        ("Author route", author),
        (
            "Reviewer route",
            join(
                &|r| format!("{} (receipt {})", code(&r.route), code(&r.receipt_path)),
                "none recorded",
            ),
        ),
        (
            "Reviewer role",
            join(&|_| "independent review lane".into(), "none recorded"),
        ),
        (
            "Independence from authorship/implementation context",
            join(
                &|r| {
                    let same = if r.route == body.author.route {
                        "the same route as"
                    } else {
                        "a different route from"
                    };
                    format!(
                        "separate attempt {} with its own prompt, on {same} the author",
                        r.attempt_id
                    )
                },
                "none recorded",
            ),
        ),
        (
            "Exact head reviewed",
            join(
                &|r| match r.head {
                    Some(reviewed) if reviewed.to_string() == head => code(&reviewed.to_string()),
                    Some(reviewed) => {
                        format!(
                            "{} (differs from the head under review)",
                            code(&reviewed.to_string())
                        )
                    }
                    None => "not recorded".into(),
                },
                "none recorded",
            ),
        ),
        (
            "Unresolved P0-P2 findings",
            join(
                &|r| {
                    format!(
                        "not recorded; read {}",
                        code(&format!("provenant output {} --part result", r.attempt_id))
                    )
                },
                "none recorded",
            ),
        ),
    ];
    let mut out = String::new();
    let mut in_details = false;
    for line in lines {
        let bullet = line.strip_prefix("- ").and_then(|rest| {
            values
                .iter()
                .find(|(label, _)| rest.starts_with(&format!("{label}:")))
        });
        if let Some((label, value)) = bullet {
            out.push_str(&format!("- {label}: {value}\n"));
            continue;
        }
        out.push_str(line);
        out.push('\n');
        if line.trim().starts_with("<details>") {
            in_details = true;
        }
        if in_details && line.contains("</summary>") {
            out.push_str(&format!("\n{}\n", detail(body)));
            in_details = false;
        }
    }
    out
}
fn detail(body: &Body<'_>) -> String {
    let author = body.author;
    let delivered = author.branch.as_ref().map_or_else(String::new, |branch| {
        format!(" It delivered {}.", code(branch))
    });
    format!(
        "Attempt {} (run {}, task {}) ended {}.{delivered} The receipt and result are under {}.",
        author.attempt_id,
        author.run_id,
        author.task_id,
        exit_text(author),
        code(&author.receipt_path),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::{AttemptId, RunId, TaskId};

    const TEMPLATE: &str = include_str!("../../../../../.github/pull_request_template.md");
    const HEAD: &str = "1111111111111111111111111111111111111111";
    const REVIEWED: &str = "2222222222222222222222222222222222222222";

    fn facts(attempt: u8, route: &str, head: &str) -> AttemptFacts {
        AttemptFacts {
            attempt_id: AttemptId::from_bytes([attempt; 16]),
            run_id: RunId::from_bytes([9; 16]),
            task_id: TaskId::from_bytes([8; 16]),
            state: "succeeded".into(),
            exit_code: Some(0),
            route: route.into(),
            head: GitOid::parse(head).ok(),
            branch: Some("feat/thing".into()),
            delivery_id: None,
            receipt_path: format!("runs/r/attempts/{attempt}"),
            risk: Some("substantial".into()),
            reviews: Vec::new(),
        }
    }
    fn body<'a>(author: &'a AttemptFacts, reviews: &'a [AttemptFacts]) -> Body<'a> {
        Body {
            summary: "Add the thing.\n\nIt does it.",
            issues: &[12, 13],
            base: "main",
            base_sha: Some("abcdef0"),
            branch: "feat/thing",
            head: GitOid::parse(HEAD).unwrap(),
            author,
            reviews,
        }
    }

    #[test]
    fn the_repository_template_is_filled_from_the_receipts() {
        let author = facts(1, "codex/gpt-6@high", HEAD);
        let reviews = [facts(2, "claude/opus@high", HEAD)];
        let text = fill(TEMPLATE, &body(&author, &reviews)).unwrap();
        assert!(!text.contains("<!--"), "{text}");
        for needle in [
            "## Summary\n\nAdd the thing.\n\nIt does it.\n\nCloses #12\nCloses #13\n",
            &format!("Approve merge of `{HEAD}` to `main`."),
            "Risk tier: substantial (delivery receipt). Rollback: revert the merge commit on `main`.",
            &format!("Base: `main` at `abcdef0`. Head under review: `feat/thing` at `{HEAD}`."),
            "- Author route: `codex/gpt-6@high` (receipt `runs/r/attempts/1`)",
            "- Reviewer route: `claude/opus@high` (receipt `runs/r/attempts/2`)",
            &format!("- Exact head reviewed: `{HEAD}`\n"),
            "<summary>Implementation detail</summary>\n\nAttempt 01010101010101010101010101010101",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in\n{text}");
        }
        // Every table row is complete, and the attempts are rows.
        let rows: Vec<&str> = text.lines().filter(|line| line.starts_with('|')).collect();
        assert!(rows.len() >= 8, "{rows:?}");
        assert!(rows.iter().all(|row| !row.contains("|  |")), "{rows:?}");
        let harness = rows
            .iter()
            .find_map(|row| {
                let cells: Vec<_> = row.split('|').map(str::trim).collect();
                (cells.get(1) == Some(&"Harness style")).then_some(cells)
            })
            .expect("harness style row");
        assert_eq!(&harness[2..4], ["`scripts/check-harness`", "pending"]);
        assert!(text.contains("| Writer attempt |"));
        assert!(text.contains("| Independent review |"));
    }

    #[test]
    fn a_review_of_another_head_or_no_review_is_stated_not_hidden() {
        let author = facts(1, "codex/gpt-6@high", HEAD);
        let none = fill(TEMPLATE, &body(&author, &[])).unwrap();
        assert!(none.contains("- Reviewer route: none recorded"), "{none}");
        let reviews = [facts(2, "codex/gpt-6@high", REVIEWED)];
        let stale = fill(TEMPLATE, &body(&author, &reviews)).unwrap();
        assert!(
            stale.contains("(differs from the head under review)"),
            "{stale}"
        );
        assert!(stale.contains("on the same route as the author"), "{stale}");
    }

    #[test]
    fn a_template_missing_sections_appends_them_with_recorded_facts() {
        let author = facts(1, "a/b", HEAD);
        let text = fill(
            "Custom intro\n\n## Summary\n\ntext\n\n## Project notes\n\nKeep me.\n",
            &body(&author, &[]),
        )
        .unwrap();
        for section in SECTIONS {
            assert_eq!(
                text.matches(&format!("## {section}\n")).count(),
                1,
                "{text}"
            );
        }
        assert!(text.starts_with("Custom intro\n"), "{text}");
        assert!(text.contains("## Project notes\n\nKeep me."), "{text}");
        assert!(
            text.contains(&format!("Approve merge of `{HEAD}`")),
            "{text}"
        );
        assert!(text.contains("| Writer attempt |"), "{text}");
        assert!(text.contains("- Author route: `a/b`"), "{text}");
        assert!(
            fill("", &body(&author, &[]))
                .unwrap()
                .contains("## Evidence")
        );
    }

    #[test]
    fn titles_come_from_a_heading_or_the_first_line() {
        assert_eq!(
            split_summary("# feat(cli): add it\n\nBody text."),
            ("feat(cli): add it".into(), "Body text.".into())
        );
        assert_eq!(
            split_summary("\nfeat(cli): add it\nmore"),
            ("feat(cli): add it".into(), "feat(cli): add it\nmore".into())
        );
        assert_eq!(split_summary(&"x".repeat(300)).0.len(), TITLE_LIMIT);
    }
}
