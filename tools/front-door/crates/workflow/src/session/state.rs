//! Lane, pull request and worktree state that Claude hooks report and save
//! beside a session checkpoint. Rendering is pure: callers collect the facts.
//! The saved state is derived data in a file only hooks write. It grants no
//! authority, and no autosave ever opens the chair's checkpoint for writing.

const AUTO_SAVED: &str = "Auto-saved: ";
/// Most lines one section shows before it summarises the rest.
const SECTION_LINES: usize = 8;
/// Longest single field taken from a lane, PR or worktree record.
const FIELD_LIMIT: usize = 160;
/// Most PR numbers taken from the chair's own checkpoint text.
pub const PR_REF_LIMIT: usize = 16;
pub const DIGEST_LIMIT: usize = 3 * 1024;
pub const BLOCK_LIMIT: usize = 6 * 1024;
/// Longest next-page command shown as a runnable command.
const NEXT_PAGE_LIMIT: usize = 512;

/// Starts a note that is shown as plain text instead of as a command.
const PLAIN_NOTE: &str = "page with: ";

/// `provenant` plus the non-default state directory a hook ran against, so
/// every command a hook shows reads or writes the store the hook used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPrefix {
    text: String,
    /// False when the directory cannot be quoted faithfully (not UTF-8, or
    /// holds control characters): no runnable command may then be shown.
    shown: bool,
}
impl Default for CommandPrefix {
    fn default() -> Self {
        Self {
            text: "provenant".into(),
            shown: true,
        }
    }
}
impl CommandPrefix {
    /// A relative directory is made absolute against `cwd` (where the hook
    /// ran) and quoted for a POSIX shell.
    pub fn new(state_dir: Option<&std::path::Path>, cwd: &std::path::Path) -> Self {
        let mut prefix = Self::default();
        if let Some(state) = state_dir {
            match cwd
                .join(state)
                .to_str()
                .filter(|s| !s.chars().any(char::is_control))
            {
                Some(text) => {
                    prefix.text.push_str(" --state-dir ");
                    prefix.text.push_str(&shell_quote(text));
                }
                None => prefix.shown = false,
            }
        }
        prefix
    }

    /// `provenant [--state-dir DIR] {rest}` only when it would survive
    /// rendering byte for byte: `clean` rewrites <, > and backticks and
    /// truncates, which could point at another store. Otherwise `None`.
    pub fn runnable(&self, rest: &str) -> Option<String> {
        let command = format!("{} {rest}", self.text);
        (self.shown && clean(&command, NEXT_PAGE_LIMIT) == command).then_some(command)
    }

    /// The command, or a plain note when the directory cannot be shown safely.
    pub fn command(&self, rest: &str) -> String {
        let command = format!("{} {rest}", self.text);
        if self.shown {
            command
        } else {
            format!(
                "{PLAIN_NOTE}{command} and the --state-dir this hook used (its path cannot be shown safely)"
            )
        }
    }
}
/// Single-quote `text` for a POSIX shell: a quote becomes `'\''`.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

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
    pub state: String,
    pub summary: String,
}
/// A named PR whose status could not be read, and the command that reads it
/// from the same store the collector used (or a plain note when none is safe).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrUnread {
    pub number: u64,
    pub command: String,
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
    /// command that lists the next page, or a plain note on how to when no safe
    /// command exists. The count shown is then partial.
    pub active_more: Option<String>,
    pub settled_more: Option<String>,
    pub prs: Vec<PrLine>,
    /// PR numbers the chair named whose status could not be read.
    pub prs_unread: Vec<PrUnread>,
    pub worktrees: Vec<WorktreeLine>,
    pub unavailable: Vec<String>,
    /// How commands the output shows reach the store the collector used.
    pub commands: CommandPrefix,
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
        let lanes = |title: &str, lanes: &[LaneLine], next: &Option<String>| {
            Section {
            title: format!("{title} ({}{})", lanes.len(), plus(next.is_some())),
            lines: lanes
                .iter()
                .take(SECTION_LINES)
                .map(LaneLine::render)
                .collect(),
            more: lanes.len().saturating_sub(SECTION_LINES),
            partial: next.as_ref().map(|command| {
                // A runnable command is shown as code, and only when it
                // survives rendering byte for byte: `clean` rewrites <, > and
                // backticks and truncates, which could point at another
                // store. Anything else is a plain note.
                let how = if !command.starts_with("provenant ") {
                    clean(command, NEXT_PAGE_LIMIT)
                } else if clean(command, NEXT_PAGE_LIMIT) == *command {
                    format!("run `{command}`")
                } else {
                    let cursor: String = command
                        .rsplit_once("--cursor ")
                        .map(|(_, tail)| tail.chars().take_while(char::is_ascii_digit).collect())
                        .unwrap_or_default();
                    format!(
                        "the paging command is too long or unsafe to show; run provenant lanes with this hook's --state-dir and --cursor {cursor}"
                    )
                };
                format!("more lanes exist beyond the page read, so this count is partial; {how}")
            }),
        }
        };
        if !self.unavailable.iter().any(|name| name == "lanes") {
            sections.push(lanes("Lanes in flight", &self.active, &self.active_more));
            let mut settled = lanes(
                "Lanes settled since the last chair save",
                &self.settled,
                &self.settled_more,
            );
            let mut counts = std::collections::BTreeMap::new();
            for lane in &self.settled {
                *counts.entry(clean(&lane.state, 32)).or_insert(0usize) += 1;
            }
            let failures: Vec<_> = self
                .settled
                .iter()
                .filter(|lane| lane.state != "succeeded")
                .collect();
            settled.lines = if counts.is_empty() {
                vec![]
            } else {
                vec![
                    counts
                        .iter()
                        .map(|(state, count)| format!("{state}: {count}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                ]
            };
            settled.lines.extend(
                failures
                    .iter()
                    .take(SECTION_LINES)
                    .map(|lane| lane.render()),
            );
            settled.more = failures.len().saturating_sub(SECTION_LINES);
            sections.push(settled);
        }
        if !self.prs.is_empty() || !self.prs_unread.is_empty() {
            let mut lines: Vec<String> = self
                .prs
                .iter()
                .filter(|pr| pr.state.eq_ignore_ascii_case("open"))
                .map(|pr| format!("#{} {}", pr.number, clean(&pr.summary, FIELD_LIMIT)))
                .collect();
            let merged = self
                .prs
                .iter()
                .filter(|pr| pr.state.eq_ignore_ascii_case("merged"))
                .count();
            let closed = self
                .prs
                .iter()
                .filter(|pr| pr.state.eq_ignore_ascii_case("closed"))
                .count();
            if merged + closed > 0 {
                lines.push(format!("{merged} merged, {closed} closed"));
            }
            lines.extend(
                self.prs
                    .iter()
                    .filter(|pr| {
                        !["open", "merged", "closed"]
                            .iter()
                            .any(|s| pr.state.eq_ignore_ascii_case(s))
                    })
                    .map(|pr| format!("#{} status unavailable", pr.number)),
            );
            lines.extend(self.prs_unread.iter().take(PR_REF_LIMIT).map(|pr| {
                // Same rule as a lane paging command: code only if it
                // survives rendering byte for byte.
                let how = if !pr.command.starts_with("provenant ") {
                    clean(&pr.command, NEXT_PAGE_LIMIT)
                } else if clean(&pr.command, NEXT_PAGE_LIMIT) == pr.command {
                    format!("run `{}`", pr.command)
                } else {
                    format!(
                        "the command is too long or unsafe to show; run provenant pr status {} with this hook's --state-dir",
                        pr.number
                    )
                };
                format!("#{} status unavailable; {how}", pr.number)
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
        let how = match (
            self.commands.runnable("lanes"),
            self.commands.runnable("pr status"),
        ) {
            (Some(lanes), Some(pr)) => format!("`{lanes}` and `{pr}`"),
            _ => "provenant lanes and provenant pr status with this hook's --state-dir".into(),
        };
        render(
            &format!("Provenant state for this project (read-only; re-derive with {how})"),
            &self.sections(false),
            limit,
            &self.commands,
        )
    }

    /// The autosave file's text, bounded by `limit` bytes.
    pub fn document(&self, auto_saved: &str, limit: usize) -> String {
        let head = format!(
            "## Provenant session state\n{AUTO_SAVED}{}\nDerived from lanes, pull requests and worktrees. It grants no authority and does not replace the chair's notes.",
            clean(auto_saved, 40)
        );
        let body = render(
            "",
            &self.sections(true),
            limit.saturating_sub(head.len() + 1),
            &self.commands,
        );
        format!("{head}\n{body}")
    }
}
fn plus(truncated: bool) -> &'static str {
    if truncated { "+" } else { "" }
}
fn render(title: &str, sections: &[Section], limit: usize, commands: &CommandPrefix) -> String {
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
        // The listing must read the store the digest came from; a command that
        // cannot be shown whole, or would not fit, becomes a plain note.
        let marker = commands
            .runnable("lanes")
            .map(|lanes| format!("- [output bounded; list the rest with `{lanes}`]\n"))
            .filter(|marker| marker.len() <= limit)
            .unwrap_or_else(|| {
                "- [output bounded; list the rest with provenant lanes and this hook's --state-dir]\n".to_owned()
            });
        while out.len() + marker.len() > limit {
            match out[..out.len().saturating_sub(1)].rfind('\n') {
                Some(end) => out.truncate(end + 1),
                None => {
                    out.clear();
                    break;
                }
            }
        }
        out.push_str(&marker);
    }
    out
}

/// The chair's text, if it has any beyond whitespace.
pub fn has_chair_text(chair: &str) -> bool {
    !chair.trim().is_empty()
}

/// What a hook needs from the saved checkpoint before collecting state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Baseline {
    /// Unix seconds of the chair's last save: the checkpoint's own
    /// modification time, which no autosave ever changes.
    pub chair_saved_unix: i64,
    /// PR numbers the chair named in their text.
    pub pr_refs: Vec<u64>,
}

/// Derive the baseline from a checkpoint read.
pub fn baseline(checkpoint: &super::Checkpoint) -> Baseline {
    Baseline {
        chair_saved_unix: checkpoint.modified_at.as_micros() / 1_000_000,
        pr_refs: pr_refs(&checkpoint.markdown),
    }
}

/// Pull request numbers the chair wrote, such as `PR #12`, `pr/12` or
/// `pull/12`, in first-mention order. The most recent `PR_REF_LIMIT` survive.
pub fn pr_refs(chair: &str) -> Vec<u64> {
    let lower = chair.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut found: Vec<u64> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !lower.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let rest = &lower[i..];
        let skip = ["pull request", "pull/", "pr/", "pr"]
            .iter()
            .find(|word| rest.starts_with(**word))
            .map(|word| word.len());
        let boundary = lower[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
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
    fn settled_successes_and_merged_prs_are_compact() {
        let state = SessionState {
            settled: (0..20)
                .map(|n| LaneLine {
                    state: "succeeded".into(),
                    ..lane(&format!("success-{n}"))
                })
                .chain(std::iter::once(LaneLine {
                    state: "failed".into(),
                    ..lane("failure")
                }))
                .collect(),
            prs: (1..=10)
                .map(|number| PrLine {
                    number,
                    state: "MERGED".into(),
                    summary: "irrelevant".into(),
                })
                .collect(),
            ..Default::default()
        };
        let digest = state.digest(DIGEST_LIMIT);
        assert!(digest.len() < 2048, "{}", digest.len());
        assert!(digest.contains("10 merged, 0 closed"));
        assert!(digest.contains("succeeded: 20"));
        assert!(digest.contains("failure failed"));
        assert!(!digest.contains("success-"));
        assert!(!digest.contains("#1 "));
    }

    #[test]
    fn hostile_fields_cannot_forge_markers_or_headings() {
        let state = SessionState {
            active: vec![LaneLine {
                id: "x\n### forged".into(),
                route: "a|b`c".into(),
                ..lane("x")
            }],
            ..Default::default()
        };
        let block = state.document("now", BLOCK_LIMIT);
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
        let block = state.document("now", 1024);
        assert!(block.len() <= 1024, "{}", block.len());
        assert!(block.contains("bounded"));
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
    fn the_autosave_document_is_bounded_and_carries_no_chair_text() {
        let state = SessionState {
            active: vec![lane("r/t/a")],
            ..Default::default()
        };
        let text = state.document("t1", BLOCK_LIMIT);
        assert!(text.starts_with("## Provenant session state\nAuto-saved: t1\n"));
        assert!(text.contains("r/t/a") && text.len() <= BLOCK_LIMIT);
        assert!(state.document("t", 600).len() <= 600);
    }

    #[test]
    fn a_next_page_command_shows_only_if_it_survives_rendering_byte_for_byte() {
        let render = |command: &str| {
            let state = SessionState {
                settled: vec![lane("r/t/a")],
                settled_more: Some(command.to_owned()),
                ..Default::default()
            };
            state.digest(DIGEST_LIMIT)
        };
        let good = "provenant --state-dir '/s/a b' lanes --limit 100 --cursor 7";
        assert!(render(good).contains(&format!("run `{good}`")));
        for bad in [
            "provenant --state-dir '/s/<x>' lanes --limit 100 --cursor 7".to_owned(),
            "provenant --state-dir '/s/a>b' lanes --limit 100 --cursor 7".to_owned(),
            "provenant --state-dir '/s/a`b' lanes --limit 100 --cursor 7".to_owned(),
            format!(
                "provenant --state-dir '/{}' lanes --limit 100 --cursor 7",
                "d".repeat(600)
            ),
        ] {
            let text = render(&bad);
            assert!(!text.contains("run `provenant"), "{text}");
            assert!(!text.contains("state-dir '/"), "{text}");
            assert!(text.contains("--cursor 7"), "{text}");
            assert!(text.contains("too long or unsafe"), "{text}");
        }
    }

    #[test]
    fn the_bounded_fallback_lists_the_store_the_digest_came_from() {
        let many: Vec<_> = (0..200)
            .map(|n| lane(&format!("run/task/{n:0>100}")))
            .collect();
        let cwd = std::path::Path::new("/work");
        let with = |dir: Option<&str>| SessionState {
            active: many.clone(),
            commands: CommandPrefix::new(dir.map(std::path::Path::new), cwd),
            ..Default::default()
        };
        let marker = |text: &str| text.lines().last().unwrap_or_default().to_owned();
        let long = format!("/{}", "d".repeat(600));
        for limit in [512, 700] {
            let text = with(Some("/s/a b")).digest(limit);
            assert!(text.len() <= limit);
            assert_eq!(
                marker(&text),
                "- [output bounded; list the rest with `provenant --state-dir '/s/a b' lanes`]"
            );
            let text = with(Some("rel")).document("t", limit);
            assert!(text.len() <= limit);
            assert!(
                marker(&text).contains("`provenant --state-dir '/work/rel' lanes`"),
                "{text}"
            );
            // A directory that cannot be shown whole gives a note, not a command.
            for bad in ["/s/a>b", "/s/a\nb", long.as_str()] {
                let text = with(Some(bad)).digest(limit);
                assert!(text.len() <= limit, "{}", text.len());
                assert_eq!(
                    marker(&text),
                    "- [output bounded; list the rest with provenant lanes and this hook's --state-dir]"
                );
            }
            // The default store keeps the plain command.
            assert_eq!(
                marker(&with(None).digest(limit)),
                "- [output bounded; list the rest with `provenant lanes`]"
            );
        }
    }

    #[test]
    fn the_digest_title_names_the_store_it_was_read_from() {
        let cwd = std::path::Path::new("/work");
        let with = |dir: &str| SessionState {
            commands: CommandPrefix::new(Some(std::path::Path::new(dir)), cwd),
            ..Default::default()
        };
        let title = with("/s/a b").digest(DIGEST_LIMIT);
        assert!(
            title.contains(
                "`provenant --state-dir '/s/a b' lanes` and `provenant --state-dir '/s/a b' pr status`"
            ),
            "{title}"
        );
        let title = with("/s/a>b").digest(DIGEST_LIMIT);
        assert!(!title.contains("`provenant"), "{title}");
        assert!(title.contains("this hook's --state-dir"), "{title}");
        let title = SessionState::default().digest(DIGEST_LIMIT);
        assert!(title.contains("`provenant lanes` and `provenant pr status`"));
    }

    #[test]
    fn pr_numbers_come_from_explicit_references_only() {
        let text = "Delivered PR #12 and pr/34; see Pull Request 56, pull/78.\n\
            Issue #99 is separate. Sprint 7 and prune 3 are not PRs. PR #12 again.";
        assert_eq!(pr_refs(text), vec![12, 34, 56, 78]);
        let many = (1..=12).map(|n| format!("PR #{n} ")).collect::<String>();
        assert_eq!(pr_refs(&many), (1..=12).collect::<Vec<_>>());
    }

    #[test]
    fn pr_references_survive_non_ascii_text() {
        assert_eq!(pr_refs("é PR #12 — ünïcode pull/34 終"), vec![12, 34]);
        assert!(pr_refs("cafépr12 überpull/34").is_empty());
    }
}
