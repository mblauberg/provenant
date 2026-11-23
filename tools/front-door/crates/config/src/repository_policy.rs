//! Opt-in launch classification from the repository's project configuration.
use crate::ConfigError;
use provenant_os::fs::Directory;
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    ffi::OsStr,
    io::Read,
    path::{Component, Path, PathBuf},
};

pub const REPOSITORY_CONFIG: &str = ".provenant/config.toml";

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct RepositoryPolicy {
    #[serde(default)]
    pub confidential: bool,
    /// Repository-relative files or directory prefixes, not shell patterns.
    #[serde(default)]
    pub protected_paths: Vec<PathBuf>,
}

/// The `[launch_policy]` section (`repository_files::LAUNCH_POLICY_SECTION`).
#[derive(Deserialize, Serialize)]
struct ProjectConfig {
    #[serde(default)]
    launch_policy: RepositoryPolicy,
    #[serde(default)]
    domains: ProjectDomains,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ProjectDomains {
    #[serde(default)]
    pr: ProjectPrDomains,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ProjectPrDomains {}

fn decode_project_config(
    source: &str,
    text: &str,
) -> Result<(ProjectConfig, Vec<provenant_records::InputWarning>), ConfigError> {
    let config: ProjectConfig =
        toml_edit::de::from_str(text).map_err(|_| ConfigError::InvalidRepositoryPolicy)?;
    let document: toml_edit::DocumentMut = text
        .parse()
        .map_err(|_| ConfigError::InvalidRepositoryPolicy)?;
    let accepted =
        serde_json::to_value(&config).map_err(|_| ConfigError::InvalidRepositoryPolicy)?;
    let mut warnings = Vec::new();
    collect_unknown_toml_keys(document.as_table(), &accepted, "", source, &mut warnings);
    Ok((config, warnings))
}

fn collect_unknown_toml_keys(
    table: &toml_edit::Table,
    accepted: &serde_json::Value,
    prefix: &str,
    source: &str,
    warnings: &mut Vec<provenant_records::InputWarning>,
) {
    for (key, item) in table.iter() {
        let path = if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        };
        if path == crate::repository_files::LAND_SECTION || path == "domains.routing" {
            continue;
        }
        let Some(accepted) = accepted.get(key) else {
            warnings.push(provenant_records::InputWarning {
                source: source.to_owned(),
                key: path,
            });
            continue;
        };
        if let Some(child) = item.as_table() {
            collect_unknown_toml_keys(child, accepted, &path, source, warnings);
        } else if let Some(child) = item.as_inline_table() {
            collect_unknown_inline_toml_keys(child, accepted, &path, source, warnings);
        }
    }
}

fn collect_unknown_inline_toml_keys(
    table: &toml_edit::InlineTable,
    accepted: &serde_json::Value,
    prefix: &str,
    source: &str,
    warnings: &mut Vec<provenant_records::InputWarning>,
) {
    for (key, value) in table.iter() {
        let path = format!("{prefix}.{key}");
        if path == crate::repository_files::LAND_SECTION || path == "domains.routing" {
            continue;
        }
        let Some(accepted) = accepted.get(key) else {
            warnings.push(provenant_records::InputWarning {
                source: source.to_owned(),
                key: path,
            });
            continue;
        };
        if let Some(child) = value.as_inline_table() {
            collect_unknown_inline_toml_keys(child, accepted, &path, source, warnings);
        }
    }
}

fn warn_project_config(warnings: &[provenant_records::InputWarning]) {
    use std::io::Write;
    for warning in warnings {
        let _ = writeln!(
            std::io::stderr().lock(),
            "warning: {}: {}: unknown configuration key; ignored",
            warning.source,
            warning.key
        );
    }
}

/// Read the landing declaration from the same project file as launch policy.
/// Known type errors remain errors; unknown keys are diagnosed and ignored.
pub fn read_land<T: serde::de::DeserializeOwned + Serialize>(
    checkout: &Path,
) -> Result<Option<T>, ConfigError> {
    let Some(text) = read_declaration(checkout)? else {
        return Ok(None);
    };
    let source = checkout.join(REPOSITORY_CONFIG).display().to_string();
    let (_, mut warnings) = decode_project_config(&source, &text)?;
    let config: serde_json::Value =
        toml_edit::de::from_str(&text).map_err(|_| ConfigError::InvalidRepositoryPolicy)?;
    let land = config
        .pointer(&crate::repository_files::section_pointer(
            crate::repository_files::LAND_SECTION,
        ))
        .cloned();
    let Some(land) = land else {
        warn_project_config(&warnings);
        return Ok(None);
    };
    let (land, land_warnings) = decode_land(&source, &land)?;
    warnings.extend(land_warnings);
    warn_project_config(&warnings);
    Ok(Some(land))
}

fn decode_land<T: serde::de::DeserializeOwned + Serialize>(
    source: &str,
    value: &serde_json::Value,
) -> Result<(T, Vec<provenant_records::InputWarning>), ConfigError> {
    let bytes = serde_json::to_vec(value).map_err(|_| ConfigError::InvalidRepositoryPolicy)?;
    let (land, warnings) = provenant_records::decode_input(source, &bytes)
        .map_err(|_| ConfigError::InvalidRepositoryPolicy)?;
    let warnings = warnings
        .into_iter()
        .map(|mut warning| {
            warning.key = if warning.key.is_empty() {
                crate::repository_files::LAND_SECTION.into()
            } else {
                format!("{}.{}", crate::repository_files::LAND_SECTION, warning.key)
            };
            warning
        })
        .collect();
    Ok((land, warnings))
}

/// Read the bounded, no-follow repository configuration declaration.
pub fn read_declaration(checkout: &Path) -> Result<Option<String>, ConfigError> {
    let root = Directory::open_anchor(checkout)?;
    let directory =
        match root.open_verified_directory(OsStr::new(crate::repository_files::DIRECTORY)) {
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
        let source = checkout.join(REPOSITORY_CONFIG).display().to_string();
        let (config, warnings) = decode_project_config(&source, &text)?;
        warn_project_config(&warnings);
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
        if let Some(rest) = path.strip_prefix("...") {
            return Some(rest);
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
        #[derive(Deserialize, Serialize)]
        struct Declaration {
            strategy: provenant_records::landing::Strategy,
        }
        let root = tempfile::tempdir()?;
        assert!(read_land::<Declaration>(root.path())?.is_none());
        std::fs::create_dir(root.path().join(".provenant"))?;
        std::fs::write(
            root.path().join(REPOSITORY_CONFIG),
            "[launch_policy]\nconfidential=true\n[domains.pr.land]\nstrategy='queue'\nfuture=true\n",
        )?;
        assert_eq!(
            read_land::<Declaration>(root.path())?
                .ok_or("missing declaration")?
                .strategy,
            provenant_records::landing::Strategy::Queue
        );
        let source = root.path().join(REPOSITORY_CONFIG).display().to_string();
        let document: serde_json::Value = toml_edit::de::from_str(&std::fs::read_to_string(
            root.path().join(REPOSITORY_CONFIG),
        )?)?;
        let land = document
            .pointer(&crate::repository_files::section_pointer(
                crate::repository_files::LAND_SECTION,
            ))
            .ok_or("missing land section")?;
        let (decoded, warnings) = decode_land::<Declaration>(&source, land)?;
        assert_eq!(
            decoded.strategy,
            provenant_records::landing::Strategy::Queue
        );
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].source, source);
        assert_eq!(
            warnings[0].key,
            format!("{}.future", crate::repository_files::LAND_SECTION)
        );
        assert!(
            RepositoryPolicy::read(root.path())?
                .ok_or("missing launch policy")?
                .confidential
        );
        std::fs::write(
            root.path().join(REPOSITORY_CONFIG),
            "[domains.pr.land]\nstrategy='unsupported'\n",
        )?;
        assert!(read_land::<Declaration>(root.path()).is_err());
        Ok(())
    }

    #[test]
    fn launch_policy_unknown_keys_warn_with_the_repository_config_source()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::repository_files::{LAUNCH_POLICY_KEYS, LAUNCH_POLICY_SECTION};
        let root = tempfile::tempdir()?;
        std::fs::create_dir(root.path().join(".provenant"))?;
        let value = |key: &str| if key == "confidential" { "true" } else { "[]" };
        for key in LAUNCH_POLICY_KEYS.iter().copied() {
            std::fs::write(
                root.path().join(REPOSITORY_CONFIG),
                format!("[{LAUNCH_POLICY_SECTION}]\n{key}={}\n", value(key)),
            )?;
            assert!(RepositoryPolicy::read(root.path())?.is_some(), "{key}");
        }

        let text = format!(
            "[{LAUNCH_POLICY_SECTION}]\nconfidential=true\nprotected_paths=[]\nundeclared=true\n[future_section]\nvalue=1\n[domains.routing.review.floors.example]\nhard=true\n"
        );
        std::fs::write(root.path().join(REPOSITORY_CONFIG), &text)?;
        assert!(RepositoryPolicy::read(root.path())?.is_some());
        let source = root.path().join(REPOSITORY_CONFIG).display().to_string();
        let (_, warnings) = decode_project_config(&source, &text)?;
        let actual: std::collections::BTreeSet<_> = warnings
            .into_iter()
            .map(|warning| (warning.source, warning.key))
            .collect();
        assert_eq!(
            actual,
            std::collections::BTreeSet::from([
                (source.clone(), "future_section".into()),
                (source, "launch_policy.undeclared".into()),
            ])
        );
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
