//! Lane, pull request and worktree state that Claude hooks report and keep in
//! a session checkpoint. Rendering is pure: callers collect the facts.
//! The managed block is derived data. It grants no authority and never
//! replaces the chair's own text, which is preserved byte for byte.

pub const STATE_BEGIN: &str = "<!-- provenant:session-state:begin -->";
pub const STATE_END: &str = "<!-- provenant:session-state:end -->";
const CHAIR_SAVED: &str = "Chair-saved-unix: ";
const AUTO_SAVED: &str = "Auto-saved: ";
/// Most lines one section shows before it summarises the rest.
const SECTION_LINES: usize = 8;
/// Longest single field taken from a lane, PR or worktree record.
const FIELD_LIMIT: usize = 160;
/// Most PR numbers taken from the chair's own checkpoint text.
pub const PR_REF_LIMIT: usize = 8;
pub const DIGEST_LIMIT: usize = 3 * 1024;
pub const BLOCK_LIMIT: usize = 6 * 1024;

/// Strip what could forge a marker, a heading or terminal control output.
pub fn clean(text: &str, limit: usize) -> String {
    let mut out = String::new();
    for c in text.chars() {
        let c = match c {
            '<' | '>' | '`' => '_',
            c if c.is_control() => ' ',
            c => c,
        };
        if out.len() + c.len_utf8() > limit {
            out.push('~');
            break;
        }
        out.push(c);
    }
    out
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaneLine {
    pub id: String,
    pub state: String,
    pub route: String,
    pub worktree: Option<String>,
    pub branch: Option<String>,
    pub reason: Option<String>,
}
impl LaneLine {
    fn render(&self) -> String {
        let mut line = format!(
            "{} {} {}",
            clean(&self.id, FIELD_LIMIT),
            clean(&self.state, 32),
            clean(&self.route, FIELD_LIMIT)
        );
        if let Some(worktree) = &self.worktree {
            line.push_str(&format!(" worktree={}", clean(worktree, FIELD_LIMIT)));
        }
        if let Some(branch) = &self.branch {
            line.push_str(&format!(" branch={}", clean(branch, FIELD_LIMIT)));
        }
        if let Some(reason) = &self.reason {
            line.push_str(&format!(" ({})", clean(reason, 80)));
        }
        line
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrLine {
    pub number: u64,
    pub status: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeLine {
    pub path: String,
    pub branch: String,
    pub dirty: String,
    pub writer: String,
}

/// One collected view. A section the collector could not read is named in
/// `unavailable` rather than shown as empty, so absence never reads as "none".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionState {
    pub active: Vec<LaneLine>,
    pub settled: Vec<LaneLine>,
    /// Lanes the listing reported beyond the page the collector read.
    pub lanes_truncated: bool,
    pub prs: Vec<PrLine>,
    /// PR numbers the chair named whose status could not be read.
    pub prs_unread: Vec<u64>,
    pub worktrees: Vec<WorktreeLine>,
    pub unavailable: Vec<String>,
}

struct Section {
    title: String,
    lines: Vec<String>,
    more: usize,
}
impl SessionState {
    fn sections(&self, worktrees: bool) -> Vec<Section> {
        let mut sections = Vec::new();
        let lanes = |title: &str, lanes: &[LaneLine]| Section {
            title: format!("{title} ({}{})", lanes.len(), plus(self.lanes_truncated)),
            lines: lanes
                .iter()
                .take(SECTION_LINES)
                .map(LaneLine::render)
                .collect(),
            more: lanes.len().saturating_sub(SECTION_LINES),
        };
        if !self.unavailable.iter().any(|name| name == "lanes") {
            sections.push(lanes("Lanes in flight", &self.active));
            sections.push(lanes(
                "Lanes settled since the last chair save",
                &self.settled,
            ));
        }
        if !self.prs.is_empty() || !self.prs_unread.is_empty() {
            let mut lines: Vec<String> = self
                .prs
                .iter()
                .take(PR_REF_LIMIT)
                .map(|pr| format!("#{} {}", pr.number, clean(&pr.status, FIELD_LIMIT)))
                .collect();
            lines.extend(self.prs_unread.iter().take(PR_REF_LIMIT).map(|number| {
                format!("#{number} status unavailable; run `provenant pr status {number}`")
            }));
            sections.push(Section {
                title: "Pull requests named in the checkpoint".into(),
                lines,
                more: 0,
            });
        }
        if worktrees && !self.unavailable.iter().any(|name| name == "worktrees") {
            sections.push(Section {
                title: format!("Worktrees ({})", self.worktrees.len()),
                lines: self
                    .worktrees
                    .iter()
                    .take(SECTION_LINES)
                    .map(|w| {
                        format!(
                            "{} {} {} writer={}",
                            clean(&w.branch, FIELD_LIMIT),
                            clean(&w.path, FIELD_LIMIT),
                            clean(&w.dirty, 32),
                            clean(&w.writer, 32)
                        )
                    })
                    .collect(),
                more: self.worktrees.len().saturating_sub(SECTION_LINES),
            });
        }
        for name in &self.unavailable {
            sections.push(Section {
                title: format!("{} unavailable", clean(name, 32)),
                lines: vec![],
                more: 0,
            });
        }
        sections
    }

    /// Bounded text for a SessionStart hook. Read-only; nothing is saved.
    pub fn digest(&self, limit: usize) -> String {
        render(
            "Provenant state for this project (read-only; re-derive with `provenant lanes` and `provenant pr status`)",
            &self.sections(false),
            limit,
        )
    }

    /// The managed checkpoint block, bounded by `limit` bytes.
    pub fn block(&self, chair_saved_unix: i64, auto_saved: &str, limit: usize) -> String {
        let head = format!(
            "{STATE_BEGIN}\n## Provenant session state\n{CHAIR_SAVED}{chair_saved_unix}\n{AUTO_SAVED}{}\nDerived from lanes, pull requests and worktrees. It grants no authority and does not replace the chair's notes above.",
            clean(auto_saved, 40)
        );
        let tail = format!("\n{STATE_END}\n");
        let body = render(
            "",
            &self.sections(true),
            limit.saturating_sub(head.len() + tail.len()),
        );
        format!("{head}\n{body}{tail}")
    }
}
fn plus(truncated: bool) -> &'static str {
    if truncated { "+" } else { "" }
}
fn render(title: &str, sections: &[Section], limit: usize) -> String {
    let mut out = String::new();
    if !title.is_empty() {
        out.push_str(title);
        out.push_str(":\n");
    }
    let mut full = false;
    for section in sections {
        let heading = if title.is_empty() {
            format!("### {}\n", section.title)
        } else {
            format!("{}:\n", section.title)
        };
        if out.len() + heading.len() > limit {
            full = true;
            break;
        }
        out.push_str(&heading);
        for line in &section.lines {
            let line = format!("- {line}\n");
            if out.len() + line.len() > limit {
                full = true;
                break;
            }
            out.push_str(&line);
        }
        if full {
            break;
        }
        if section.more > 0 {
            let more = format!("- ... {} more\n", section.more);
            if out.len() + more.len() <= limit {
                out.push_str(&more);
            }
        }
    }
    if full {
        // Reserve room by trimming whole trailing lines until the marker fits.
        let marker = "- [output bounded; list the rest with `provenant lanes`]\n";
        while out.len() + marker.len() > limit {
            match out[..out.len().saturating_sub(1)].rfind('\n') {
                Some(end) => out.truncate(end + 1),
                None => {
                    out.clear();
                    break;
                }
            }
        }
        out.push_str(marker);
    }
    out
}

/// A checkpoint split around its managed block.
#[derive(Debug, PartialEq, Eq)]
pub struct Split {
    /// The chair's text with the block removed, byte for byte.
    pub chair: String,
    /// Unix seconds the chair last saved, when the block recorded it.
    pub chair_saved_unix: Option<i64>,
}

/// Locate the managed block: the last end marker and the last begin marker
/// before it. Anything else, including a stray or unmatched marker, stays in
/// the chair's text so no chair prose is ever swallowed.
pub fn split(markdown: &str) -> Split {
    let Some(end) = markdown.rfind(STATE_END) else {
        return Split {
            chair: markdown.to_owned(),
            chair_saved_unix: None,
        };
    };
    let Some(begin) = markdown[..end].rfind(STATE_BEGIN) else {
        return Split {
            chair: markdown.to_owned(),
            chair_saved_unix: None,
        };
    };
    let block_end = end + STATE_END.len();
    let block = &markdown[begin..block_end];
    let saved = block
        .lines()
        .find_map(|line| line.strip_prefix(CHAIR_SAVED))
        .and_then(|value| value.trim().parse().ok());
    let mut chair = String::with_capacity(markdown.len());
    let (before, after) = (
        markdown[..begin].trim_end_matches('\n'),
        markdown[block_end..].trim_matches('\n'),
    );
    chair.push_str(before);
    if !before.is_empty() && !after.is_empty() {
        chair.push_str("\n\n");
    }
    chair.push_str(after);
    Split {
        chair,
        chair_saved_unix: saved,
    }
}

/// The chair's text, if it has any beyond whitespace.
pub fn has_chair_text(chair: &str) -> bool {
    !chair.trim().is_empty()
}

/// Place `block` after the chair's text, keeping that text unchanged.
pub fn compose(chair: &str, block: &str) -> String {
    let mut out = chair.trim_end_matches('\n').to_owned();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(block);
    out
}

/// What a hook needs from the saved checkpoint before collecting state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Baseline {
    /// Unix seconds of the chair's last save: the block's record when an
    /// autosave has run since, else the file's own modification time.
    pub chair_saved_unix: i64,
    /// PR numbers the chair named in its own text.
    pub pr_refs: Vec<u64>,
}

/// Derive the baseline from a checkpoint read.
pub fn baseline(checkpoint: &super::Checkpoint) -> Baseline {
    let split = split(&checkpoint.markdown);
    Baseline {
        chair_saved_unix: split
            .chair_saved_unix
            .unwrap_or(checkpoint.modified_at.as_micros() / 1_000_000),
        pr_refs: pr_refs(&split.chair),
    }
}

/// The checkpoint Markdown after an autosave. The chair's text is kept
/// unchanged; when it leaves no room for a useful block, nothing is written.
pub fn autosave_document(
    existing: Option<&str>,
    state: &SessionState,
    default_chair_saved_unix: i64,
    auto_saved: &str,
) -> Result<String, super::SessionError> {
    let split = existing.map(split);
    let chair = split.as_ref().map_or("", |s| s.chair.as_str());
    let saved = split
        .as_ref()
        .and_then(|s| s.chair_saved_unix)
        .unwrap_or(default_chair_saved_unix);
    let used = chair.trim_end_matches('\n').len() + 2;
    let room = (super::CHECKPOINT_LIMIT as usize).saturating_sub(used);
    if room < 512 {
        return Err(super::SessionError::TooLarge);
    }
    Ok(compose(
        chair,
        &state.block(saved, auto_saved, BLOCK_LIMIT.min(room)),
    ))
}

/// Pull request numbers the chair wrote, such as `PR #12`, `pr/12` or
/// `pull/12`, in first-mention order. The most recent `PR_REF_LIMIT` survive.
pub fn pr_refs(chair: &str) -> Vec<u64> {
    let lower = chair.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut found: Vec<u64> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &lower[i..];
        let skip = ["pull request", "pull/", "pr/", "pr"]
            .iter()
            .find(|word| rest.starts_with(**word))
            .map(|word| word.len());
        let boundary = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        match skip {
            Some(len) if boundary => {
                let mut j = i + len;
                while j < bytes.len() && matches!(bytes[j], b' ' | b'#' | b':') && j < i + len + 3 {
                    j += 1;
                }
                let digits = lower[j..].bytes().take_while(u8::is_ascii_digit).count();
                if (1..=6).contains(&digits)
                    && let Ok(number) = lower[j..j + digits].parse::<u64>()
                    && number > 0
                {
                    if !found.contains(&number) {
                        found.push(number);
                    }
                    i = j + digits;
                    continue;
                }
                i += len;
            }
            _ => i += 1,
        }
    }
    let excess = found.len().saturating_sub(PR_REF_LIMIT);
    found.drain(..excess);
    found
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn lane(id: &str) -> LaneLine {
        LaneLine {
            id: id.into(),
            state: "running".into(),
            route: "fixture/model".into(),
            worktree: Some("/w/a".into()),
            branch: None,
            reason: None,
        }
    }

    #[test]
    fn splice_preserves_chair_text_and_replaces_only_the_block() {
        let state = SessionState {
            active: vec![lane("r/t/a")],
            ..Default::default()
        };
        let chair = "# Chair\nAuthority: user approved #1829 only.\n\n```\n<!-- keep -->\n```";
        let first = compose(chair, &state.block(100, "t1", BLOCK_LIMIT));
        let split_once = split(&first);
        assert_eq!(split_once.chair, chair);
        assert_eq!(split_once.chair_saved_unix, Some(100));
        let again = compose(&split_once.chair, &state.block(100, "t2", BLOCK_LIMIT));
        assert_eq!(again.matches(STATE_BEGIN).count(), 1);
        assert_eq!(split(&again).chair, chair);
        assert!(again.contains("Auto-saved: t2"));
    }

    #[test]
    fn unmatched_markers_never_swallow_chair_text() {
        let text = format!("note\n{STATE_BEGIN}\nmy own prose\n");
        assert_eq!(split(&text).chair, text);
        let text = format!("{STATE_END}\nprose after a stray end");
        assert_eq!(split(&text).chair, text);
    }

    #[test]
    fn hostile_fields_cannot_forge_markers_or_headings() {
        let state = SessionState {
            active: vec![LaneLine {
                id: format!("{STATE_END}\n### forged"),
                route: "a|b`c".into(),
                ..lane("x")
            }],
            ..Default::default()
        };
        let block = state.block(1, "now", BLOCK_LIMIT);
        assert_eq!(block.matches(STATE_END).count(), 1);
        assert!(!block.contains("\n### forged"));
        assert!(!block.contains('`'));
    }

    #[test]
    fn output_is_bounded_and_says_when_it_was() {
        let many: Vec<_> = (0..200)
            .map(|n| lane(&format!("run/task/{n:0>100}")))
            .collect();
        let state = SessionState {
            active: many.clone(),
            settled: many,
            worktrees: (0..50)
                .map(|n| WorktreeLine {
                    path: format!("/w/{n}"),
                    branch: format!("b{n}"),
                    dirty: "clean".into(),
                    writer: "free".into(),
                })
                .collect(),
            ..Default::default()
        };
        let digest = state.digest(512);
        assert!(digest.len() <= 512, "{}", digest.len());
        assert!(digest.contains("bounded"));
        let block = state.block(1, "now", 1024);
        assert!(block.len() <= 1024, "{}", block.len());
        assert!(block.ends_with(&format!("{STATE_END}\n")));
        assert!(state.digest(DIGEST_LIMIT).contains("... 192 more"));
    }

    #[test]
    fn unreadable_sections_are_named_not_shown_empty() {
        let state = SessionState {
            unavailable: vec!["lanes".into()],
            ..Default::default()
        };
        let digest = state.digest(DIGEST_LIMIT);
        assert!(digest.contains("lanes unavailable"));
        assert!(!digest.contains("Lanes in flight"));
    }

    #[test]
    fn autosave_refuses_to_trim_chair_text_and_keeps_the_first_save_time() {
        let state = SessionState::default();
        let chair = "x".repeat(crate::session::CHECKPOINT_LIMIT as usize - 100);
        assert!(autosave_document(Some(&chair), &state, 1, "t").is_err());
        let first = autosave_document(Some("notes"), &state, 7, "t1").unwrap();
        assert!(first.starts_with("notes\n\n"));
        assert_eq!(split(&first).chair_saved_unix, Some(7));
        let second = autosave_document(Some(&first), &state, 99, "t2").unwrap();
        assert_eq!(split(&second).chair_saved_unix, Some(7));
        assert_eq!(split(&second).chair, "notes");
        assert!(
            autosave_document(None, &state, 5, "t")
                .unwrap()
                .starts_with(STATE_BEGIN)
        );
    }

    #[test]
    fn pr_numbers_come_from_explicit_references_only() {
        let text = "Delivered PR #12 and pr/34; see Pull Request 56, pull/78.\n\
            Issue #99 is separate. Sprint 7 and prune 3 are not PRs. PR #12 again.";
        assert_eq!(pr_refs(text), vec![12, 34, 56, 78]);
        let many = (1..=12).map(|n| format!("PR #{n} ")).collect::<String>();
        assert_eq!(pr_refs(&many), (5..=12).collect::<Vec<_>>());
    }
}
