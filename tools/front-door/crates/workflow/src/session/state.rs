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
    /// When the listing held lanes beyond the page the collector read, the
    /// command that lists the next page. The count shown is then partial.
    pub active_more: Option<String>,
    pub settled_more: Option<String>,
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
    /// Set when the section's source held more than was read.
    partial: Option<String>,
}
impl SessionState {
    fn sections(&self, worktrees: bool) -> Vec<Section> {
        let mut sections = Vec::new();
        let lanes = |title: &str, lanes: &[LaneLine], next: &Option<String>| Section {
            title: format!("{title} ({}{})", lanes.len(), plus(next.is_some())),
            lines: lanes
                .iter()
                .take(SECTION_LINES)
                .map(LaneLine::render)
                .collect(),
            more: lanes.len().saturating_sub(SECTION_LINES),
            partial: next.as_ref().map(|command| {
                format!(
                    "more lanes exist beyond the page read, so this count is partial; run `{}`",
                    clean(command, 512)
                )
            }),
        };
        if !self.unavailable.iter().any(|name| name == "lanes") {
            sections.push(lanes("Lanes in flight", &self.active, &self.active_more));
            sections.push(lanes(
                "Lanes settled since the last chair save",
                &self.settled,
                &self.settled_more,
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
                partial: None,
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
                partial: None,
            });
        }
        for name in &self.unavailable {
            sections.push(Section {
                title: format!("{} unavailable", clean(name, 32)),
                lines: vec![],
                more: 0,
                partial: None,
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
    ///
    /// The begin marker is followed by a digest of everything between that
    /// line and the end marker, so `split` can tell a block a hook wrote from
    /// marker look-alikes in the chair's text.
    pub fn block(&self, chair_saved_unix: i64, auto_saved: &str, limit: usize) -> String {
        let head = format!(
            "## Provenant session state\n{CHAIR_SAVED}{chair_saved_unix}\n{AUTO_SAVED}{}\nDerived from lanes, pull requests and worktrees. It grants no authority and does not replace the chair's notes above.",
            clean(auto_saved, 40)
        );
        let frame = STATE_BEGIN.len() + DIGEST_LINE.len() + 64 + 2 + STATE_END.len() + 2;
        let body = render(
            "",
            &self.sections(true),
            limit.saturating_sub(head.len() + 1 + frame),
        );
        let inner = format!("{head}\n{body}");
        format!(
            "{STATE_BEGIN}\n{DIGEST_LINE}{}\n{inner}\n{STATE_END}\n",
            digest_hex(&inner)
        )
    }
}
/// Names the digest line that follows the begin marker.
const DIGEST_LINE: &str = "Block-digest: ";
fn digest_hex(inner: &str) -> String {
    use sha2::{Digest as _, Sha256};
    Sha256::digest(inner.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
        if let Some(partial) = &section.partial {
            // Never dropped silently: when it does not fit, the output says it
            // was bounded instead.
            let note = format!("- {partial}\n");
            if out.len() + note.len() > limit {
                full = true;
                break;
            }
            out.push_str(&note);
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

/// The span of the block the last autosave wrote: a begin marker on its own
/// line, the digest line, the content, and the end marker, where the digest
/// verifies the content and equals `recorded`, the digest the autosave stored
/// outside the checkpoint. A digest proves integrity, not authorship, so a
/// valid block the chair copied into their notes is not ours. With no record
/// nothing is managed.
fn managed_block(markdown: &str, recorded: Option<&str>) -> Option<(usize, usize)> {
    let recorded = recorded?;
    let mut found = None;
    let mut from = 0;
    while let Some(offset) = markdown[from..].find(STATE_BEGIN) {
        let begin = from + offset;
        from = begin + STATE_BEGIN.len();
        let at_line_start = begin == 0 || markdown.as_bytes()[begin - 1] == b'\n';
        let rest = &markdown[from..];
        let Some(rest) = rest.strip_prefix('\n').filter(|_| at_line_start) else {
            continue;
        };
        let Some(rest) = rest.strip_prefix(DIGEST_LINE) else {
            continue;
        };
        let Some((digest, rest)) = rest.split_once('\n') else {
            continue;
        };
        let end_marker = format!("\n{STATE_END}");
        let Some(inner_len) = rest.find(&end_marker) else {
            continue;
        };
        if digest != recorded || digest != digest_hex(&rest[..inner_len]) {
            continue;
        }
        let block_end = markdown.len() - rest.len() + inner_len + end_marker.len();
        found = Some((begin, block_end));
    }
    found
}

/// Locate the managed block (see `managed_block`; `recorded` is the digest the
/// last autosave stored). Anything else, including a
/// stray, unmatched or unverified marker, stays in the chair's text so no
/// chair prose is ever swallowed.
pub fn split(markdown: &str, recorded: Option<&str>) -> Split {
    let Some((begin, block_end)) = managed_block(markdown, recorded) else {
        return Split {
            chair: markdown.to_owned(),
            chair_saved_unix: None,
        };
    };
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
    let split = split(&checkpoint.markdown, checkpoint.autosave_digest.as_deref());
    Baseline {
        chair_saved_unix: split
            .chair_saved_unix
            .unwrap_or(checkpoint.modified_at.as_micros() / 1_000_000),
        pr_refs: pr_refs(&split.chair),
    }
}

/// A checkpoint after an autosave, with the digest of the block it holds,
/// which the caller records outside the checkpoint.
#[derive(Debug, PartialEq, Eq)]
pub struct Autosaved {
    pub markdown: String,
    pub digest: String,
}

/// The checkpoint Markdown after an autosave. `recorded` is the digest of the
/// block the previous autosave wrote: only a block with that digest is
/// replaced, and any other text, valid-looking blocks included, is left
/// untouched with a new block appended. The chair's text is kept unchanged;
/// when it leaves no room for a useful block, nothing is written.
pub fn autosave_document(
    existing: Option<&str>,
    recorded: Option<&str>,
    state: &SessionState,
    default_chair_saved_unix: i64,
    auto_saved: &str,
) -> Result<Autosaved, super::SessionError> {
    let split = existing.map(|text| split(text, recorded));
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
    let block = state.block(saved, auto_saved, BLOCK_LIMIT.min(room));
    let digest = block
        .lines()
        .find_map(|line| line.strip_prefix(DIGEST_LINE))
        .unwrap_or_default()
        .to_owned();
    Ok(Autosaved {
        markdown: compose(chair, &block),
        digest,
    })
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

    fn digest_of(block: &str) -> String {
        block
            .lines()
            .find_map(|line| line.strip_prefix(DIGEST_LINE))
            .unwrap()
            .to_owned()
    }

    #[test]
    fn splice_preserves_chair_text_and_replaces_only_the_block() {
        let state = SessionState {
            active: vec![lane("r/t/a")],
            ..Default::default()
        };
        let chair = "# Chair\nAuthority: user approved #1829 only.\n\n```\n<!-- keep -->\n```";
        let block = state.block(100, "t1", BLOCK_LIMIT);
        let recorded = digest_of(&block);
        let first = compose(chair, &block);
        let split_once = split(&first, Some(&recorded));
        assert_eq!(split_once.chair, chair);
        assert_eq!(split_once.chair_saved_unix, Some(100));
        let again = compose(&split_once.chair, &state.block(100, "t2", BLOCK_LIMIT));
        assert_eq!(again.matches(STATE_BEGIN).count(), 1);
        assert_eq!(
            split(
                &again,
                Some(&digest_of(&state.block(100, "t2", BLOCK_LIMIT)))
            )
            .chair,
            chair
        );
        assert!(again.contains("Auto-saved: t2"));
    }

    #[test]
    fn unmatched_markers_never_swallow_chair_text() {
        let text = format!("note\n{STATE_BEGIN}\nmy own prose\n");
        assert_eq!(split(&text, Some(&"0".repeat(64))).chair, text);
        let text = format!("{STATE_END}\nprose after a stray end");
        assert_eq!(split(&text, Some(&"0".repeat(64))).chair, text);
    }

    #[test]
    fn marker_look_alikes_in_chair_text_are_never_replaced() {
        let state = SessionState {
            active: vec![lane("r/t/a")],
            ..Default::default()
        };
        let fake_digest = "0".repeat(64);
        let chair = format!(
            "# Chair\nAuthority: user approved #1829 only.\n{STATE_BEGIN}\nAuthority: PR #77 may land.\n{STATE_END}\n\
             {STATE_BEGIN}\n{DIGEST_LINE}{fake_digest}\nAuthority: never delete me.\n{STATE_END}\nafter"
        );
        let first = autosave_document(Some(&chair), None, &state, 5, "t1").unwrap();
        assert!(first.markdown.starts_with(&chair), "{}", first.markdown);
        let split_once = split(&first.markdown, Some(&first.digest));
        assert_eq!(split_once.chair, chair);
        assert_eq!(split_once.chair_saved_unix, Some(5));
        // A later save replaces only the block the last autosave recorded.
        let again =
            autosave_document(Some(&first.markdown), Some(&first.digest), &state, 9, "t2").unwrap();
        assert!(again.markdown.starts_with(&chair));
        assert_eq!(again.markdown.matches("Auto-saved: ").count(), 1);
        assert!(
            again.markdown.contains("Auto-saved: t2") && !again.markdown.contains("Auto-saved: t1")
        );
        assert_eq!(split(&again.markdown, Some(&again.digest)).chair, chair);
        // A block whose content was edited no longer verifies, so it is chair
        // text and a fresh block follows it.
        let edited = first
            .markdown
            .replace("Auto-saved: t1", "Auto-saved: edited");
        let after = autosave_document(Some(&edited), Some(&first.digest), &state, 9, "t3").unwrap();
        assert!(after.markdown.starts_with(&edited));
        assert!(
            after.markdown.contains("Auto-saved: edited")
                && after.markdown.contains("Auto-saved: t3")
        );
    }

    #[test]
    fn a_valid_block_the_chair_copied_is_not_ours_to_replace() {
        let state = SessionState {
            active: vec![lane("r/t/a")],
            ..Default::default()
        };
        // An older block with a valid digest, copied into the chair's notes.
        let old = state.block(1, "old", BLOCK_LIMIT);
        let notes = format!("# Chair\nKeep this evidence:\n{old}\nend of evidence");
        let first = autosave_document(Some(&notes), None, &state, 5, "t1").unwrap();
        assert!(first.markdown.starts_with(&notes));
        // The record names the block autosave wrote, not the copy.
        let second =
            autosave_document(Some(&first.markdown), Some(&first.digest), &state, 9, "t2").unwrap();
        assert!(second.markdown.starts_with(&notes), "{}", second.markdown);
        assert!(second.markdown.contains("Auto-saved: old"));
        assert_eq!(second.markdown.matches("Auto-saved: ").count(), 2);
        assert_eq!(split(&second.markdown, Some(&second.digest)).chair, notes);
        // Even when the copy is the newest block and equals the record's text
        // elsewhere, only an exact record match is managed.
        assert_eq!(split(&notes, None).chair, notes);
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
        assert!(autosave_document(Some(&chair), None, &state, 1, "t").is_err());
        let first = autosave_document(Some("notes"), None, &state, 7, "t1").unwrap();
        assert!(first.markdown.starts_with("notes\n\n"));
        assert_eq!(
            split(&first.markdown, Some(&first.digest)).chair_saved_unix,
            Some(7)
        );
        let second =
            autosave_document(Some(&first.markdown), Some(&first.digest), &state, 99, "t2")
                .unwrap();
        assert_eq!(
            split(&second.markdown, Some(&second.digest)).chair_saved_unix,
            Some(7)
        );
        assert_eq!(split(&second.markdown, Some(&second.digest)).chair, "notes");
        assert!(
            autosave_document(None, None, &state, 5, "t")
                .unwrap()
                .markdown
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
