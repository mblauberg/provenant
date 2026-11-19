//! What a writer dispatch does when its worktree still holds uncommitted
//! edits (`lanes.writer_dirty`). Advisory except for credential and Git
//! internal roots: the effective value and the layer that set it are named in
//! every refusal and warning.
//!
//! The default is `refuse`: an immediate, token-free refusal at graph
//! submission. `wip_commit` preserves the edits in a local WIP commit before
//! any plan node launches. `continue_in_place` lets the writer keep the edits
//! and warns. Credential and Git internal roots are always a hard refusal.

/// The registry knob.
pub const KNOB: &str = "lanes.writer_dirty";

/// The three operator choices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WriterDirty {
    /// Refuse at graph submission, before a plan node spends tokens.
    #[default]
    Refuse,
    /// Commit the retained edits locally before any plan node launches.
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
    /// default, `refuse`.
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
            "writer worktree has uncommitted edits: {}; set {} to wip_commit or continue_in_place to allow a continued draft",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dirty_worktree_is_refused_by_default_and_names_paths_and_setting() {
        let resolved = ResolvedWriterDirty::resolve(None, "default");
        assert_eq!(resolved.value, WriterDirty::Refuse);
        let paths = vec!["src/main.rs".to_owned(), "docs/run.md".to_owned()];
        match preflight(&resolved, &paths) {
            Preflight::Refuse(reason) => {
                assert!(reason.contains("src/main.rs"), "{reason}");
                assert!(reason.contains("docs/run.md"), "{reason}");
                assert!(reason.contains(KNOB), "{reason}");
                assert!(reason.contains("source: default"), "{reason}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn every_setting_is_overridable_with_its_source() {
        for (value, source, expected) in [
            ("refuse", "host", WriterDirty::Refuse),
            ("wip_commit", "launch", WriterDirty::WipCommit),
            ("continue_in_place", "project", WriterDirty::ContinueInPlace),
        ] {
            let resolved = ResolvedWriterDirty::resolve(Some(value), source);
            assert_eq!(resolved.value, expected);
            assert!(resolved.setting().contains(source));
            assert!(resolved.setting().contains(KNOB));
        }
    }

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

    #[test]
    fn a_clean_worktree_is_always_allowed() {
        for value in ["refuse", "wip_commit", "continue_in_place"] {
            let resolved = ResolvedWriterDirty::resolve(Some(value), "default");
            assert_eq!(preflight(&resolved, &[]), Preflight::Clean);
        }
    }
}
