//! Conflict override for Codex's bundled skill-creator.
use crate::{invalid, io, Result};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item};

pub const SKILL_NAME: &str = "skill-creator";
pub const BLOCK: &str = "# agent-harness: disable overlapping bundled skill\n[[skills.config]]\nname = \"skill-creator\"\nenabled = false\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandMode {
    Check,
    Preflight,
    Configure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub fn default_config_path(codex_home: Option<&OsStr>, home: &Path) -> PathBuf {
    codex_home
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"))
        .join("config.toml")
}

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
    if provenant_core::py::python_strip(text).is_empty() {
        return Ok(State::Missing);
    }
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
    let mut conflicting = false;
    for entry in entries {
        if entry.get("name").and_then(Item::as_str) != Some(SKILL_NAME) {
            continue;
        }
        found = true;
        let enabled = entry.get("enabled");
        if let Some(enabled) = enabled {
            if enabled.as_array().is_some() || matches!(enabled, Item::ArrayOfTables(_)) {
                return Err(crate::Error::CodexUnhashable { kind: "list" });
            }
            if enabled.as_table_like().is_some() {
                return Err(crate::Error::CodexUnhashable { kind: "dict" });
            }
        }
        // Python's False compares equal to integer/float zero.
        let disabled = enabled.is_some_and(|v| {
            v.as_bool() == Some(false) || v.as_integer() == Some(0) || v.as_float() == Some(0.0)
        });
        conflicting |= !disabled;
    }
    if conflicting {
        return Err(invalid(
            "skill-creator has a conflicting enabled or malformed override",
        ));
    }
    Ok(if found {
        State::Disabled
    } else {
        State::Missing
    })
}

pub fn canonical_path(path: &Path) -> Result<PathBuf> {
    if path.is_symlink() {
        let target = fs::canonicalize(path).map_err(|source| crate::Error::CodexSymlink {
            path: path.into(),
            source,
        })?;
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
        let bytes = fs::read(&target).map_err(|e| io(&target, e))?;
        String::from_utf8(bytes)
            .map_err(|source| crate::Error::CodexEncoding {
                path: target.clone(),
                source,
            })?
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
    crate::filesystem::atomic_write(&proposal.target, ".codex-config.", proposal.text.as_bytes())?;
    Ok(Outcome::Configured)
}

/// The inert command boundary, including the script's output and exit policy.
pub fn run(path: &Path, mode: CommandMode) -> CommandResult {
    match run_inner(path, mode) {
        Ok(result) => result,
        Err(error @ crate::Error::CodexUnhashable { .. }) => CommandResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: format!("TypeError: {error}\n"),
        },
        Err(error) => CommandResult {
            exit_code: 3,
            stdout: String::new(),
            stderr: format!("conflicting: {error}\n"),
        },
    }
}

fn run_inner(path: &Path, mode: CommandMode) -> Result<CommandResult> {
    let path_display = path.display();
    match mode {
        CommandMode::Check => {
            let target = canonical_path(path)?;
            let text = if target.exists() {
                fs::read_to_string(&target)
                    .map_err(|e| io(&target, e))?
                    .replace("\r\n", "\n")
                    .replace('\r', "\n")
            } else {
                String::new()
            };
            if state(&text)? == State::Disabled {
                Ok(CommandResult {
                    exit_code: 0,
                    stdout: format!("skill override verified={path_display}\n"),
                    stderr: String::new(),
                })
            } else {
                Ok(CommandResult {
                    exit_code: 1,
                    stdout: format!("missing: {SKILL_NAME} disable override in {path_display}\n"),
                    stderr: String::new(),
                })
            }
        }
        CommandMode::Preflight => {
            let outcome = preflight(path)?;
            let value = match outcome {
                Outcome::Existing => "existing",
                Outcome::Ready => "ready",
                Outcome::Configured => "configured",
            };
            Ok(CommandResult {
                exit_code: 0,
                stdout: format!("skill override preflight {value}={path_display}\n"),
                stderr: String::new(),
            })
        }
        CommandMode::Configure => {
            let outcome = configure(path)?;
            let value = match outcome {
                Outcome::Existing => "existing",
                Outcome::Ready => "ready",
                Outcome::Configured => "configured",
            };
            Ok(CommandResult {
                exit_code: 0,
                stdout: format!("skill override {value}={path_display}\n"),
                stderr: String::new(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    #[test]
    fn invalid_toml_keeps_python_prefix_and_typed_source() {
        let error = state("[skills").unwrap_err();
        assert!(error
            .to_string()
            .starts_with("Codex config is invalid TOML: "));
        assert!(matches!(error, crate::Error::Toml(_)));
    }
    #[test]
    fn proposed_update_rejects_non_utf8_without_modifying_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let bytes = b"# invalid byte: \xff\n";
        fs::write(&path, bytes).unwrap();

        let error = proposed_update(&path).unwrap_err();
        let source = std::error::Error::source(&error).unwrap();
        assert!(source.is::<std::string::FromUtf8Error>());
        assert!(matches!(error, crate::Error::CodexEncoding { path: p, .. } if p == path));
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
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
        let error = configure(&link).unwrap_err();
        assert!(error
            .to_string()
            .starts_with("Codex config symlink cannot be resolved: "));
        assert!(error.to_string().contains(link.to_str().unwrap()));
        assert!(matches!(error, crate::Error::CodexSymlink { .. }));
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

    #[test]
    fn command_outputs_and_exit_codes_match_python() {
        for (mode, args, original) in [
            (CommandMode::Check, vec!["--check"], ""),
            (CommandMode::Preflight, vec!["--preflight"], ""),
            (CommandMode::Configure, vec![], ""),
            (
                CommandMode::Check,
                vec!["--check"],
                "[[skills.config]]\nname='skill-creator'\nenabled=false\n",
            ),
            (
                CommandMode::Preflight,
                vec!["--preflight"],
                "[[skills.config]]\nname='skill-creator'\nenabled=true\n",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let rust_path = dir.path().join("rust.toml");
            fs::write(&rust_path, original).unwrap();
            let script_args = args
                .iter()
                .map(|arg| format!("{arg:?}"))
                .collect::<Vec<_>>()
                .join(",");
            let body = format!(
                "import contextlib,io,json\nout=io.StringIO();err=io.StringIO()\nwith contextlib.redirect_stdout(out),contextlib.redirect_stderr(err): code=m.main(['--config',str(p),{script_args}])\nprint(json.dumps([code,out.getvalue(),err.getvalue()]))"
            );
            let Some(output) = crate::oracle::python(
                "scripts/configure-codex-skill-overrides.py",
                &body,
                &rust_path,
            ) else {
                return;
            };
            let old: serde_json::Value = serde_json::from_slice(&output).unwrap();
            fs::write(&rust_path, original).unwrap();
            let result = run(&rust_path, mode);
            assert_eq!(
                old,
                serde_json::json!([result.exit_code, result.stdout, result.stderr]),
                "mode={mode:?}, original={original:?}"
            );
        }
    }

    #[test]
    fn unhashable_enabled_values_keep_python_type_error_and_exit_status() {
        for (value, kind) in [("[]", "list"), ("{key=1}", "dict")] {
            let input = format!("[[skills.config]]\nname='skill-creator'\nenabled={value}\n");
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");
            fs::write(&path, &input).unwrap();
            let body =
                "try: m.state(p.read_text())\nexcept Exception as e: print(type(e).__name__ + ':' + str(e))";
            let Some(output) =
                crate::oracle::python("scripts/configure-codex-skill-overrides.py", body, &path)
            else {
                return;
            };
            assert_eq!(
                output,
                format!(
                    "TypeError:cannot use '{kind}' as a set element (unhashable type: '{kind}')\n"
                )
                .as_bytes()
            );
            let result = run(&path, CommandMode::Configure);
            assert_eq!(result.exit_code, 1);
            assert_eq!(
                result.stderr,
                format!(
                    "TypeError: cannot use '{kind}' as a set element (unhashable type: '{kind}')\n"
                )
            );
            assert_eq!(fs::read_to_string(path).unwrap(), input);
        }
    }
}

#[derive(Debug, clap::Parser)]
#[command(name = "configure-codex-skill-overrides.py")]
pub struct Arguments {
    #[arg(long)]
    pub config: Option<PathBuf>,
    #[arg(long, conflicts_with = "preflight")]
    pub check: bool,
    #[arg(long)]
    pub preflight: bool,
}
impl Arguments {
    pub fn run(&self, codex_home: Option<&OsStr>, home: &Path) -> CommandResult {
        let path = self
            .config
            .clone()
            .unwrap_or_else(|| default_config_path(codex_home, home));
        run(
            &path,
            if self.check {
                CommandMode::Check
            } else if self.preflight {
                CommandMode::Preflight
            } else {
                CommandMode::Configure
            },
        )
    }
}

#[cfg(test)]
mod edge_tests {
    use super::*;
    #[test]
    fn bare_cr_check_whitespace_and_set_collection_precedence_match_python() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for original in [
            "\u{1c}\u{1f}",
            "[[skills.config]]\rname='skill-creator'\renabled=false\r",
        ] {
            fs::write(&path, original).unwrap();
            let Some(old)=crate::oracle::python("scripts/configure-codex-skill-overrides.py","import json,io,contextlib\no=io.StringIO()\nwith contextlib.redirect_stdout(o): code=m.main(['--config',str(p),'--check'])\nprint(json.dumps([code,o.getvalue()]))",&path)else{return;};
            let result = run(&path, CommandMode::Check);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&old).unwrap(),
                serde_json::json!([result.exit_code, result.stdout])
            );
        }
        let input="[[skills.config]]\nname='skill-creator'\nenabled=true\n[[skills.config]]\nname='skill-creator'\nenabled=[]\n";
        assert!(matches!(
            state(input),
            Err(crate::Error::CodexUnhashable { kind: "list" })
        ));
    }
}
