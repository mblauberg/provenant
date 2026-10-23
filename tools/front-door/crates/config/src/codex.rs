//! Conflict override for Codex's bundled skill-creator.
use crate::{invalid, io, pending, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item};

pub const SKILL_NAME: &str = "skill-creator";
pub const BLOCK: &str = "# agent-harness: disable overlapping bundled skill\n[[skills.config]]\nname = \"skill-creator\"\nenabled = false\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Missing,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Existing,
    Ready,
    Configured,
}

pub fn state(text: &str) -> Result<State> {
    let doc: DocumentMut = text.parse()?;
    let Some(skills) = doc.get("skills") else {
        return Ok(State::Missing);
    };
    let skills = skills
        .as_table_like()
        .ok_or_else(|| invalid("Codex config skills value must be a table"))?;
    let Some(config) = skills.get("config") else {
        return Ok(State::Missing);
    };
    let entries: Vec<&dyn toml_edit::TableLike> = match config {
        Item::ArrayOfTables(tables) => tables
            .iter()
            .map(|t| t as &dyn toml_edit::TableLike)
            .collect(),
        Item::Value(toml_edit::Value::Array(array)) => array
            .iter()
            .map(|v| {
                v.as_inline_table()
                    .map(|t| t as &dyn toml_edit::TableLike)
                    .ok_or_else(|| {
                        invalid("Codex config skills.config value must be an array of tables")
                    })
            })
            .collect::<Result<_>>()?,
        _ => {
            return Err(invalid(
                "Codex config skills.config value must be an array of tables",
            ))
        }
    };
    let mut found = false;
    for entry in entries {
        if entry.get("name").and_then(Item::as_str) != Some(SKILL_NAME) {
            continue;
        }
        found = true;
        let enabled = entry.get("enabled");
        // Python's False compares equal to integer/float zero.
        let disabled = enabled.is_some_and(|v| {
            v.as_bool() == Some(false) || v.as_integer() == Some(0) || v.as_float() == Some(0.0)
        });
        if !disabled {
            return Err(invalid(
                "skill-creator has a conflicting enabled or malformed override",
            ));
        }
    }
    Ok(if found {
        State::Disabled
    } else {
        State::Missing
    })
}

pub fn canonical_path(path: &Path) -> Result<PathBuf> {
    if path.is_symlink() {
        let target = fs::canonicalize(path).map_err(|e| io(path, e))?;
        if !target.is_file() {
            return Err(invalid("Codex config symlink must target a regular file"));
        }
        Ok(target)
    } else {
        Ok(path.into())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Proposal {
    pub target: PathBuf,
    pub text: String,
    pub outcome: Outcome,
}

pub fn proposed_update(path: &Path) -> Result<Proposal> {
    let target = canonical_path(path)?;
    let mut text = if target.exists() {
        fs::read_to_string(&target)
            .map_err(|e| io(&target, e))?
            .replace("\r\n", "\n")
            .replace('\r', "\n")
    } else {
        String::new()
    };
    if state(&text)? == State::Disabled {
        return Ok(Proposal {
            target,
            text,
            outcome: Outcome::Existing,
        });
    }
    if !text.is_empty() {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push('\n');
    }
    text.push_str(BLOCK);
    if state(&text)? != State::Disabled {
        return Err(invalid(
            "composed Codex config does not disable skill-creator",
        ));
    }
    Ok(Proposal {
        target,
        text,
        outcome: Outcome::Ready,
    })
}

pub fn preflight(path: &Path) -> Result<Outcome> {
    Ok(proposed_update(path)?.outcome)
}

pub fn configure(path: &Path) -> Result<Outcome> {
    let proposal = proposed_update(path)?;
    if proposal.outcome == Outcome::Existing {
        return Ok(Outcome::Existing);
    }
    pending::atomic_write(&proposal.target, ".codex-config.", proposal.text.as_bytes())?;
    Ok(Outcome::Configured)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    #[test]
    fn conflicts_are_read_only() {
        for original in [
            "[[skills.config]]\nname='skill-creator'\nenabled=true\n",
            "[skills]\nconfig=[{name='other',enabled=true}]\n",
            "skills=1",
            "[skills]\nconfig=[1]",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");
            fs::write(&path, original).unwrap();
            assert!(configure(&path).is_err());
            assert_eq!(fs::read_to_string(path).unwrap(), original);
        }
    }
    #[test]
    fn preserves_symlink_and_configures_once() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("dotfile");
        let link = dir.path().join("config.toml");
        fs::write(&target, "[custom]\nvalue='preserved'").unwrap();
        symlink(&target, &link).unwrap();
        assert_eq!(preflight(&link).unwrap(), Outcome::Ready);
        assert!(!fs::read_to_string(&target).unwrap().contains(SKILL_NAME));
        assert_eq!(configure(&link).unwrap(), Outcome::Configured);
        let bytes = fs::read(&target).unwrap();
        assert_eq!(configure(&link).unwrap(), Outcome::Existing);
        assert_eq!(fs::read(&target).unwrap(), bytes);
        assert!(link.is_symlink());
    }
    #[test]
    fn rejects_broken_links_and_directory_links() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("config");
        symlink(dir.path().join("absent"), &link).unwrap();
        assert!(configure(&link).is_err());
        fs::remove_file(&link).unwrap();
        symlink(dir.path(), &link).unwrap();
        assert!(configure(&link).is_err());
    }
    #[test]
    fn duplicate_disabled_and_python_zero_are_accepted() {
        assert_eq!(state("[[skills.config]]\nname='skill-creator'\nenabled=false\n[[skills.config]]\nname='skill-creator'\nenabled=0\n").unwrap(), State::Disabled);
        assert_eq!(
            state("[skills]\nconfig=[{name='skill-creator', enabled=false}]").unwrap(),
            State::Disabled
        );
    }
    #[test]
    fn python_differential_proposals() {
        for original in [
            "",
            "# comment\n[custom]\na='é'",
            "[skills]\nconfig=[{name='other'}]",
            "[[skills.config]]\nname='skill-creator'\nenabled=false\n",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config");
            fs::write(&path, original).unwrap();
            let Some(output) = crate::oracle::python("scripts/configure-codex-skill-overrides.py", "import json;\ntry:\n t,s,r=m.proposed_update(p); print(json.dumps([s,r]))\nexcept (OSError,ValueError): print('null')", &path) else { return; };
            let old: serde_json::Value = serde_json::from_slice(&output).unwrap();
            match proposed_update(&path) {
                Ok(proposal) => assert_eq!(
                    old,
                    serde_json::json!([
                        proposal.text,
                        if proposal.outcome == Outcome::Existing {
                            "existing"
                        } else {
                            "ready"
                        }
                    ])
                ),
                Err(_) => assert!(old.is_null()),
            }
        }
    }
}
