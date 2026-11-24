//! What a writer dispatch does when its worktree still holds uncommitted
//! edits (`lanes.writer_dirty`). Advisory except for a worktree another lane
//! holds, and credential and Git internal roots: the effective value and the
//! layer that set it are named in every refusal and warning.
//!
//! The default is `wip_commit`: the preflight preserves the edits of an unheld
//! worktree in a local WIP commit before any plan node launches and warns with
//! `WORKTREE_ADOPTED_DIRTY`. `refuse` is an immediate, token-free refusal at
//! graph submission. `continue_in_place` lets the writer keep the edits and
//! warns. A worktree another lane holds is always refused (boundary 2), as are
//! credential and Git internal roots.

use provenant_core::AttemptId;

/// The registry knob.
pub const KNOB: &str = "lanes.writer_dirty";

/// The three operator choices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WriterDirty {
    /// Refuse at graph submission, before a plan node spends tokens.
    Refuse,
    /// Commit the retained edits locally before any plan node launches. This
    /// is the default for a worktree no live writer holds.
    #[default]
    WipCommit,
    /// Keep the edits in place and warn.
    ContinueInPlace,
}

impl WriterDirty {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Refuse => "refuse",
            Self::WipCommit => "wip_commit",
            Self::ContinueInPlace => "continue_in_place",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "refuse" => Some(Self::Refuse),
            "wip_commit" | "wip-commit" => Some(Self::WipCommit),
            "continue_in_place" | "continue-in-place" | "continue" => Some(Self::ContinueInPlace),
            _ => None,
        }
    }
}

/// The effective choice and the layer that set it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedWriterDirty {
    pub value: WriterDirty,
    pub source: String,
}

impl ResolvedWriterDirty {
    /// Resolve from a knob value and its already-rendered source (`default`,
    /// `host`, `launch`, ...). A missing or unknown value is the registry
    /// default, `wip_commit`.
    pub fn resolve(value: Option<&str>, source: &str) -> Self {
        Self {
            value: value.and_then(WriterDirty::parse).unwrap_or_default(),
            source: source.to_owned(),
        }
    }

    /// The setting suffix shared by every refusal and warning.
    pub fn setting(&self) -> String {
        format!(
            "{} = {} (source: {})",
            KNOB,
            self.value.as_str(),
            self.source
        )
    }

    /// Refusal text naming the dirty paths and the setting that could change
    /// the decision. Used only for the hard `refuse` case.
    pub fn refusal(&self, paths: &[String]) -> String {
        format!(
            "writer worktree has uncommitted edits: {}; the setting is {}; set it to wip_commit or continue_in_place to allow a continued draft",
            paths.join(", "),
            self.setting()
        )
    }

    /// Warning text for `continue_in_place`.
    pub fn warning(&self, paths: &[String]) -> String {
        format!(
            "warning: writer worktree retains uncommitted edits: {}; set {} to refuse or wip_commit to change this",
            paths.join(", "),
            self.setting()
        )
    }
}

/// The outcome of the token-free dirty-worktree preflight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Preflight {
    Clean,
    /// Hard refusal: either the operator chose `refuse`, or a credential or
    /// Git internal root is among the dirty paths.
    Refuse(String),
    /// Commit the edits as WIP before any plan node launches.
    WipCommit,
    /// Continue with the edits in place and surface the warning.
    ContinueInPlace(String),
}

/// Decide the preflight outcome for the observed dirty paths.
pub fn preflight(resolved: &ResolvedWriterDirty, paths: &[String]) -> Preflight {
    if paths.is_empty() {
        return Preflight::Clean;
    }
    let protected: Vec<String> = paths
        .iter()
        .filter(|path| provenant_confinement::worktrees::writer_edit_is_protected(path))
        .cloned()
        .collect();
    if !protected.is_empty() {
        return Preflight::Refuse(format!(
            "writer worktree has uncommitted edits in protected roots: {}; commit or relocate them before retrying",
            protected.join(", ")
        ));
    }
    match resolved.value {
        WriterDirty::Refuse => Preflight::Refuse(resolved.refusal(paths)),
        WriterDirty::WipCommit => Preflight::WipCommit,
        WriterDirty::ContinueInPlace => Preflight::ContinueInPlace(resolved.warning(paths)),
    }
}

/// The exact remedy for a worktree another lane holds or leaves unsettled.
/// One projection shared by the submission preflight and the run diagnostics:
/// it names the holder and the command that settles it, and never advises
/// committing or preserving another attempt's edits.
/// Spec: docs/specs/run-launch.md#writer-binding
pub fn held_writer_next(holder: Option<AttemptId>) -> String {
    match holder {
        Some(holder) => format!(
            "reconcile the holding attempt with `provenant reconcile {holder}` before retrying writer binding"
        ),
        None => "wait for the current writer or lock holder to finish, then retry".to_owned(),
    }
}

/// A worktree another lane holds is never adopted: committing or moving that
/// lane's uncommitted work would cross boundary 2. This refusal is
/// unconditional and does not depend on `lanes.writer_dirty`. The detail
/// carries the exact remedy too, because a refused node has no separate next
/// step of its own.
pub fn held_refusal(lanes: &[String], path: &str, holder: Option<AttemptId>) -> String {
    format!(
        "worktree {path} is in use by {}; refusing to adopt another lane's uncommitted work; {}",
        lanes.join(", "),
        held_writer_next(holder)
    )
}

/// An operator-facing submission refusal: the detail recorded on the node and
/// the next step recorded on the refusal notice. A held checkout settles its
/// holder; an unheld dirty tree is the operator's own to preserve or commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriterRefusal {
    pub reason: String,
    pub next: String,
}

impl WriterRefusal {
    /// A worktree another lane holds or leaves unsettled.
    pub fn held(lanes: &[String], path: &str, holder: Option<AttemptId>) -> Self {
        Self {
            reason: held_refusal(lanes, path, holder),
            next: held_writer_next(holder),
        }
    }

    /// An unheld dirty tree refused by `lanes.writer_dirty`.
    pub fn dirty(reason: String) -> Self {
        Self {
            reason,
            next: "commit or preserve the worktree changes before retrying".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Spec: docs/specs/run.md#writer-pin-and-re-pin
    #[test]
    fn an_unheld_dirty_worktree_is_wip_committed_by_default() {
        let resolved = ResolvedWriterDirty::resolve(None, "default");
        assert_eq!(resolved.value, WriterDirty::WipCommit);
        let paths = vec!["src/main.rs".to_owned(), "docs/run.md".to_owned()];
        assert_eq!(preflight(&resolved, &paths), Preflight::WipCommit);
        // The setting stays visible with its source even on the default path.
        assert!(resolved.setting().contains(KNOB));
        assert!(resolved.setting().contains("source: default"));
    }

    // Spec: docs/specs/run.md#writer-pin-and-re-pin
    #[test]
    fn an_explicit_refuse_setting_still_refuses_and_names_paths_and_setting() {
        let resolved = ResolvedWriterDirty::resolve(Some("refuse"), "host");
        assert_eq!(resolved.value, WriterDirty::Refuse);
        let paths = vec!["src/main.rs".to_owned(), "docs/run.md".to_owned()];
        match preflight(&resolved, &paths) {
            Preflight::Refuse(reason) => {
                assert!(reason.contains("src/main.rs"), "{reason}");
                assert!(reason.contains("docs/run.md"), "{reason}");
                assert!(reason.contains(KNOB), "{reason}");
                assert!(reason.contains("source: host"), "{reason}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    // Spec: docs/specs/run.md#writer-pin-and-re-pin
    #[test]
    fn a_held_worktree_is_never_adopted() {
        let holder = AttemptId::from_bytes([53; 16]);
        let lanes = vec![format!("w3/other (attempt {holder})")];
        let refusal = WriterRefusal::held(&lanes, ".worktrees/writer", Some(holder));
        assert!(refusal.reason.contains(".worktrees/writer"), "{refusal:?}");
        assert!(refusal.reason.contains("w3/other"), "{refusal:?}");
        assert!(refusal.reason.contains("another lane"), "{refusal:?}");
        // The holder and the exact command reach both the node detail and the
        // refusal notice; committing or preserving the holder's edits is never
        // offered. Spec: docs/specs/run-launch.md#writer-binding
        assert!(refusal.reason.contains(&holder.to_string()), "{refusal:?}");
        assert!(refusal.next.contains(&holder.to_string()), "{refusal:?}");
        assert!(refusal.next.contains("provenant reconcile"), "{refusal:?}");
        assert!(
            !refusal.reason.contains("commit or preserve"),
            "{refusal:?}"
        );
        assert!(!refusal.next.contains("commit or preserve"), "{refusal:?}");
    }

    // Spec: docs/specs/run-launch.md#writer-binding
    #[test]
    fn a_live_holder_without_an_attempt_gets_a_wait_step() {
        let next = held_writer_next(None);
        assert!(next.contains("wait for"), "{next}");
        assert!(!next.contains("commit"), "{next}");
        assert!(!next.contains("preserve"), "{next}");
    }

    // Spec: docs/specs/run.md#writer-pin-and-re-pin
    #[test]
    fn every_setting_is_overridable_with_its_source() {
        for (value, source, expected) in [
            (None, "default", WriterDirty::WipCommit),
            (Some("refuse"), "host", WriterDirty::Refuse),
            (Some("wip_commit"), "launch", WriterDirty::WipCommit),
            (
                Some("continue_in_place"),
                "project",
                WriterDirty::ContinueInPlace,
            ),
        ] {
            let resolved = ResolvedWriterDirty::resolve(value, source);
            assert_eq!(resolved.value, expected);
            assert!(resolved.setting().contains(source));
            assert!(resolved.setting().contains(KNOB));
            assert!(resolved.setting().contains("source:"));
        }
    }

    // Spec: docs/specs/run.md#writer-pin-and-re-pin
    #[test]
    fn wip_commit_and_continue_only_apply_to_ordinary_paths() {
        let ordinary = vec!["src/main.rs".to_owned()];
        for value in ["wip_commit", "continue_in_place"] {
            let resolved = ResolvedWriterDirty::resolve(Some(value), "host");
            assert_ne!(preflight(&resolved, &ordinary), Preflight::Clean);
        }
        for protected in [
            ".git/config".to_owned(),
            ".ssh/id_rsa".to_owned(),
            "credentials.json".to_owned(),
        ] {
            let resolved = ResolvedWriterDirty::resolve(Some("wip_commit"), "host");
            assert!(
                matches!(
                    preflight(&resolved, std::slice::from_ref(&protected)),
                    Preflight::Refuse(_)
                ),
                "{protected} must stay a hard refusal"
            );
            let resolved = ResolvedWriterDirty::resolve(Some("continue_in_place"), "host");
            assert!(
                matches!(
                    preflight(&resolved, std::slice::from_ref(&protected)),
                    Preflight::Refuse(_)
                ),
                "{protected} must stay a hard refusal"
            );
        }
    }

    // Spec: docs/specs/run.md#writer-pin-and-re-pin
    #[test]
    fn a_clean_worktree_is_always_allowed() {
        for value in ["refuse", "wip_commit", "continue_in_place"] {
            let resolved = ResolvedWriterDirty::resolve(Some(value), "default");
            assert_eq!(preflight(&resolved, &[]), Preflight::Clean);
        }
    }
}
