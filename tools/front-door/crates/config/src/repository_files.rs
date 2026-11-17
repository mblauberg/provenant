//! Every path Provenant reads or writes inside a consuming repository.
//!
//! All of them live under the repository's `.provenant/` directory and all
//! are optional: a repository with none of them gets compiled and host
//! defaults. Readers name these paths through the constants here, and the
//! outward contract renders the table, so a new repository file is one
//! constant and one row. Nothing else in a repository (scripts, build files,
//! instruction files, CI configuration or directory layout) is required.
//! Provider-owned ignore files that doctor and launch read only to warn
//! (such as `.cursorignore`) are declared on the adapter
//! (`AdapterSpec::ignore_files`), not here: they are not Provenant's files.
use provenant_core::consumer_env::Stability;
use serde::{Deserialize, Serialize};

/// The project's Provenant directory, relative to the repository root.
pub const DIRECTORY: &str = ".provenant";
pub const TEMPLATES: &str = ".provenant/templates";
pub const TRACKER_LINT: &str = ".provenant/tracker-lint.toml";
pub const SCRATCH: &str = ".provenant/scratch";

/// Sections of `.provenant/config.toml`. The launch-policy keys are the
/// fields of `RepositoryPolicy`; the landing section takes the host's
/// `[domains.pr.land]` keys (`provenant config schema`).
pub const LAUNCH_POLICY_SECTION: &str = "launch_policy";
pub const LAUNCH_POLICY_KEYS: &[&str] = &["confidential", "protected_paths"];
pub const LAND_SECTION: &str = "domains.pr.land";
/// Review floors a project declares for its own boundaries, in the shape of
/// the host's `[domains.routing.review.floors]` table.
pub const REVIEW_FLOORS_SECTION: &str = "domains.routing.review.floors";
/// Sections their owners decode, so the launch-policy reader does not flag them.
pub const DECODED_SECTIONS: &[&str] = &[LAND_SECTION, REVIEW_FLOORS_SECTION];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Entry {
    File,
    Directory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// Provenant reads it when present.
    Read,
    /// Provenant creates it, untracked and excluded from Git.
    Created,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepositoryFile {
    pub path: &'static str,
    pub entry: Entry,
    pub access: Access,
    pub stability: Stability,
    pub purpose: &'static str,
}

const fn file(
    path: &'static str,
    entry: Entry,
    access: Access,
    stability: Stability,
    purpose: &'static str,
) -> RepositoryFile {
    RepositoryFile {
        path,
        entry,
        access,
        stability,
        purpose,
    }
}

/// Every repository path Provenant uses. Order is presentation order only.
pub const REPOSITORY_FILES: &[RepositoryFile] = &[
    file(
        crate::REPOSITORY_CONFIG,
        Entry::File,
        Access::Read,
        Stability::Evolving,
        "Repository settings: `[launch_policy]`, `[domains.pr.land]` and `[domains.routing.review.floors]`.",
    ),
    file(
        crate::PORTS_CONFIG,
        Entry::File,
        Access::Read,
        Stability::Evolving,
        "Repository port table: verify, land, CI observation, worktree layout, naming, post-create, tracker, release and admission backends.",
    ),
    file(
        TEMPLATES,
        Entry::Directory,
        Access::Read,
        Stability::Evolving,
        "Repository graph templates, layered over the bundled and user templates.",
    ),
    file(
        TRACKER_LINT,
        Entry::File,
        Access::Read,
        Stability::Evolving,
        "Severity of the tracker lint rules.",
    ),
    file(
        SCRATCH,
        Entry::Directory,
        Access::Created,
        Stability::Internal,
        "Operator scratch for briefs and graph documents; Provenant excludes it through `.git/info/exclude`.",
    ),
];

/// The JSON pointer of a dotted config section, such as `/domains/pr/land`.
pub fn section_pointer(section: &str) -> String {
    section.split('.').fold(String::new(), |mut pointer, part| {
        pointer.push('/');
        pointer.push_str(part);
        pointer
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn repository_paths_are_unique_and_inside_the_provenant_directory() {
        let mut seen = BTreeSet::new();
        for file in REPOSITORY_FILES {
            assert!(seen.insert(file.path), "{} is declared twice", file.path);
            let relative = std::path::Path::new(file.path);
            assert!(relative.is_relative(), "{}", file.path);
            assert!(relative.starts_with(DIRECTORY), "{}", file.path);
        }
        assert_eq!(section_pointer(LAND_SECTION), "/domains/pr/land");
    }
}
