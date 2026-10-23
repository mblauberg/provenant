//! Bootstrap unit 01: helpers only. Mission creation (`main`) is a later unit.
use crate::policy::strip;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

pub const PROG: &str = "bootstrap-autopilot.py";
pub const REQUIRED_KNOBS: &[&str] = &[
    "DOMAIN",
    "MISSION",
    "LOCKED_CONSTRAINTS",
    "BUILD_CEILING",
    "ESCALATION_GATES",
];
pub const SUBST_TARGETS: &[&str] = &["README.md", "STATE.md", "HANDOFF.md"];
pub const SCAN_TARGETS: &[&str] = &["README.md", "STATE.md", "HANDOFF.md", "QUEUE.md"];
pub const DEFAULT_RUNAWAY_CAPS: &str = "max ~4 concurrent jobs; max ~3 active forks; fork-depth <=2; ~3 runs/unit before escalation; bounded-retry <=2; long-wake ~3600s";

#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error("{0}")]
    Usage(String),
    #[error("'{0}'")]
    UnknownFallback(String),
    #[error("PosixPath('{}') has an empty name", path.display())]
    EmptyName { path: PathBuf },
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: {source}")]
    Encoding {
        path: PathBuf,
        #[source]
        source: std::string::FromUtf8Error,
    },
}
impl BootstrapError {
    pub fn exit_code(&self) -> i32 {
        if matches!(self, Self::Usage(_)) { 2 } else { 1 }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapArgs {
    pub mission_id: String,
    pub repo_root: String,
    pub domain: String,
    pub force: bool,
    pub dry_run: bool,
    pub quiet: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedArgs {
    Help,
    Values(BootstrapArgs),
}

pub fn usage() -> &'static str {
    include_str!("bootstrap_defaults/usage.txt")
}
pub fn err(message: &str) -> String {
    if message.is_empty() {
        format!("{PROG}:\n")
    } else {
        format!("{PROG}: {message}\n")
    }
}
/// Quiet mode suppresses notes, never warnings.
pub fn warn(message: &str, _quiet: bool) -> String {
    format!("{PROG}: WARNING: {message}\n")
}

pub fn parse_args(argv: &[String]) -> Result<ParsedArgs, BootstrapError> {
    let mut values = BootstrapArgs::default();
    let mut positionals = Vec::new();
    let mut index = 0;
    while let Some(arg) = argv.get(index) {
        match arg.as_str() {
            "--" => break, // The Python helper discards the remainder, rather than consuming it.
            "--id" | "--repo-root" | "--domain" => {
                index += 1;
                let value = argv
                    .get(index)
                    .ok_or_else(|| BootstrapError::Usage(format!("missing value for {arg}")))?;
                match arg.as_str() {
                    "--id" => values.mission_id = value.clone(),
                    "--repo-root" => values.repo_root = value.clone(),
                    _ => values.domain = value.clone(),
                }
            }
            "--force" => values.force = true,
            "--dry-run" => values.dry_run = true,
            "-q" | "--quiet" => values.quiet = true,
            "-h" | "--help" => return Ok(ParsedArgs::Help),
            _ if arg.starts_with('-') => {
                return Err(BootstrapError::Usage(format!(
                    "unknown option: {arg} (try --help)"
                )));
            }
            _ => positionals.push(arg.clone()),
        }
        index += 1;
    }
    let mut positionals = positionals.into_iter();
    if values.mission_id.is_empty() {
        values.mission_id = positionals.next().unwrap_or_default();
    }
    if values.domain.is_empty() {
        values.domain = positionals.next().unwrap_or_default();
    }
    if let Some(extra) = positionals.next() {
        return Err(BootstrapError::Usage(format!(
            "unexpected extra argument: {extra} (try --help)"
        )));
    }
    Ok(ParsedArgs::Values(values))
}

fn universal_newlines(s: String) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}
pub fn safe_read(path: &Path) -> Result<Option<String>, BootstrapError> {
    match fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(universal_newlines)
            .map(Some)
            .map_err(|source| BootstrapError::Encoding {
                path: path.into(),
                source,
            }),
        Err(_) => Ok(None),
    }
}
pub fn git_output(args: &[OsString], cwd: Option<&Path>) -> Result<Option<String>, BootstrapError> {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let Ok(output) = command.output() else {
        return Ok(None);
    };
    let stdout = String::from_utf8(output.stdout).map_err(|source| BootstrapError::Encoding {
        path: "git stdout".into(),
        source,
    })?;
    String::from_utf8(output.stderr).map_err(|source| BootstrapError::Encoding {
        path: "git stderr".into(),
        source,
    })?;
    Ok(output
        .status
        .success()
        .then(|| strip(&universal_newlines(stdout)).into()))
}
/// Preserve the original helper's replace and cleanup effects. This helper is
/// not a descriptor-bound custody primitive and must not be used as one.
pub fn atomic_write(path: &Path, content: &str) -> Result<bool, BootstrapError> {
    let name = match path.components().next_back() {
        Some(Component::Normal(name)) => name,
        Some(Component::ParentDir) => std::ffi::OsStr::new(".."),
        _ => {
            return Err(BootstrapError::EmptyName {
                path: if path.as_os_str().is_empty() {
                    ".".into()
                } else {
                    path.into()
                },
            });
        }
    };
    let mut filename = name.to_os_string();
    filename.push(format!(".tmp.{}", std::process::id()));
    let temporary = path
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(filename);
    if fs::write(&temporary, content)
        .and_then(|()| fs::rename(&temporary, path))
        .is_ok()
    {
        return Ok(true);
    }
    match fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(BootstrapError::Io {
                path: temporary,
                source,
            });
        }
    }
    Ok(false)
}
pub fn fallback_content(name: &str, domain: &str) -> Result<String, BootstrapError> {
    let content = match name {
        "GOAL.md" => {
            return Ok(include_str!("bootstrap_defaults/GOAL.md").replacen(
                "{{DOMAIN}}",
                if domain.is_empty() {
                    "{{DOMAIN}}"
                } else {
                    domain
                },
                1,
            ));
        }
        "STATE.md" => include_str!("bootstrap_defaults/STATE.md"),
        "QUEUE.md" => include_str!("bootstrap_defaults/QUEUE.md"),
        "HANDOFF.md" => include_str!("bootstrap_defaults/HANDOFF.md"),
        "README.md" => include_str!("bootstrap_defaults/README.md"),
        _ => return Err(BootstrapError::UnknownFallback(name.into())),
    };
    Ok(content.into())
}
/// Keep the source dictionary's first-insertion order when a knob is replaced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigKnobs(Vec<(String, String)>);
impl ConfigKnobs {
    fn insert(&mut self, key: String, value: String) {
        if let Some((_, current)) = self.0.iter_mut().find(|(name, _)| name == &key) {
            *current = value;
        } else {
            self.0.push((key, value));
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.iter()
            .find_map(|(name, value)| (name == key).then_some(value))
    }
}
impl Serialize for ConfigKnobs {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in self.iter() {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}
impl<'de> Deserialize<'de> for ConfigKnobs {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = ConfigKnobs;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("an ordered string-to-string knob object")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                let mut result = ConfigKnobs::default();
                while let Some((key, value)) = map.next_entry()? {
                    result.insert(key, value);
                }
                Ok(result)
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

pub fn parse_knobs(goal: &str) -> ConfigKnobs {
    let mut result = ConfigKnobs::default();
    let mut in_block = false;
    for line in goal.split([
        '\n', '\r', '\u{b}', '\u{c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}',
        '\u{2029}',
    ]) {
        if line.trim_end_matches(crate::policy::whitespace) == "```config-knobs" {
            in_block = true;
            continue;
        }
        if line.trim_end_matches(crate::policy::whitespace) == "```" {
            in_block = false;
            continue;
        }
        if !in_block {
            continue;
        }
        let line = line.trim_start_matches(crate::policy::whitespace);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if !strip(key).is_empty() {
                result.insert(strip(key).into(), strip(value).into());
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn argument_help_default_and_knob_differentials() {
        let mut cases = vec![json!({"op":"usage"})];
        let mut actual = vec![json!(usage())];
        for args in [
            vec![],
            vec!["mission", "domain"],
            vec!["--id", "mission", "domain"],
            vec!["--id", "mission", "--domain", "explicit", "extra"],
            vec!["--id", ""],
            vec!["mission", "--", "ignored"],
            vec!["--repo-root"],
            vec!["--id", "--force"],
            vec!["--id", "a", "--id", "b", "-q", "--dry-run", "--force"],
            vec!["--help"],
            vec!["--unknown"],
            vec!["a", "b", "c"],
            vec!["--domain", "domain", "mission"],
            vec!["--domain", "domain", "a", "b"],
            vec!["--", "ignored"],
            vec!["--id", "a", "--domain", "", "b"],
        ] {
            cases.push(json!({"op":"args","args":args}));
            actual.push(
                match parse_args(&args.iter().map(|s| (*s).into()).collect::<Vec<_>>()) {
                    Ok(ParsedArgs::Values(values)) => json!({"values":values,"rest":[]}),
                    Ok(ParsedArgs::Help) => json!({"exit":0,"stdout":usage(),"stderr":""}),
                    Err(e) => {
                        json!({"exit":e.exit_code(),"stdout":"","stderr":err(&e.to_string())})
                    }
                },
            );
        }
        for name in ["GOAL.md", "STATE.md", "QUEUE.md", "HANDOFF.md", "README.md"] {
            for domain in ["", "Example & tools | scripts\\paths", "领域 {{MISSION}}"] {
                cases.push(json!({"op":"fallback","name":name,"domain":domain}));
                actual.push(json!(fallback_content(name, domain).unwrap()));
            }
        }
        for goal in [
            "",
            "outside = ignore\n```config-knobs\n A = one=two\n#comment=ignored\nA = final\n = empty\n```\na=outside",
            " ```config-knobs\nA=no\n```",
            "```config-knobs \r\nA = yes\rB = also\u{85}C = line\n```",
            "```config-knobs\nA=1\n```other\nB=2\n```\nC=no\n```config-knobs\nD=4",
        ] {
            cases.push(json!({"op":"knobs","goal":goal}));
            actual.push(json!(parse_knobs(goal)));
        }
        if let Some(expected) = crate::test_support::oracle("bootstrap", &json!(cases)) {
            assert_eq!(json!(actual), expected);
        }
    }
    #[test]
    fn file_helpers_replace_cleanup_and_preserve_read_errors() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state");
        assert_eq!(safe_read(&path).unwrap(), None);
        assert!(atomic_write(&path, "first").unwrap());
        assert!(atomic_write(&path, "new\r\nline\rother").unwrap());
        assert_eq!(safe_read(&path).unwrap().unwrap(), "new\nline\nother");
        fs::write(&path, [0xff]).unwrap();
        assert!(matches!(
            safe_read(&path),
            Err(BootstrapError::Encoding { .. })
        ));
        assert!(!atomic_write(&root.path().join("missing/state"), "x").unwrap());
        let directory = root.path().join("directory");
        fs::create_dir(&directory).unwrap();
        assert!(!atomic_write(&directory, "x").unwrap());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
        assert_eq!(warn("check", true), warn("check", false));
        assert_eq!(err(""), "bootstrap-autopilot.py:\n");
        assert!(matches!(
            atomic_write(Path::new("/"), "refuse"),
            Err(BootstrapError::EmptyName { .. })
        ));
        let knobs = parse_knobs("```config-knobs\nZ=first\nA=next\nZ=last\n```");
        assert_eq!(
            knobs.iter().collect::<Vec<_>>(),
            vec![("Z", "last"), ("A", "next")]
        );
        assert_eq!(knobs.get("Z"), Some("last"));
        assert_eq!(
            serde_json::to_string(&knobs).unwrap(),
            "{\"Z\":\"last\",\"A\":\"next\"}"
        );
        assert_eq!(
            serde_json::from_str::<ConfigKnobs>("{\"Z\":\"last\",\"A\":\"next\"}").unwrap(),
            knobs
        );
    }
    #[test]
    fn git_queries_report_failure_without_mutating_repository() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            git_output(
                &["workflow-port-no-such-git-verb".into()],
                Some(root.path())
            )
            .unwrap(),
            None
        );
        let result = git_output(&["--version".into()], None).unwrap();
        if let Some(result) = result {
            assert!(result.starts_with("git version "));
        }
    }
}
