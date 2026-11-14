//! Opt-in launch classification from the repository's project configuration.
use crate::ConfigError;
use provenant_os::fs::Directory;
use serde::Deserialize;
use std::{
    borrow::Cow,
    ffi::OsStr,
    io::Read,
    path::{Component, Path, PathBuf},
};

pub const REPOSITORY_CONFIG: &str = ".provenant/config.toml";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryPolicy {
    #[serde(default)]
    pub confidential: bool,
    /// Repository-relative files or directory prefixes, not shell patterns.
    #[serde(default)]
    pub protected_paths: Vec<PathBuf>,
}

#[derive(Deserialize)]
struct ProjectConfig {
    #[serde(default)]
    launch_policy: RepositoryPolicy,
}

/// Read the landing declaration from the same project file as launch policy.
/// `T` is the domain owner's strict override type, so obsolete keys are rejected.
pub fn read_land<T: serde::de::DeserializeOwned>(
    checkout: &Path,
) -> Result<Option<T>, ConfigError> {
    let Some(text) = read_declaration(checkout)? else {
        return Ok(None);
    };
    let config: serde_json::Value =
        toml_edit::de::from_str(&text).map_err(|_| ConfigError::InvalidRepositoryPolicy)?;
    config
        .pointer("/domains/pr/land")
        .cloned()
        .map(|value| {
            serde_json::from_value(value).map_err(|_| ConfigError::InvalidRepositoryPolicy)
        })
        .transpose()
}

fn read_declaration(checkout: &Path) -> Result<Option<String>, ConfigError> {
    let root = Directory::open_anchor(checkout)?;
    let directory = match root.open_verified_directory(OsStr::new(".provenant")) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let file = match directory.open_read(OsStr::new("config.toml")) {
        Ok(file) => file,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut text = String::new();
    file.take(64 * 1024 + 1)
        .read_to_string(&mut text)
        .map_err(|_| ConfigError::InvalidRepositoryPolicy)?;
    if text.len() > 64 * 1024 {
        return Err(ConfigError::InvalidRepositoryPolicy);
    }
    Ok(Some(text))
}

impl RepositoryPolicy {
    /// Only absence means no policy. An unreadable or malformed declaration
    /// cannot silently remove the repository's confidentiality boundary.
    pub fn read(checkout: &Path) -> Result<Option<Self>, ConfigError> {
        let Some(text) = read_declaration(checkout)? else {
            return Ok(None);
        };
        let config: ProjectConfig =
            toml_edit::de::from_str(&text).map_err(|_| ConfigError::InvalidRepositoryPolicy)?;
        if config.launch_policy.protected_paths.iter().any(|path| {
            let path = path_with_separators(path);
            path.as_os_str().is_empty()
                || path.components().any(|part| {
                    matches!(
                        part,
                        Component::ParentDir | Component::RootDir | Component::Prefix(_)
                    )
                })
        }) {
            return Err(ConfigError::InvalidRepositoryPolicy);
        }
        Ok(Some(config.launch_policy))
    }

    /// A directory input may contain a protected file, so overlap in either
    /// direction classifies it. Canonical paths also catch symlink aliases.
    pub fn protects(&self, checkout: &Path, cwd: &Path, path: &Path) -> bool {
        let path = path_with_separators(path);
        let input = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path.as_ref())
        };
        let input = normalise(&input);
        self.protected_paths.iter().any(|protected| {
            let protected = normalise(&checkout.join(protected));
            let overlaps = |a: &Path, b: &Path| a.starts_with(b) || b.starts_with(a);
            overlaps(&input, &protected)
                || input
                    .canonicalize()
                    .ok()
                    .zip(protected.canonicalize().ok())
                    .is_some_and(|(input, protected)| overlaps(&input, &protected))
        })
    }

    pub fn protects_brief(&self, checkout: &Path, cwd: &Path, brief: &str) -> bool {
        let matches_reference = |token: &str| {
            // Sentence punctuation is not a reference to the cwd or its parent.
            if token.chars().all(sentence_punctuation) {
                return false;
            }
            path_candidates(token).any(|path| self.protects(checkout, cwd, Path::new(path)))
        };
        brief.split_whitespace().any(|token| {
            matches_reference(token) || token.split(path_delimiter).any(matches_reference)
        })
    }
}

/// Yield the literal token first, then candidates with prose wrappers peeled
/// from its edges. Prefix peeling stops when the remaining bytes begin a path
/// root or relative marker, so normalization sees `./` and `../` intact.
fn path_candidates(token: &str) -> impl Iterator<Item = &str> {
    let preserve_initial_path = begins_path_syntax(token);
    std::iter::successors(Some(token), move |path| {
        if preserve_initial_path || begins_path_syntax(path) {
            return None;
        }
        // An ellipsis is a prose wrapper as a unit. It may expose `./`, `../`,
        // or an absolute path, whose marker must then remain intact.
        if path.starts_with("...") {
            return Some(&path[3..]);
        }
        let first = path.chars().next()?;
        (first != '.' && sentence_punctuation(first)).then_some(&path[first.len_utf8()..])
    })
    .flat_map(|path| {
        std::iter::successors(Some(path), |candidate| {
            let last = candidate.chars().next_back()?;
            sentence_punctuation(last).then_some(&candidate[..candidate.len() - last.len_utf8()])
        })
    })
    .filter(|candidate| !candidate.is_empty())
}

fn begins_path_syntax(path: &str) -> bool {
    path.starts_with('/')
        || path.starts_with('\\')
        || path.starts_with("./")
        || path.starts_with(".\\")
        || path.starts_with("../")
        || path.starts_with("..\\")
}

fn sentence_punctuation(c: char) -> bool {
    c.is_ascii_punctuation() && !matches!(c, '/' | '\\')
        || path_delimiter(c)
        || matches!(c, '…' | '。' | '，' | '！' | '？')
}

fn path_delimiter(c: char) -> bool {
    matches!(
        c,
        '`' | '\''
            | '"'
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '<'
            | '>'
            | ','
            | ';'
            | ':'
            | '!'
            | '?'
            | '*'
            | '“'
            | '”'
            | '‘'
            | '’'
            | '«'
            | '»'
    )
}

fn path_with_separators(path: &Path) -> Cow<'_, Path> {
    match path.to_str().filter(|text| text.contains('\\')) {
        Some(text) => Cow::Owned(PathBuf::from(text.replace('\\', "/"))),
        None => Cow::Borrowed(path),
    }
}

fn normalise(path: &Path) -> PathBuf {
    // Briefs and declarations may spell separators in either common form.
    let path = path_with_separators(path);
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_file_classification_is_invariant_under_prose_wrappers() {
        let checkout = Path::new("/fixture/repo");
        let policy = RepositoryPolicy {
            confidential: false,
            protected_paths: vec!["private/brief.md".into()],
        };
        for path in [
            "private/brief.md",
            "./private/brief.md",
            "private\\brief.md",
            "./private//brief.md",
            "/fixture/repo/private/brief.md",
            r"\fixture\repo\private\brief.md",
        ] {
            for prefix in [
                "", "(", "[", "{", "<", "\"", "'", "`", "“", "‘", "**", "...", "#", "@", "=", "_",
                "-",
            ] {
                for suffix in [
                    "", ".", ",", ":", ";", "!", "?", ")", "]", "}", ">", "\"", "'", "`", "”", "’",
                    "**.", ").", "]:",
                ] {
                    let brief = format!("Read {prefix}{path}{suffix}");
                    assert!(policy.protects_brief(checkout, checkout, &brief), "{brief}");
                    assert!(!policy.protects_brief(
                        checkout,
                        checkout,
                        &brief.replace("private", "public")
                    ));
                }
            }
        }
        for reference in [
            "[brief](private/brief.md).",
            "Read:private/brief.md.",
            "Read \"private/brief.md\".",
        ] {
            assert!(policy.protects_brief(checkout, checkout, reference));
            assert!(!policy.protects_brief(
                checkout,
                checkout,
                &reference.replace("private", "public")
            ));
        }
        // Literal filename punctuation remains significant, not discarded.
        for name in [
            "private/brief.md.",
            "private/a,b.md",
            "private/.hidden",
            "private/a!b.md",
            "private/_brief.md",
        ] {
            let policy = RepositoryPolicy {
                confidential: false,
                protected_paths: vec![name.into()],
            };
            for brief in [
                format!("Read {name}"),
                format!("Read `{name}`"),
                format!("Read ({name})"),
                format!("Read ({name})."),
            ] {
                assert!(policy.protects_brief(checkout, checkout, &brief));
            }
        }
    }

    #[test]
    fn landing_uses_the_existing_project_declaration() -> Result<(), Box<dyn std::error::Error>> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Declaration {
            strategy: provenant_records::landing::Strategy,
        }
        let root = tempfile::tempdir()?;
        assert!(read_land::<Declaration>(root.path())?.is_none());
        std::fs::create_dir(root.path().join(".provenant"))?;
        std::fs::write(
            root.path().join(REPOSITORY_CONFIG),
            "[launch_policy]\nconfidential=true\n[domains.pr.land]\nstrategy='queue'\n",
        )?;
        assert_eq!(
            read_land::<Declaration>(root.path())?
                .ok_or("missing declaration")?
                .strategy,
            provenant_records::landing::Strategy::Queue
        );
        assert!(
            RepositoryPolicy::read(root.path())?
                .ok_or("missing launch policy")?
                .confidential
        );
        std::fs::write(
            root.path().join(REPOSITORY_CONFIG),
            "[domains.pr.land]\nintegration='single'\n",
        )?;
        assert!(read_land::<Declaration>(root.path()).is_err());
        Ok(())
    }

    #[test]
    fn declared_policy_classifies_inputs_and_briefs_without_implicit_policy()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        assert!(RepositoryPolicy::read(root.path())?.is_none());
        std::fs::create_dir(root.path().join(".provenant"))?;
        std::fs::write(
            root.path().join(REPOSITORY_CONFIG),
            "[launch_policy]\nprotected_paths=['private']\n",
        )?;
        let policy = RepositoryPolicy::read(root.path())?.ok_or("missing policy")?;
        assert!(policy.protects(root.path(), root.path(), Path::new("private/file.md")));
        assert!(policy.protects_brief(root.path(), root.path(), "Read `private/brief.md`"));
        assert!(!policy.protects_brief(root.path(), root.path(), "Read public/brief.md"));
        for path in ["../escape", r"..\escape", r"\escape", "/escape"] {
            std::fs::write(
                root.path().join(REPOSITORY_CONFIG),
                format!("[launch_policy]\nprotected_paths=['{path}']\n"),
            )?;
            assert!(RepositoryPolicy::read(root.path()).is_err(), "{path}");
        }
        Ok(())
    }
}
