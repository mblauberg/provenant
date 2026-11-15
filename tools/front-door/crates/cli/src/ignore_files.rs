//! Provider ignore files that hide a lane worktree from the lane's own agent.
//!
//! Some providers honour an ignore file of the checkout that contains the
//! lane's worktree: Cursor reads the parent checkout's `.cursorignore`, so a
//! lane under `.worktrees/` cannot read or write its own files when that file
//! lists `.worktrees/`. Each adapter declares such files in
//! `AdapterSpec::ignore_files`; this module reads the declared files
//! generically and reports the pattern that hides a path. It only warns: an
//! owner may keep the pattern on purpose (to stop an IDE indexing worktrees),
//! and the remedy is the worktree layout port, never an edit of the file.
//!
//! Patterns follow gitignore for the common forms: blank lines and `#`
//! comments are skipped; `!` re-includes; a trailing `/` marks a directory
//! (every checked path is a directory, so it only strips); a leading `/` or an
//! inner `/` anchors the pattern to the checkout root, otherwise it matches a
//! single path component at any depth; `*` and `?` stay within a component and
//! `**` spans components. As in Git, once a directory is ignored nothing below
//! it can be re-included. Backslash escapes and trailing-space escapes are not
//! interpreted.
use provenant_config::{PortSource, PortTable};
use provenant_records::AdapterSpec;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// Largest declared ignore file read; a larger one is reported unreadable.
pub const IGNORE_FILE_LIMIT: u64 = 1024 * 1024;

/// A placeholder branch used to render the layout's default worktree path.
const PROBE_BRANCH: &str = "lane";

/// What a declared ignore file says about one checkout-relative path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IgnoreFinding {
    /// `pattern`, on `line` of `file`, hides `path` from `adapter`'s agent.
    Hidden {
        adapter: &'static str,
        file: &'static str,
        line: usize,
        pattern: String,
        path: PathBuf,
        /// The adapter's indexing-only ignore files, named in the warning.
        indexing: &'static [&'static str],
    },
    /// The declared file exists but was not read, so nothing is known.
    Unreadable {
        adapter: &'static str,
        file: &'static str,
        error: String,
    },
}

/// Check one checkout-relative directory against every file declared by
/// `adapters` in `checkout`. A missing file hides nothing.
pub fn findings<'a>(
    checkout: &Path,
    path: &Path,
    adapters: impl IntoIterator<Item = &'a AdapterSpec>,
) -> Vec<IgnoreFinding> {
    let mut findings = Vec::new();
    for spec in adapters {
        for &file in spec.ignore_files {
            match read(&checkout.join(file)) {
                Ok(None) => {}
                Ok(Some(contents)) => {
                    if let Some((line, pattern)) = hiding_rule(&contents, path) {
                        findings.push(IgnoreFinding::Hidden {
                            adapter: spec.name,
                            file,
                            line,
                            pattern,
                            path: path.to_path_buf(),
                            indexing: spec.indexing_ignore_files,
                        });
                    }
                }
                Err(error) => findings.push(IgnoreFinding::Unreadable {
                    adapter: spec.name,
                    file,
                    error,
                }),
            }
        }
    }
    findings
}

/// Warnings for a lane of `adapter` running in `worktree` (absolute) of the
/// checkout `checkout`. A worktree outside the checkout is never hidden by it.
pub fn launch_warnings(
    checkout: &Path,
    worktree: &Path,
    adapter: &AdapterSpec,
    host_state: &Path,
) -> Vec<String> {
    if adapter.ignore_files.is_empty() {
        return Vec::new();
    }
    let Ok(relative) = worktree.strip_prefix(checkout) else {
        return Vec::new();
    };
    let found = findings(checkout, relative, [adapter]);
    if found.is_empty() {
        return Vec::new();
    }
    let remedy = layout_remedy(checkout, host_state);
    found
        .iter()
        .map(|finding| warning(finding, &remedy))
        .collect()
}

/// Warnings for every layout root and the default worktree path of
/// `checkout`, for the adapters whose lanes would run there.
pub fn layout_warnings<'a>(
    checkout: &Path,
    host_state: &Path,
    adapters: impl IntoIterator<Item = &'a AdapterSpec> + Clone,
) -> Vec<String> {
    let ports = match PortTable::read_for_inspection(Some(checkout), host_state) {
        Ok(ports) => ports,
        Err(error) => {
            return vec![format!(
                "warning: provider ignore files unchecked: port table unreadable: {error}"
            )];
        }
    };
    let layout = &ports.worktree_layout;
    let mut paths = layout.roots.clone();
    // The rendered default covers its root and any pattern for the worktree
    // directory itself, such as `.worktrees/*`.
    paths.extend(layout.path_for_branch(PROBE_BRANCH).ok());
    let remedy = remedy_for(&ports);
    let mut found: Vec<IgnoreFinding> = Vec::new();
    for path in paths {
        for finding in findings(checkout, &path, adapters.clone()) {
            // One rule hiding a root also hides its default path.
            if !found.iter().any(|seen| same_rule(seen, &finding)) {
                found.push(finding);
            }
        }
    }
    found
        .iter()
        .map(|finding| warning(finding, &remedy))
        .collect()
}

fn same_rule(left: &IgnoreFinding, right: &IgnoreFinding) -> bool {
    match (left, right) {
        (
            IgnoreFinding::Hidden {
                adapter,
                file,
                line,
                ..
            },
            IgnoreFinding::Hidden {
                adapter: other,
                file: other_file,
                line: other_line,
                ..
            },
        ) => adapter == other && file == other_file && line == other_line,
        _ => left == right,
    }
}

/// Render one finding as an operator warning ending with `remedy`.
pub fn warning(finding: &IgnoreFinding, remedy: &str) -> String {
    match finding {
        IgnoreFinding::Hidden {
            adapter,
            file,
            line,
            pattern,
            path,
            indexing,
        } => {
            let mut text = format!(
                "warning: {adapter} lanes cannot read or write worktrees under {}: {file}:{line} `{pattern}` hides it from {adapter}; {remedy}",
                path.display()
            );
            if !indexing.is_empty() {
                text.push_str(&format!(
                    "; {file} controls what {adapter} agents may access, while {} only controls indexing, so moving the pattern there keeps worktrees out of indexing without hiding them from lanes",
                    indexing.join(" or ")
                ));
            }
            text
        }
        IgnoreFinding::Unreadable {
            adapter,
            file,
            error,
        } => format!("warning: {adapter} ignore file {file} unchecked: {error}"),
    }
}

fn layout_remedy(checkout: &Path, host_state: &Path) -> String {
    match PortTable::read_for_inspection(Some(checkout), host_state) {
        Ok(ports) => remedy_for(&ports),
        Err(error) => format!(
            "place lane worktrees under a root the file does not list with worktree_layout.default (port table unreadable: {error})"
        ),
    }
}

/// Name the layout key that places worktrees elsewhere, with its source.
fn remedy_for(ports: &PortTable) -> String {
    let layout = &ports.worktree_layout;
    let source = match layout.source {
        PortSource::Builtin => "compiled default".to_owned(),
        PortSource::Host => "host ports.json in the state directory".to_owned(),
        PortSource::Project => provenant_config::PORTS_CONFIG.to_owned(),
    };
    format!(
        "place lane worktrees under a root the file does not list by setting worktree_layout.default and worktree_layout.roots in {} (now {}, source: {source}); roots outside the checkout are not configurable",
        provenant_config::PORTS_CONFIG,
        layout.default.display(),
    )
}

fn read(path: &Path) -> Result<Option<String>, String> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.is_file() {
        return Err("not a regular file".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(IGNORE_FILE_LIMIT + 1).read_to_end(&mut bytes))
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > IGNORE_FILE_LIMIT {
        return Err(format!("larger than {IGNORE_FILE_LIMIT} bytes"));
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

struct Rule<'a> {
    line: usize,
    text: &'a str,
    negated: bool,
    anchored: bool,
    glob: &'a str,
}

fn rules(contents: &str) -> impl Iterator<Item = Rule<'_>> {
    contents.lines().enumerate().filter_map(|(index, raw)| {
        let text = raw.trim_end();
        if text.is_empty() || text.starts_with('#') {
            return None;
        }
        let (negated, body) = match text.strip_prefix('!') {
            Some(body) => (true, body),
            None => (false, text),
        };
        let body = body.trim_end_matches('/');
        let (leading, glob) = match body.strip_prefix('/') {
            Some(glob) => (true, glob),
            None => (false, body),
        };
        (!glob.is_empty()).then_some(Rule {
            line: index + 1,
            text,
            negated,
            anchored: leading || glob.contains('/'),
            glob,
        })
    })
}

/// The line and text of the rule that hides `path` (checkout-relative, a
/// directory), checking each ancestor first as Git does.
fn hiding_rule(contents: &str, path: &Path) -> Option<(usize, String)> {
    let mut prefix = String::new();
    for component in path.components() {
        let Component::Normal(name) = component else {
            return None;
        };
        let name = name.to_string_lossy();
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(&name);
        let mut decision = None;
        for rule in rules(contents) {
            let subject = if rule.anchored {
                prefix.as_str()
            } else {
                &*name
            };
            if provenant_routing::path_matches(rule.glob, subject) {
                decision = (!rule.negated).then(|| (rule.line, rule.text.to_owned()));
            }
        }
        if decision.is_some() {
            return decision;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_records::ADAPTER_SPECS;

    fn declared_files() -> Vec<&'static str> {
        let mut files: Vec<_> = ADAPTER_SPECS
            .iter()
            .flat_map(|spec| spec.ignore_files.iter().copied())
            .collect();
        files.sort_unstable();
        files.dedup();
        files
    }

    fn checkout(contents: Option<&str>) -> tempfile::TempDir {
        let directory = tempfile::tempdir().expect("checkout");
        if let Some(contents) = contents {
            for file in declared_files() {
                std::fs::write(directory.path().join(file), contents).expect("ignore file");
            }
        }
        directory
    }

    fn hidden(directory: &tempfile::TempDir, spec: &AdapterSpec, path: &str) -> bool {
        findings(directory.path(), Path::new(path), [spec])
            .iter()
            .any(|finding| matches!(finding, IgnoreFinding::Hidden { adapter, .. } if *adapter == spec.name))
    }

    // The invariant comes from the declaration: an adapter warns exactly when
    // one of its declared files lists the worktree root.
    #[test]
    fn declared_ignore_files_warn_only_when_they_list_the_worktree_root() {
        let root = PortTable::builtin().worktree_layout.roots[0].clone();
        let root = root.to_str().expect("root");
        let worktree = format!("{root}/{PROBE_BRANCH}");
        let listing = checkout(Some(format!("# indexing\n{root}/\n").as_str()));
        let unrelated = checkout(Some("target/\n*.log\n"));
        let absent = checkout(None);
        let state = tempfile::tempdir().expect("host state");
        assert!(
            ADAPTER_SPECS
                .iter()
                .any(|spec| !spec.ignore_files.is_empty())
        );
        for spec in ADAPTER_SPECS {
            for file in spec.ignore_files {
                let path = Path::new(file);
                assert!(
                    path.components().count() == 1
                        && matches!(path.components().next(), Some(Component::Normal(_))),
                    "{}: {file} must be a checkout-root file",
                    spec.name
                );
            }
            let declares = !spec.ignore_files.is_empty();
            assert_eq!(hidden(&listing, spec, &worktree), declares, "{}", spec.name);
            assert_eq!(hidden(&listing, spec, root), declares, "{}", spec.name);
            assert!(!hidden(&unrelated, spec, &worktree), "{}", spec.name);
            assert!(!hidden(&absent, spec, &worktree), "{}", spec.name);
            assert!(findings(absent.path(), Path::new(&worktree), [spec]).is_empty());
            // Doctor reads the same declaration through the layout port.
            assert_eq!(
                !layout_warnings(listing.path(), state.path(), [spec]).is_empty(),
                declares,
                "{}",
                spec.name
            );
            assert!(layout_warnings(unrelated.path(), state.path(), [spec]).is_empty());
            // The warning names each declared indexing-only file as the place
            // that keeps worktrees out of indexing without hiding them.
            for warning in layout_warnings(listing.path(), state.path(), [spec]) {
                for file in spec.indexing_ignore_files {
                    assert!(warning.contains(file), "{}: {warning}", spec.name);
                }
            }
            for file in spec.indexing_ignore_files {
                assert!(!spec.ignore_files.contains(file), "{}: {file}", spec.name);
            }
        }
    }

    #[test]
    fn gitignore_forms_decide_whether_a_worktree_is_hidden() {
        let path = Path::new(".worktrees/lane");
        for (contents, expected) in [
            (".worktrees/", true),
            (".worktrees", true),
            ("/.worktrees/", true),
            (".worktrees/*", true),
            (".worktrees/**", true),
            ("**/.worktrees", true),
            (".work*", true),
            ("lane", true),
            ("# .worktrees/", false),
            ("worktrees/", false),
            ("nested/.worktrees/", false),
            (".worktrees/\n!.worktrees/", false),
            (".worktrees/*\n!.worktrees/lane", false),
            // Git cannot re-include below an ignored directory.
            (".worktrees/\n!.worktrees/lane", true),
        ] {
            assert_eq!(
                hiding_rule(contents, path).is_some(),
                expected,
                "{contents:?}"
            );
        }
    }
}
