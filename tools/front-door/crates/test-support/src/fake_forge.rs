//! The base branch's required status checks as a fake forge serves them.
//!
//! The CI observer settles a head only against a non-empty required set, read
//! from branch protection. Every fake `gh` that serves pull requests shares
//! this one contract so a rig names its required checks the same way and the
//! forge answers the same shape of reads.

use std::{io, path::Path};

use serde_json::{Value, json};

/// The rig file, in the fake forge's state directory, listing one required
/// check name per line.
pub const REQUIRED_CHECKS_FILE: &str = "required-checks";

/// Declare `checks` (newline separated) as the base branch's required checks.
pub fn require_checks(dir: &Path, checks: &str) -> io::Result<()> {
    std::fs::write(dir.join(REQUIRED_CHECKS_FILE), format!("{checks}\n"))
}

/// The declared required checks; none when the rig declared none.
pub fn required_checks(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join(REQUIRED_CHECKS_FILE))
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `GET repos/{owner}/{repo}/branches/{branch}`: protected exactly when a
/// required set is declared.
pub fn branch_read(branch: &str, contexts: &[String]) -> Value {
    let protected = !contexts.is_empty();
    json!({
        "name": branch,
        "protected": protected,
        "protection": {
            "enabled": protected,
            "required_status_checks": {
                "enforcement_level": if protected { "everyone" } else { "off" },
                "contexts": contexts,
                "checks": [],
            },
        },
    })
}

/// `GET repos/{owner}/{repo}/branches/{branch}/protection`: the classic
/// protection the observer reads its required set from. `None` is the forge's
/// 404 for an unprotected branch.
pub fn protection_read(contexts: &[String]) -> Option<Value> {
    (!contexts.is_empty())
        .then(|| json!({"required_status_checks": {"contexts": contexts, "checks": []}}))
}
