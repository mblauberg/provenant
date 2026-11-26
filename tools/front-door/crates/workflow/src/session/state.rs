//! Lane, custody and worktree state that Claude hooks report and save
//! beside a session checkpoint. Rendering is pure: callers collect the facts.
//! The saved state is derived data in a file only hooks write. It grants no
//! authority, and no autosave ever opens the chair's checkpoint for writing.

use provenant_os::shell::{quote, quote_if_needed};
use std::path::{Path, PathBuf};

const AUTO_SAVED: &str = "Auto-saved: ";
/// Most lines one section shows before it summarises the rest.
const SECTION_LINES: usize = 8;
/// Longest single field taken from a lane, custody or worktree record.
const FIELD_LIMIT: usize = 160;
pub const DIGEST_LIMIT: usize = 3 * 1024;
pub const BLOCK_LIMIT: usize = 6 * 1024;
/// Longest next-page command shown as a runnable command.
const NEXT_PAGE_LIMIT: usize = 512;

/// Starts a note that is shown as plain text instead of as a command.
const PLAIN_NOTE: &str = "page with: ";

/// What the caller's environment resolves when no `--state-dir` or `--project`
/// flag is passed. A formatter drops a flag that names one of these defaults:
/// a copied command then reaches the same store and project from any directory.
/// Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandDefaults {
    /// Absolute state root the environment resolves with no flag.
    pub state_dir: Option<PathBuf>,
    /// Absolute project the environment resolves with no flag.
    pub project: Option<PathBuf>,
}

/// A relative path is made absolute against the directory the caller ran in.
fn absolute(path: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    }
}

/// `provenant` plus the flags a hook must repeat so every command it shows
/// reads or writes the store the hook used. Flags that equal the environment's
/// defaults are omitted. This is the one formatter for suggested commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPrefix {
    text: String,
    /// False when a directory cannot be quoted faithfully (not UTF-8, or
    /// holds control characters): no runnable command may then be shown.
    shown: bool,
    /// The project this command is for, quoted, for `--project`.
    checkout: Option<String>,
    /// Whether `--project` belongs in the commands this prefix renders.
    project: bool,
    /// The defaults used to drop redundant flags from an argv.
    defaults: CommandDefaults,
}
impl Default for CommandPrefix {
    fn default() -> Self {
        Self {
            text: "provenant".into(),
            shown: true,
            checkout: None,
            project: false,
            defaults: CommandDefaults::default(),
        }
    }
}
impl CommandPrefix {
    /// A relative directory is made absolute against `cwd` (where the hook
    /// ran) and quoted for a POSIX shell. The project flag names `cwd`.
    pub fn new(state_dir: Option<&Path>, cwd: &Path) -> Self {
        let mut prefix = Self {
            checkout: cwd
                .to_str()
                .filter(|s| !s.chars().any(char::is_control))
                .map(quote),
            project: true,
            ..Self::default()
        };
        if let Some(state) = state_dir {
            match absolute(state, cwd)
                .to_str()
                .filter(|s| !s.chars().any(char::is_control))
            {
                Some(text) => {
                    prefix.text.push_str(" --state-dir ");
                    prefix.text.push_str(&quote(text));
                }
                None => prefix.shown = false,
            }
        }
        prefix
    }

    /// Build a prefix for a command that will run in the caller's environment.
    /// `state_dir` and `project` are the flags this command actually used; a
    /// flag equal to the corresponding default is omitted from every command
    /// this prefix renders. Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    pub fn with_defaults(
        state_dir: Option<&Path>,
        project: Option<&Path>,
        cwd: &Path,
        defaults: &CommandDefaults,
    ) -> Self {
        let mut prefix = Self {
            defaults: defaults.clone(),
            ..Self::default()
        };
        if let Some(state) = state_dir {
            let state = absolute(state, cwd);
            let is_default = defaults
                .state_dir
                .as_deref()
                .is_some_and(|default| absolute(default, cwd) == state);
            if !is_default {
                match state.to_str().filter(|s| !s.chars().any(char::is_control)) {
                    Some(text) => {
                        prefix.text.push_str(" --state-dir ");
                        prefix.text.push_str(&quote(text));
                    }
                    None => prefix.shown = false,
                }
            }
        }
        if let Some(dir) = project {
            let dir = absolute(dir, cwd);
            let is_default = defaults
                .project
                .as_deref()
                .is_some_and(|default| absolute(default, cwd) == dir);
            if !is_default {
                prefix.checkout = dir
                    .to_str()
                    .filter(|s| !s.chars().any(char::is_control))
                    .map(quote);
                prefix.project = prefix.checkout.is_some();
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

    /// ` --project DIR` naming the selected checkout, so a save reaches this
    /// project from any directory the agent is in. Empty when the flag equals
    /// the environment default or the path would not survive rendering.
    pub fn project_flag(&self) -> String {
        let Some(checkout) = self.checkout.as_ref().filter(|_| self.project) else {
            return String::new();
        };
        let flag = format!(" --project {checkout}");
        if clean(&flag, NEXT_PAGE_LIMIT) == flag {
            flag
        } else {
            String::new()
        }
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

    /// Render one suggested command from its argv. Flags that name an
    /// environment default are dropped, then every argument is shell-quoted.
    /// `None` when the command cannot be shown safely or would be truncated.
    pub fn render_argv(&self, argv: &[String]) -> Option<String> {
        let mut parts = Vec::new();
        let mut index = 0;
        while index < argv.len() {
            let arg = &argv[index];
            let flag = match arg.as_str() {
                "--state-dir" => Some(&self.defaults.state_dir),
                "--project" => Some(&self.defaults.project),
                _ => None,
            };
            if let Some(default) = flag
                && index + 1 < argv.len()
                && default
                    .as_deref()
                    .is_some_and(|path| Path::new(&argv[index + 1]) == path)
            {
                index += 2;
                continue;
            }
            parts.push(quote_if_needed(arg));
            index += 1;
        }
        let command = parts.join(" ");
        (self.shown && clean(&command, NEXT_PAGE_LIMIT) == command).then_some(command)
    }
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
pub struct WorktreeLine {
    pub path: String,
    pub branch: String,
    pub dirty: String,
    pub writer: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unavailable {
    /// The lane, worktree or other hook source that could not be read.
    pub source: String,
    /// A bounded explanation of why the source could not be read.
    pub reason: String,
}

/// Where the session digest looks for custody rows. `project` (the default)
/// keeps another project's work out of the chair's context; `all` opts back in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DigestScope {
    #[default]
    Project,
    All,
}
impl DigestScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::All => "all",
        }
    }
}

/// One custody row as the collector saw it. Presence and project travel with
/// the subject so the digest can collapse unattended and foreign work without
/// losing the counts or showing a row it should hide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyLine {
    pub subject: String,
    pub presence: String,
    pub project: String,
    pub mine: bool,
}

/// One collected view. A section the collector could not read is named in
/// `unavailable` rather than shown as empty, so absence never reads as "none".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionState {
    /// Active custody rows the collector could see, before scoping.
    pub custody: Vec<CustodyLine>,
    pub custody_more: Option<String>,
    /// Project the hook ran in; another project's rows stay hidden by default.
    pub current_project: Option<String>,
    /// Where the digest looks for custody; from `session.digest.scope`.
    pub scope: DigestScope,
    /// Config layer that set `scope`, shown in the unattended line.
    pub scope_source: Option<String>,
    pub active: Vec<LaneLine>,
    pub settled: Vec<LaneLine>,
    /// When the listing held lanes beyond the page the collector read, the
    /// command that lists the next page, or a plain note on how to when no safe
    /// command exists. The count shown is then partial.
    pub active_more: Option<String>,
    pub settled_more: Option<String>,
    /// Records in the selected page not inspected before the deadline.
    pub active_skipped: usize,
    pub settled_skipped: usize,
    pub worktrees: Vec<WorktreeLine>,
    /// Linked worktrees the inventory could not inspect before its deadline.
    pub worktrees_skipped: usize,
    pub unavailable: Vec<Unavailable>,
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
        let here = |project: &str| {
            self.current_project
                .as_deref()
                .is_some_and(|current| current == project)
        };
        let mut custody_lines = Vec::new();
        let mut unattended = 0usize;
        let mut unattended_here = 0usize;
        for row in &self.custody {
            if row.presence == "unattended" {
                unattended += 1;
                if here(&row.project) {
                    unattended_here += 1;
                }
            } else if row.mine && (self.scope == DigestScope::All || here(&row.project)) {
                // Only custody this chair holds is named; unattended and
                // foreign-project rows collapse into one counted line below.
                custody_lines.push(format!(
                    "{} ({})",
                    clean(&row.subject, FIELD_LIMIT),
                    clean(&row.presence, 32)
                ));
            }
        }
        if unattended > 0 {
            let command = self
                .commands
                .runnable("status --adoptable")
                .unwrap_or_else(|| {
                    "provenant status --adoptable with this hook's --state-dir".into()
                });
            custody_lines.push(format!(
                "Unattended: {unattended} ({unattended_here} this project) — {command} (scope: {}, source: {})",
                self.scope.as_str(),
                self.scope_source.as_deref().unwrap_or("default")
            ));
        }
        if !custody_lines.is_empty() || self.custody_more.is_some() {
            sections.push(Section {
                title: "Active custody".into(),
                lines: custody_lines,
                more: 0,
                partial: self.custody_more.clone(),
            });
        }
        let lanes = |title: &str, lanes: &[LaneLine], next: &Option<String>, skipped: usize| {
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
                        "the paging command is too long or unsafe to show; run provenant status with this hook's --state-dir and --cursor {cursor}"
                    )
                };
                if skipped > 0 {
                    format!("{skipped} lane record{} not inspected in this page; {how}", if skipped == 1 { " was" } else { "s were" })
                } else {
                    format!("more lanes exist beyond the page read, so this count is partial; {how}")
                }
            }),
        }
        };
        // A lane section with no lanes and no partial note is omitted: an empty
        // count says nothing the agent can act on.
        if !self.active.is_empty() || self.active_more.is_some() {
            sections.push(lanes(
                "Lanes in flight",
                &self.active,
                &self.active_more,
                self.active_skipped,
            ));
        }
        let mut settled = lanes(
            "Lanes settled since the last chair save",
            &self.settled,
            &self.settled_more,
            self.settled_skipped,
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
        if !self.settled.is_empty() || self.settled_more.is_some() {
            sections.push(settled);
        }
        if worktrees
            && !self
                .unavailable
                .iter()
                .any(|unavailable| unavailable.source == "worktrees")
        {
            sections.push(Section {
                title: format!(
                    "Worktrees ({}{})",
                    self.worktrees.len(),
                    plus(self.worktrees_skipped > 0)
                ),
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
                partial: (self.worktrees_skipped > 0).then(|| {
                    format!(
                        "{} worktree{} skipped during inventory",
                        self.worktrees_skipped,
                        if self.worktrees_skipped == 1 { "" } else { "s" }
                    )
                }),
            });
        }
        for name in &self.unavailable {
            sections.push(Section {
                title: format!(
                    "{} unavailable: {}",
                    clean(&name.source, 32),
                    clean(&name.reason, 120)
                ),
                lines: vec![],
                more: 0,
                partial: None,
            });
        }
        sections
    }

    /// Bounded text for a SessionStart hook. Read-only; nothing is saved.
    pub fn digest(&self, limit: usize) -> String {
        let sections = self.sections(!self.worktrees.is_empty() || self.worktrees_skipped > 0);
        if sections.is_empty() {
            return String::new();
        }
        let mut how = match (
            self.commands.runnable("status"),
            self.commands.runnable("pr status"),
        ) {
            (Some(lanes), Some(pr)) => format!("`{lanes}` and `{pr}`"),
            _ => "provenant status and provenant pr status with this hook's --state-dir".into(),
        };
        if !self.worktrees.is_empty() || self.worktrees_skipped > 0 {
            match self.commands.runnable("worktree list") {
                Some(command) => how.push_str(&format!(", and `{command}`")),
                None => how.push_str(", and provenant worktree list with this hook's --state-dir"),
            }
        }
        render(
            &format!("Provenant state for this project (read-only; re-derive with {how})"),
            &sections,
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
            .runnable("status")
            .map(|lanes| format!("- [output bounded; list the rest with `{lanes}`]\n"))
            .filter(|marker| marker.len() <= limit)
            .unwrap_or_else(|| {
                "- [output bounded; list the rest with provenant status and this hook's --state-dir]\n".to_owned()
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
}

/// Derive the baseline from a checkpoint read.
pub fn baseline(checkpoint: &super::Checkpoint) -> Baseline {
    Baseline {
        chair_saved_unix: checkpoint.modified_at.as_micros() / 1_000_000,
    }
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

    fn custody(subject: &str, presence: &str, project: &str, mine: bool) -> CustodyLine {
        CustodyLine {
            subject: subject.into(),
            presence: presence.into(),
            project: project.into(),
            mine,
        }
    }

    #[test]
    fn unattended_custody_collapses_within_the_digest_limit() {
        // A store full of unattended rows must not fill the chair's context:
        // they collapse to one counted line, whatever their number.
        let rows: Vec<_> = (0..90)
            .map(|n| {
                custody(
                    &format!("foreign/unattended-{n}"),
                    "unattended",
                    "other",
                    false,
                )
            })
            .collect();
        let state = SessionState {
            custody: rows,
            current_project: Some("here".into()),
            ..Default::default()
        };
        let digest = state.digest(DIGEST_LIMIT);
        assert!(digest.len() <= DIGEST_LIMIT, "{}", digest.len());
        assert!(digest.len() < 1024, "{}", digest.len());
        assert!(
            digest.contains("Unattended: 90 (0 this project)"),
            "{digest}"
        );
        assert!(digest.contains("status --adoptable"), "{digest}");
        assert!(digest.contains("source: default"), "{digest}");
        assert!(!digest.contains("unattended-0"), "{digest}");
    }

    #[test]
    fn foreign_project_rows_are_hidden_unless_the_scope_is_all() {
        let rows = vec![
            custody("mine/here", "live", "here", true),
            custody("mine/there", "live", "there", true),
            custody("adoptable/here", "unattended", "here", false),
            custody("adoptable/there", "unattended", "there", false),
        ];
        let scoped = |scope| SessionState {
            custody: rows.clone(),
            current_project: Some("here".into()),
            scope,
            ..Default::default()
        };
        let project = scoped(DigestScope::Project).digest(DIGEST_LIMIT);
        assert!(project.contains("mine/here"), "{project}");
        assert!(!project.contains("mine/there"), "{project}");
        assert!(
            project.contains("Unattended: 2 (1 this project)"),
            "{project}"
        );
        let all = scoped(DigestScope::All).digest(DIGEST_LIMIT);
        assert!(
            all.contains("mine/here") && all.contains("mine/there"),
            "{all}"
        );
        assert!(all.contains("scope: all"), "{all}");
    }

    #[test]
    fn settled_successes_are_compacted_to_a_count() {
        let successes: Vec<_> = (0..SECTION_LINES + 1)
            .map(|n| LaneLine {
                state: "succeeded".into(),
                ..lane(&format!("success-{n}"))
            })
            .collect();
        let success_count = successes.len();
        let state = SessionState {
            settled: successes
                .into_iter()
                .chain(std::iter::once(LaneLine {
                    state: "failed".into(),
                    ..lane("failure")
                }))
                .collect(),
            ..Default::default()
        };
        let digest = state.digest(DIGEST_LIMIT);
        assert!(digest.len() <= DIGEST_LIMIT, "{}", digest.len());
        // The count is derived from the fixture and conveyed; the individual
        // successes are collapsed, while a non-success is still named.
        assert!(digest.contains(&success_count.to_string()), "{digest}");
        assert!(
            digest.contains("failure") && digest.contains("failed"),
            "{digest}"
        );
        assert!(!digest.contains("success-"), "{digest}");
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
    fn output_is_bounded_and_remains_actionable() {
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
        let block = state.document("now", 1024);
        assert!(block.len() <= 1024, "{}", block.len());
        // Truncation is disclosed, not silent, and the bounded form still
        // carries an actionable command to list the rest.
        assert!(digest.contains("bounded"), "{digest}");
        assert!(block.contains("bounded"), "{block}");
        assert!(state.digest(DIGEST_LIMIT).contains("more"));
        assert_ne!(digest, state.digest(DIGEST_LIMIT));
        let list = CommandPrefix::default().runnable("status").unwrap();
        assert!(digest.contains(&list), "{digest}");
        assert!(block.contains(&list), "{block}");
    }

    #[test]
    fn empty_lane_sections_are_omitted() {
        assert_eq!(SessionState::default().digest(DIGEST_LIMIT), "");
        let state = SessionState {
            settled: vec![lane("r/t/a")],
            ..Default::default()
        };
        let digest = state.digest(DIGEST_LIMIT);
        // The one populated lane is shown exactly once; the empty active
        // section adds no heading or placeholder line.
        assert!(!digest.contains("in flight"), "{digest}");
        assert!(digest.contains("r/t/a"), "{digest}");
        assert_eq!(digest.matches("r/t/a").count(), 1, "{digest}");
    }

    #[test]
    fn project_flag_names_the_checkout_only_when_it_renders() {
        let cwd = std::path::Path::new("/work/proj");
        let flag = CommandPrefix::new(None, cwd).project_flag();
        assert!(
            flag.contains(&format!("--project '{}'", cwd.display())),
            "{flag}"
        );
        assert!(CommandPrefix::default().project_flag().is_empty());
        let hostile = std::path::Path::new("/work/a<b>");
        assert!(CommandPrefix::new(None, hostile).project_flag().is_empty());
    }

    #[test]
    fn unreadable_sections_are_named_not_shown_empty() {
        let state = SessionState {
            unavailable: vec![Unavailable {
                source: "lanes".into(),
                reason: "timed out".into(),
            }],
            ..Default::default()
        };
        let digest = state.digest(DIGEST_LIMIT);
        // The source and reason come from the fixture and must both surface,
        // so an unreadable section is not mistaken for an empty one.
        assert!(
            digest.contains("lanes") && digest.contains("timed out"),
            "{digest}"
        );

        let partial = SessionState {
            active: vec![lane("r/t/a")],
            unavailable: vec![Unavailable {
                source: "settled lanes".into(),
                reason: "timed out".into(),
            }],
            ..Default::default()
        };
        let digest = partial.digest(DIGEST_LIMIT);
        assert!(digest.contains("r/t/a"), "{digest}");
        assert!(
            digest.contains("settled lanes") && digest.contains("timed out"),
            "{digest}"
        );
    }

    #[test]
    fn next_page_command_shows_only_if_it_survives_rendering() {
        let render = |command: &str| {
            let state = SessionState {
                settled: vec![lane("r/t/a")],
                settled_more: Some(command.to_owned()),
                ..Default::default()
            };
            state.digest(DIGEST_LIMIT)
        };
        let good = "provenant --state-dir '/s/a b' status --limit 100 --cursor 7";
        assert!(render(good).contains(good), "{}", render(good));
        let long = format!("/{}", "d".repeat(600));
        for dir in ["/s/<x>", "/s/a>b", "/s/a`b", long.as_str()] {
            let bad = format!("provenant --state-dir '{dir}' status --limit 100 --cursor 7");
            let text = render(&bad);
            // Neither the unsafe command nor any unsafe fragment of the path
            // is shown (a long path is checked by a leading run); its cursor
            // still reaches the output so the owner can page by hand.
            let fragment = &dir[..dir.len().min(32)];
            assert!(!text.contains(&bad), "{text}");
            assert!(!text.contains(fragment), "{text}");
            assert!(text.contains("--cursor 7"), "{text}");
        }
    }

    #[test]
    fn session_digest_includes_collected_worktrees() {
        let state = SessionState {
            worktrees: vec![WorktreeLine {
                path: "/work/tree".into(),
                branch: "topic".into(),
                dirty: "clean".into(),
                writer: "free".into(),
            }],
            ..Default::default()
        };
        let digest = state.digest(DIGEST_LIMIT);
        assert!(digest.contains("/work/tree"), "{digest}");
        assert!(digest.contains("topic"), "{digest}");
        assert!(digest.contains("clean"), "{digest}");
    }

    #[test]
    fn bounded_fallback_names_the_store_without_leaking_unsafe_paths() {
        let many: Vec<_> = (0..200)
            .map(|n| lane(&format!("run/task/{n:0>100}")))
            .collect();
        let cwd = std::path::Path::new("/work");
        let with = |dir: Option<&str>| SessionState {
            active: many.clone(),
            commands: CommandPrefix::new(dir.map(std::path::Path::new), cwd),
            ..Default::default()
        };
        let long = format!("/{}", "d".repeat(600));
        for limit in [512, 700] {
            let text = with(Some("/s/a b")).digest(limit);
            assert!(text.len() <= limit);
            assert!(text.contains("/s/a b"), "{text}");
            let text = with(Some("rel")).document("t", limit);
            assert!(text.len() <= limit);
            assert!(text.contains("/work/rel"), "{text}");
            for bad in ["/s/a>b", "/s/a\nb", long.as_str()] {
                let text = with(Some(bad)).digest(limit);
                assert!(text.len() <= limit, "{}", text.len());
                assert!(!text.contains("`provenant"), "{text}");
                assert!(text.contains("--state-dir"), "{text}");
                assert!(!text.contains(bad), "{text}");
            }
            // The default store keeps the plain command.
            let text = with(None).digest(limit);
            assert!(text.len() <= limit);
            let plain = CommandPrefix::default().runnable("status").unwrap();
            assert!(text.contains(&plain), "{text}");
        }
    }

    #[test]
    fn digest_names_the_store_it_was_read_from() {
        let cwd = std::path::Path::new("/work");
        let with = |dir: Option<&str>| SessionState {
            active: vec![lane("run/task/a")],
            commands: CommandPrefix::new(dir.map(std::path::Path::new), cwd),
            ..Default::default()
        };
        let text = with(Some("/s/a b")).digest(DIGEST_LIMIT);
        assert!(text.contains("/s/a b"), "{text}");
        let text = with(Some("/s/a>b")).digest(DIGEST_LIMIT);
        assert!(!text.contains("/s/a>b"), "{text}");
        assert!(text.contains("--state-dir"), "{text}");
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn default_flags_are_omitted_but_non_defaults_are_shown() {
        let cwd = Path::new("/work/proj");
        let defaults = CommandDefaults {
            state_dir: Some(PathBuf::from("/home/agent/.agents/state")),
            project: Some(PathBuf::from("/work/proj")),
        };
        let same = CommandPrefix::with_defaults(
            Some(Path::new("/home/agent/.agents/state")),
            Some(cwd),
            cwd,
            &defaults,
        );
        assert_eq!(same.runnable("status").as_deref(), Some("provenant status"));
        assert_eq!(same.project_flag(), "");

        let other = CommandPrefix::with_defaults(
            Some(Path::new("/other/state")),
            Some(Path::new("/other/proj")),
            cwd,
            &defaults,
        );
        assert_eq!(
            other.runnable("status").as_deref(),
            Some("provenant --state-dir '/other/state' status")
        );
        assert_eq!(other.project_flag(), " --project '/other/proj'");

        // The argv form drops only the flags that name a default.
        let same_argv: Vec<String> = [
            "provenant",
            "--state-dir",
            "/home/agent/.agents/state",
            "--project",
            "/work/proj",
            "status",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            same.render_argv(&same_argv).as_deref(),
            Some("provenant status")
        );
        let other_argv: Vec<String> = [
            "provenant",
            "--state-dir",
            "/other/state",
            "--project",
            "/other/proj",
            "status",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            other.render_argv(&other_argv).as_deref(),
            Some("provenant --state-dir /other/state --project /other/proj status")
        );
    }
}
