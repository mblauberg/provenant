//! Deterministic checks of tracked skill, documentation and harness assets.
use super::{CheckError, GitContext, Scan, WorkingFile, index_records, read_file};
use provenant_confinement::GitRead;
use provenant_records::{self as yaml, SpannedYaml, YamlValue};
use regex::Regex;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    os::unix::ffi::OsStringExt,
    path::{Component, Path, PathBuf},
};

const FILE_LIMIT: u64 = 5 * 1024 * 1024;
const TOTAL_LIMIT: u64 = 64 * 1024 * 1024;
const ISSUE_FORM_TOP_LEVEL_KEYS: &[&str] = &[
    "assignees",
    "body",
    "description",
    "labels",
    "name",
    "projects",
    "title",
    "type",
];
const ISSUE_FORM_BODY_KEYS: &[&str] = &["attributes", "id", "type", "validations"];

fn read_harness_file(
    root: &dyn provenant_supervisor::fs::SafeDir,
    path: &Path,
    limit: u64,
) -> Result<Vec<u8>, CheckError> {
    match read_file(root, path, limit)? {
        WorkingFile::Bytes(bytes) => Ok(bytes),
        WorkingFile::Symlink(_) | WorkingFile::Missing => {
            Err(CheckError::Evidence("tracked input is not a regular file"))
        }
    }
}

pub(super) fn inspect(git: &GitContext<'_>) -> Result<Scan, CheckError> {
    let index = git.read(GitRead::IndexEntries { paths: Vec::new() })?;
    let mut scan = Scan::default();
    scan.note("tracked-index", &index);
    let mut paths = BTreeSet::new();
    for record in index_records(&index)? {
        let stage = std::str::from_utf8(record.stage)
            .map_err(|_| CheckError::Evidence("index header encoding"))?;
        if stage != "0" {
            return Err(CheckError::Evidence("unmerged Git index"));
        }
        let path = PathBuf::from(OsString::from_vec(record.path.to_vec()));
        super::wire(&path)?;
        paths.insert(path);
    }
    if paths.len() > 10_000 {
        return Err(CheckError::Evidence("tracked file count bound"));
    }
    if !index.is_empty() && !index.ends_with(&[0]) {
        return Err(CheckError::Evidence("unterminated Git index"));
    }
    let mut files = BTreeMap::new();
    let mut total = 0u64;
    for path in &paths {
        if !relevant(path) {
            continue;
        }
        let bytes = read_harness_file(git.repo.root_dir(), path, FILE_LIMIT)?;
        total = total.saturating_add(bytes.len() as u64);
        if total > TOTAL_LIMIT {
            return Err(CheckError::Evidence("harness byte bound"));
        }
        scan.note(
            path.as_os_str()
                .as_encoded_bytes()
                .escape_ascii()
                .to_string(),
            &bytes,
        );
        let text = String::from_utf8(bytes)
            .map_err(|_| CheckError::Evidence("harness input is not UTF-8"))?;
        files.insert(path.clone(), text);
    }
    let mut anchor_cache = BTreeMap::new();
    let skills: BTreeSet<_> = files
        .keys()
        .filter(|path| path.file_name().is_some_and(|name| name == "SKILL.md"))
        .filter_map(|path| path.parent()?.file_name()?.to_str().map(str::to_owned))
        .collect();
    for (path, text) in &files {
        if path.file_name().is_some_and(|name| name == "SKILL.md") {
            skill(&mut scan, path, text)?;
        } else if path.ends_with("agents/openai.yaml") {
            sidecar(&mut scan, path, text, &paths, git)?;
        } else if issue_form(path) {
            issue(&mut scan, path, text)?;
        } else if route_fixture(path) {
            routes(&mut scan, path, text, &skills)?;
        }
        if path.extension().is_some_and(|extension| extension == "md") {
            markdown(
                &mut scan,
                path,
                text,
                &files,
                &paths,
                git,
                &mut anchor_cache,
            )?;
        }
    }
    Ok(scan)
}
fn issue_form(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    path.parent()
        .is_some_and(|parent| parent.ends_with("ISSUE_TEMPLATE"))
        && matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("yml" | "yaml")
        )
        && !matches!(name, "config.yml" | "config.yaml")
}
fn route_fixture(path: &Path) -> bool {
    let is_fixture = matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("trigger_cases.json" | "regression_cases.json")
    );
    let Some(evals) = path.parent() else {
        return false;
    };
    let Some(skill) = evals.parent() else {
        return false;
    };
    is_fixture
        && evals.file_name().is_some_and(|name| name == "evals")
        && matches!(
            skill.parent().and_then(Path::file_name),
            Some(name) if name == "skills" || name == "library"
        )
}
fn relevant(path: &Path) -> bool {
    path.extension().is_some_and(|s| s == "md")
        || path.ends_with("agents/openai.yaml")
        || issue_form(path)
        || route_fixture(path)
}
fn map(value: &SpannedYaml) -> Option<&BTreeMap<String, SpannedYaml>> {
    if let YamlValue::Mapping(map) = &value.value {
        Some(map)
    } else {
        None
    }
}
fn string(value: Option<&SpannedYaml>) -> Option<&str> {
    match value.map(|v| &v.value) {
        Some(YamlValue::String(text)) if !text.trim().is_empty() => Some(text),
        _ => None,
    }
}
fn sequence(value: Option<&SpannedYaml>) -> Option<&[SpannedYaml]> {
    match value.map(|v| &v.value) {
        Some(YamlValue::Sequence(items)) => Some(items),
        _ => None,
    }
}
fn yaml_input(
    scan: &mut Scan,
    path: &Path,
    parsed: Result<SpannedYaml, yaml::YamlError>,
) -> Result<Option<SpannedYaml>, CheckError> {
    match parsed {
        Ok(value) => Ok(Some(value)),
        Err(error) => {
            scan.finding(
                "external_yaml",
                path,
                error.line,
                &format!("{}", error.kind),
            )?;
            Ok(None)
        }
    }
}
fn skill(scan: &mut Scan, path: &Path, text: &str) -> Result<(), CheckError> {
    let Some(parsed) = yaml_input(scan, path, yaml::parse_frontmatter(text))? else {
        return Ok(());
    };
    let Some(fields) = map(&parsed) else {
        return scan.finding(
            "skill_frontmatter",
            path,
            1,
            "skill frontmatter must be a mapping",
        );
    };
    let expected = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|s| s.to_str());
    if string(fields.get("name")) != expected
        || fields
            .keys()
            .any(|key| !matches!(key.as_str(), "name" | "description"))
    {
        scan.finding(
            "skill_frontmatter",
            path,
            2,
            "frontmatter needs its directory name and description only",
        )?;
    }
    if string(fields.get("description")).is_none_or(|s| s.chars().count() > 1024) {
        scan.finding(
            "skill_description",
            path,
            2,
            "description must contain 1..1024 characters",
        )?;
    }
    Ok(())
}
fn sidecar(
    scan: &mut Scan,
    path: &Path,
    text: &str,
    paths: &BTreeSet<PathBuf>,
    git: &GitContext<'_>,
) -> Result<(), CheckError> {
    let Some(parsed) = yaml_input(scan, path, yaml::parse_sidecar(text))? else {
        return Ok(());
    };
    let Some(interface) = map(&parsed).and_then(|m| m.get("interface")).and_then(map) else {
        return scan.finding(
            "sidecar_interface",
            path,
            1,
            "interface mapping is required",
        );
    };
    for field in ["display_name", "short_description", "default_prompt"] {
        if string(interface.get(field)).is_none() {
            scan.finding(
                "sidecar_interface",
                path,
                1,
                &format!("interface.{field} is required"),
            )?;
        }
    }
    if string(interface.get("short_description"))
        .is_some_and(|text| !(25..=64).contains(&text.chars().count()))
    {
        scan.finding(
            "sidecar_description",
            path,
            1,
            "short description must contain 25..64 characters",
        )?;
    }
    let skill = path
        .parent()
        .and_then(Path::parent)
        .ok_or(CheckError::Evidence("sidecar path"))?;
    if let Some(name) = skill.file_name().and_then(|s| s.to_str())
        && string(interface.get("default_prompt"))
            .is_some_and(|text| !text.contains(&format!("${name}")))
    {
        scan.finding(
            "sidecar_prompt",
            path,
            1,
            "default prompt must invoke the skill",
        )?;
    }
    for field in ["icon_small", "icon_large"] {
        if let Some(value) = string(interface.get(field)) {
            let resolved = resolve(skill, value);
            if let Some(asset) = resolved.filter(|asset| paths.contains(asset)) {
                let bytes = read_harness_file(git.repo.root_dir(), &asset, FILE_LIMIT)?;
                scan.note(
                    asset
                        .as_os_str()
                        .as_encoded_bytes()
                        .escape_ascii()
                        .to_string(),
                    &bytes,
                );
            } else {
                scan.finding(
                    "sidecar_asset",
                    path,
                    1,
                    "icon must refer to a tracked local asset",
                )?;
            }
        }
    }
    Ok(())
}
fn issue(scan: &mut Scan, path: &Path, text: &str) -> Result<(), CheckError> {
    let Some(parsed) = yaml_input(scan, path, yaml::parse_sidecar(text))? else {
        return Ok(());
    };
    let Some(fields) = map(&parsed) else {
        return scan.finding("issue_form", path, 1, "issue form must be a mapping");
    };
    let unsupported: Vec<_> = fields
        .keys()
        .filter(|key| !ISSUE_FORM_TOP_LEVEL_KEYS.contains(&key.as_str()))
        .cloned()
        .collect();
    if !unsupported.is_empty() {
        scan.finding(
            "issue_form",
            path,
            1,
            &format!("unsupported top-level keys: {}", unsupported.join(", ")),
        )?;
    }
    if ["name", "description"]
        .iter()
        .any(|field| string(fields.get(*field)).is_none())
    {
        scan.finding("issue_form", path, 1, "name and description are required")?;
    }
    let Some(items) = sequence(fields.get("body")).filter(|items| !items.is_empty()) else {
        return scan.finding("issue_body", path, 1, "body must be a nonempty sequence");
    };
    let mut ids = BTreeSet::new();
    let mut inputs = 0u64;
    for (index, item) in items.iter().enumerate() {
        let Some(fields) = map(item) else {
            scan.finding(
                "issue_body",
                path,
                item.span.line,
                "body item must be a mapping",
            )?;
            continue;
        };
        let kind = string(fields.get("type")).unwrap_or("");
        if [
            "checkboxes",
            "dropdown",
            "input",
            "markdown",
            "textarea",
            "upload",
        ]
        .contains(&kind)
        {
            let unsupported: Vec<_> = fields
                .keys()
                .filter(|key| !ISSUE_FORM_BODY_KEYS.contains(&key.as_str()))
                .cloned()
                .collect();
            if !unsupported.is_empty() {
                scan.finding(
                    "issue_body",
                    path,
                    item.span.line,
                    &format!(
                        "body item {} has unsupported keys: {}",
                        index + 1,
                        unsupported.join(", ")
                    ),
                )?;
            }
        }
        if ![
            "checkboxes",
            "dropdown",
            "input",
            "markdown",
            "textarea",
            "upload",
        ]
        .contains(&kind)
        {
            scan.finding(
                "issue_type",
                path,
                item.span.line,
                "unsupported issue field type",
            )?;
        }
        let attributes = fields.get("attributes").and_then(map);
        if kind == "markdown" {
            if fields.contains_key("id")
                || attributes.is_none_or(|m| string(m.get("value")).is_none())
            {
                scan.finding(
                    "issue_markdown",
                    path,
                    item.span.line,
                    "markdown needs value and no id",
                )?;
            }
        } else {
            inputs += 1;
            let id = string(fields.get("id")).unwrap_or("");
            if id.is_empty()
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                || !ids.insert(id)
            {
                scan.finding(
                    "issue_id",
                    path,
                    item.span.line,
                    "input ids must be nonempty, unique letters, digits, underscore or hyphen",
                )?;
            }
            if attributes.is_none_or(|m| string(m.get("label")).is_none()) {
                scan.finding(
                    "issue_label",
                    path,
                    item.span.line,
                    "input label is required",
                )?;
            }
        }
        if let Some(validations) = fields.get("validations")
            && map(validations).is_none_or(|m| {
                m.get("required")
                    .is_some_and(|v| !matches!(v.value, YamlValue::Boolean(_)))
            })
        {
            scan.finding(
                "issue_validation",
                path,
                item.span.line,
                "validations requires a mapping with boolean required",
            )?;
        }
        if matches!(kind, "checkboxes" | "dropdown") {
            let options = attributes.and_then(|m| sequence(m.get("options")));
            let mut labels = BTreeSet::new();
            if options.is_none_or(|items| {
                items.is_empty()
                    || items.iter().any(|option| {
                        let label = if kind == "dropdown" {
                            string(Some(option))
                        } else {
                            map(option).and_then(|m| string(m.get("label")))
                        };
                        label.is_none_or(|label| !labels.insert(label))
                    })
            }) {
                scan.finding(
                    "issue_options",
                    path,
                    item.span.line,
                    "options need unique, nonempty labels",
                )?;
            }
        }
    }
    if inputs == 0 {
        scan.finding("issue_body", path, 1, "at least one input is required")?;
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Routes {
    schema_version: u64,
    target_skill: String,
    cases: Vec<Route>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Route {
    id: String,
    relation: Relation,
    prompt: String,
    tags: Vec<String>,
    expected: Expected,
    #[serde(default)]
    note: Present<Option<String>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expected {
    #[serde(deserialize_with = "Present::deserialize")]
    primary_skill: Present<Option<String>>,
    companion_skills: Vec<String>,
}
#[derive(Default)]
struct Present<T>(Option<T>);
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Present<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        T::deserialize(deserializer).map(|value| Self(Some(value)))
    }
}
#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Relation {
    Positive,
    Negative,
    Boundary,
}
fn routes(
    scan: &mut Scan,
    path: &Path,
    text: &str,
    skills: &BTreeSet<String>,
) -> Result<(), CheckError> {
    let route: Routes =
        match provenant_records::decode(text.as_bytes(), provenant_records::Limits::CONFIG) {
            Ok(route) => route,
            Err(_) => {
                return scan.finding(
                    "route_fixture",
                    path,
                    1,
                    "route fixture must be strict JSON with typed fields",
                );
            }
        };
    let skill = path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let regression = path
        .file_name()
        .is_some_and(|name| name == "regression_cases.json");
    // The q/r namespaces make valid IDs disjoint across each fixture pair.
    let id_prefix = if regression { "r" } else { "q" };
    let mut ids = BTreeSet::new();
    let mut valid =
        route.schema_version == 1 && route.target_skill == skill && !route.cases.is_empty();
    for case in route.cases {
        let companions: BTreeSet<_> = case.expected.companion_skills.iter().collect();
        let tags: BTreeSet<_> = case.tags.iter().collect();
        let primary = case
            .expected
            .primary_skill
            .0
            .as_ref()
            .and_then(Option::as_deref);
        let note_valid = if regression {
            matches!(case.note.0.as_ref(), Some(Some(note)) if !note.trim().is_empty())
        } else {
            case.note.0.is_none()
        };
        let id_digits = case.id.strip_prefix(id_prefix).unwrap_or("");
        valid &= !id_digits.is_empty()
            && id_digits.bytes().all(|byte| byte.is_ascii_digit())
            && ids.insert(case.id)
            && !case.prompt.trim().is_empty()
            && !case.tags.is_empty()
            && tags.len() == case.tags.len()
            && case.tags.iter().all(|tag| !tag.is_empty())
            && primary.is_none_or(|name| skills.contains(name))
            && companions.len() == case.expected.companion_skills.len()
            && companions
                .iter()
                .all(|name| skills.contains(*name) && Some(name.as_str()) != primary)
            && (primary.is_some()
                || (companions.is_empty() && case.tags.iter().any(|tag| tag == "no-skill")))
            && match case.relation {
                Relation::Positive => primary == Some(skill),
                Relation::Negative => {
                    primary != Some(skill) && !companions.iter().any(|name| name.as_str() == skill)
                }
                Relation::Boundary => {
                    case.tags
                        .iter()
                        .any(|tag| matches!(tag.as_str(), "adjacent" | "composition"))
                        && (!regression
                            || primary == Some(skill)
                            || companions.iter().any(|name| name.as_str() == skill))
                }
            }
            && note_valid;
    }
    if !valid {
        scan.finding(
            "route_fixture",
            path,
            1,
            "invalid fixture version, target, case identity or expected route",
        )?;
    }
    Ok(())
}
fn resolve(parent: &Path, target: &str) -> Option<PathBuf> {
    let mut path = PathBuf::new();
    for part in parent.join(target).components() {
        match part {
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !path.pop() {
                    return None;
                }
            }
            _ => return None,
        }
    }
    (!path.as_os_str().is_empty()).then_some(path)
}
fn regex(pattern: &str) -> Result<Regex, CheckError> {
    Regex::new(pattern).map_err(|_| CheckError::Evidence("invalid compiled Markdown rule"))
}
fn strip_heading_markup(text: &str) -> String {
    let characters: Vec<_> = text.chars().collect();
    let mut removed = vec![false; characters.len()];
    let mut index = 0;
    while index < characters.len() {
        if characters[index] == '<'
            && let Some(relative_end) = characters[index..]
                .iter()
                .position(|character| *character == '>')
        {
            let end = index + relative_end;
            removed[index..=end].fill(true);
            index = end + 1;
            continue;
        }
        let marker = characters[index];
        if !matches!(marker, '_' | '*') {
            index += 1;
            continue;
        }
        let run = characters[index..]
            .iter()
            .take_while(|character| **character == marker)
            .count();
        let after_open = index + run;
        let before = index
            .checked_sub(1)
            .and_then(|position| characters.get(position));
        let can_open = run <= 3
            && after_open < characters.len()
            && !characters[after_open].is_whitespace()
            && (before.is_none_or(|character| character.is_whitespace())
                || (marker == '*' && before.is_some_and(|character| !character.is_alphanumeric())));
        if can_open {
            let mut close = after_open + 1;
            while close < characters.len() {
                if characters[close] != marker {
                    close += 1;
                    continue;
                }
                let close_run = characters[close..]
                    .iter()
                    .take_while(|character| **character == marker)
                    .count();
                let before_close = characters.get(close.saturating_sub(1));
                let after_close = characters.get(close + close_run);
                if close_run == run
                    && before_close.is_some_and(|character| !character.is_whitespace())
                    && after_close.is_none_or(|character| {
                        character.is_whitespace() || !character.is_alphanumeric()
                    })
                {
                    removed[index..after_open].fill(true);
                    removed[close..close + close_run].fill(true);
                    index = close + close_run;
                    break;
                }
                close += close_run.max(1);
            }
            if index >= after_open {
                continue;
            }
        }
        index += run;
    }
    characters
        .into_iter()
        .enumerate()
        .filter_map(|(index, character)| (!removed[index]).then_some(character))
        .collect()
}
fn anchors(text: &str) -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    let mut fence = None;
    for (_, line) in prose(text, &mut fence) {
        let trimmed = line.trim_start_matches('#');
        if trimmed.len() == line.len() || !trimmed.starts_with(' ') {
            continue;
        }
        let rendered = strip_heading_markup(trimmed.trim().trim_end_matches('#').trim());
        let base: String = rendered
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | ' '))
            .map(|c| if c == ' ' { '-' } else { c })
            .collect();
        let mut anchor = base.clone();
        let mut suffix = 1u64;
        while result.contains(&anchor) {
            anchor = format!("{base}-{suffix}");
            suffix += 1;
        }
        result.insert(anchor);
    }
    result
}
fn prose<'a>(
    text: &'a str,
    fence: &'a mut Option<(char, usize)>,
) -> impl Iterator<Item = (usize, &'a str)> {
    text.lines().enumerate().filter_map(move |(index, line)| {
        let start = line.trim_start();
        let marker = start.chars().next().unwrap_or(' ');
        let length = start.chars().take_while(|c| *c == marker).count();
        if matches!(marker, '`' | '~') && length >= 3 {
            match *fence {
                None => *fence = Some((marker, length)),
                Some((m, l)) if m == marker && length >= l => *fence = None,
                _ => {}
            }
            return None;
        }
        fence.is_none().then_some((index + 1, line))
    })
}
// CommonMark backtick spans close only on a run of the same length. A run
// without a closer is literal text, so links following it still get checked.
fn code_spans(text: &str) -> Vec<std::ops::Range<usize>> {
    let bytes = text.as_bytes();
    let mut counts = BTreeMap::<usize, usize>::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes.get(index) != Some(&b'`') {
            index += 1;
            continue;
        }
        let length = bytes
            .get(index..)
            .unwrap_or(&[])
            .iter()
            .take_while(|b| **b == b'`')
            .count();
        *counts.entry(length).or_default() += 1;
        index += length;
    }
    let mut spans = Vec::new();
    index = 0;
    while index < bytes.len() {
        if bytes.get(index) != Some(&b'`') {
            index += 1;
            continue;
        }
        let start = index;
        let slashes = bytes
            .get(..start)
            .unwrap_or(&[])
            .iter()
            .rev()
            .take_while(|b| **b == b'\\')
            .count();
        let length = bytes
            .get(index..)
            .unwrap_or(&[])
            .iter()
            .take_while(|b| **b == b'`')
            .count();
        index += length;
        let remaining = counts.entry(length).or_default();
        *remaining = remaining.saturating_sub(1);
        // Knowing that a closer exists avoids rescanning the whole suffix for
        // each unmatched run in hostile input: both passes are linear.
        if slashes % 2 == 1 || *remaining == 0 {
            continue;
        }
        let mut end = index;
        while end < bytes.len() {
            if bytes.get(end) != Some(&b'`') {
                end += 1;
                continue;
            }
            let closing = bytes
                .get(end..)
                .unwrap_or(&[])
                .iter()
                .take_while(|b| **b == b'`')
                .count();
            let count = counts.entry(closing).or_default();
            *count = count.saturating_sub(1);
            end += closing;
            if closing == length {
                spans.push(start..end);
                index = end;
                break;
            }
        }
    }
    spans
}
fn markdown_blocks(text: &str) -> Vec<(usize, String)> {
    let mut blocks = Vec::new();
    let mut block = String::new();
    let mut start = 1usize;
    let mut previous = 0usize;
    let mut fence = None;
    for (line, text) in prose(text, &mut fence) {
        let trimmed = text.trim_start_matches(' ');
        let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
        let heading = text.len() - trimmed.len() <= 3
            && (1..=6).contains(&hashes)
            && trimmed
                .as_bytes()
                .get(hashes)
                .is_none_or(u8::is_ascii_whitespace);
        if (line != previous + 1 || text.trim().is_empty() || heading) && !block.is_empty() {
            blocks.push((start, std::mem::take(&mut block)));
        }
        if !text.trim().is_empty() {
            if block.is_empty() {
                start = line;
            }
            block.push_str(text);
            block.push('\n');
        }
        if heading && !block.is_empty() {
            blocks.push((start, std::mem::take(&mut block)));
        }
        previous = line;
    }
    if !block.is_empty() {
        blocks.push((start, block));
    }
    blocks
}
fn markdown(
    scan: &mut Scan,
    path: &Path,
    text: &str,
    files: &BTreeMap<PathBuf, String>,
    paths: &BTreeSet<PathBuf>,
    git: &GitContext<'_>,
    anchor_cache: &mut BTreeMap<PathBuf, BTreeSet<String>>,
) -> Result<(), CheckError> {
    let links = regex(r"\[[^\]]*\]\(([^\s)]+)(?:\s+[^)]*)?\)")?;
    for (first_line, text) in markdown_blocks(text) {
        let code = code_spans(&text);
        let mut spans = code.iter().peekable();
        let mut counted = 0;
        let mut line = first_line;
        for link in links.captures_iter(&text) {
            let start = link.get(0).map_or(counted, |matched| matched.start());
            line += text
                .get(counted..start)
                .unwrap_or("")
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count();
            counted = start;
            while spans.peek().is_some_and(|span| span.end <= start) {
                spans.next();
            }
            if spans.peek().is_some_and(|span| span.contains(&start)) {
                continue;
            }
            let Some(target) = link.get(1).map(|s| s.as_str().trim_matches(['<', '>'])) else {
                continue;
            };
            if target.contains(':') || target.starts_with('/') {
                continue;
            }
            let (relative, anchor) = target
                .split_once('#')
                .map_or((target, None), |(p, a)| (p, Some(a)));
            let destination = if relative.is_empty() {
                Some(path.into())
            } else {
                resolve(path.parent().unwrap_or(Path::new("")), relative)
            };
            let Some(destination) = destination.filter(|p| {
                paths.contains(p) || paths.iter().any(|tracked| tracked.starts_with(p))
            }) else {
                scan.finding(
                    "markdown_link",
                    path,
                    line,
                    "local link must resolve to a tracked path inside the project",
                )?;
                continue;
            };
            if !paths.contains(&destination) {
                let mut directory = git.repo.root_dir().open_directory(
                    std::ffi::OsStr::new(
                        destination
                            .iter()
                            .next()
                            .ok_or(CheckError::Evidence("link directory"))?,
                    ),
                    false,
                )?;
                for name in destination.iter().skip(1) {
                    directory = directory.open_directory(name, false)?;
                }
                continue;
            }
            if !files.contains_key(&destination) {
                // Validate binary/local assets too; a link never authorises a
                // filesystem escape or an untracked dependency.
                let bytes = read_harness_file(git.repo.root_dir(), &destination, FILE_LIMIT)?;
                scan.note(
                    destination
                        .as_os_str()
                        .as_encoded_bytes()
                        .escape_ascii()
                        .to_string(),
                    &bytes,
                );
            }
            if let Some(anchor) = anchor.filter(|s| !s.is_empty())
                && destination.extension().is_some_and(|s| s == "md")
                && !files.get(&destination).is_some_and(|document| {
                    anchor_cache
                        .entry(destination.clone())
                        .or_insert_with(|| anchors(document))
                        .contains(anchor)
                })
            {
                scan.finding(
                    "markdown_anchor",
                    path,
                    line,
                    "Markdown fragment must resolve to an existing heading",
                )?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_records::Finding;
    use serde_json::{Value, json};

    fn finding(path: &Path, rule: &str, line: usize, message: &str) -> Finding {
        Finding {
            rule: rule.into(),
            path: super::super::wire(path).unwrap(),
            line,
            message: message.into(),
        }
    }

    fn route_case(
        id: &str,
        relation: &str,
        primary: Value,
        companions: &[&str],
        tags: &[&str],
    ) -> Value {
        json!({
            "id": id,
            "relation": relation,
            "prompt": "Help with this task",
            "tags": tags,
            "expected": {
                "primary_skill": primary,
                "companion_skills": companions
            }
        })
    }

    fn route_fixture(cases: Vec<Value>) -> Value {
        json!({"schema_version": 1, "target_skill": "rust", "cases": cases})
    }

    fn assert_route_finding(filename: &str, fixture: Value) {
        assert_route_finding_message(
            filename,
            fixture,
            "invalid fixture version, target, case identity or expected route",
        );
    }

    fn assert_route_finding_message(filename: &str, fixture: Value, message: &str) {
        let path = PathBuf::from("skills/rust/evals").join(filename);
        let text = serde_json::to_string(&fixture).unwrap();
        let skills = BTreeSet::from(["rust".to_owned(), "web".to_owned()]);
        let mut scan = Scan::default();
        routes(&mut scan, &path, &text, &skills).unwrap();
        assert_eq!(
            scan.findings,
            vec![finding(&path, "route_fixture", 1, message)]
        );
    }

    fn valid_trigger() -> Value {
        route_fixture(vec![route_case(
            "q1",
            "positive",
            json!("rust"),
            &[],
            &["routing"],
        )])
    }

    fn valid_regression_case() -> Value {
        let mut case = route_case("r1", "positive", json!("rust"), &[], &["routing"]);
        case["note"] = json!("Regression coverage");
        case
    }

    #[test]
    fn tracked_route_fixtures_pass_the_route_contract() -> Result<(), Box<dyn std::error::Error>> {
        let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for _ in 0..4 {
            root.pop();
        }
        let mut valid_skills = BTreeSet::new();
        let mut skill_dirs = Vec::new();
        for base in ["skills", "library"] {
            for entry in std::fs::read_dir(root.join(base))? {
                let path = entry?.path();
                if path.join("SKILL.md").is_file() {
                    valid_skills.insert(path.file_name().unwrap().to_string_lossy().into_owned());
                    skill_dirs.push(path);
                }
            }
        }
        let mut fixture_count = 0;
        for skill_dir in skill_dirs {
            let trigger = skill_dir.join("evals/trigger_cases.json");
            assert!(trigger.is_file(), "missing canonical fixture: {trigger:?}");
            let regression = skill_dir.join("evals/regression_cases.json");
            for path in [Some(trigger), regression.is_file().then_some(regression)]
                .into_iter()
                .flatten()
            {
                fixture_count += 1;
                let text = std::fs::read_to_string(&path)?;
                let relative = path.strip_prefix(&root)?;
                let mut scan = Scan::default();
                routes(&mut scan, relative, &text, &valid_skills)?;
                assert!(
                    scan.findings.is_empty(),
                    "{relative:?}: {:?}",
                    scan.findings
                );
            }
        }
        assert!(fixture_count > 0, "expected tracked route fixtures");
        Ok(())
    }

    #[test]
    fn trigger_ids_require_q_followed_by_digits() {
        let mut fixture = valid_trigger();
        fixture["cases"][0]["id"] = json!("case1");
        assert_route_finding("trigger_cases.json", fixture);
    }

    #[test]
    fn regression_ids_require_r_followed_by_digits() {
        let mut fixture = route_fixture(vec![valid_regression_case()]);
        fixture["cases"][0]["id"] = json!("q1");
        assert_route_finding("regression_cases.json", fixture);
    }

    #[test]
    fn route_ids_must_be_unique() {
        let mut fixture = valid_trigger();
        fixture["cases"].as_array_mut().unwrap().push(route_case(
            "q1",
            "positive",
            json!("rust"),
            &[],
            &["another-tag"],
        ));
        assert_route_finding("trigger_cases.json", fixture);
    }

    #[test]
    fn regression_routes_require_nonempty_notes() {
        let missing = route_fixture(vec![route_case(
            "r1",
            "positive",
            json!("rust"),
            &[],
            &["routing"],
        )]);
        assert_route_finding("regression_cases.json", missing);
        let mut blank = route_fixture(vec![valid_regression_case()]);
        blank["cases"][0]["note"] = json!("  ");
        assert_route_finding("regression_cases.json", blank);
    }

    #[test]
    fn trigger_routes_forbid_a_note_even_when_null() {
        let mut fixture = valid_trigger();
        fixture["cases"][0]["note"] = Value::Null;
        assert_route_finding("trigger_cases.json", fixture);
    }

    #[test]
    fn regression_boundary_routes_must_include_the_target_skill() {
        let mut case = route_case("r1", "boundary", json!("web"), &[], &["adjacent"]);
        case["note"] = json!("Regression coverage");
        assert_route_finding("regression_cases.json", route_fixture(vec![case]));
    }

    #[test]
    fn boundary_routes_require_an_adjacent_or_composition_tag() {
        let fixture = route_fixture(vec![route_case(
            "q1",
            "boundary",
            json!("rust"),
            &[],
            &["other"],
        )]);
        assert_route_finding("trigger_cases.json", fixture);
    }

    #[test]
    fn route_fixtures_reject_unknown_keys() {
        let mut fixture = valid_trigger();
        fixture["extra"] = json!(true);
        assert_route_finding_message(
            "trigger_cases.json",
            fixture,
            "route fixture must be strict JSON with typed fields",
        );
    }

    #[test]
    fn route_cases_reject_unknown_keys() {
        let mut fixture = valid_trigger();
        fixture["cases"][0]["extra"] = json!(true);
        assert_route_finding_message(
            "trigger_cases.json",
            fixture,
            "route fixture must be strict JSON with typed fields",
        );
    }

    #[test]
    fn expected_routes_reject_unknown_keys() {
        let mut fixture = valid_trigger();
        fixture["cases"][0]["expected"]["extra"] = json!(true);
        assert_route_finding_message(
            "trigger_cases.json",
            fixture,
            "route fixture must be strict JSON with typed fields",
        );
    }

    #[test]
    fn route_relations_must_be_supported() {
        let mut fixture = valid_trigger();
        fixture["cases"][0]["relation"] = json!("unrecognised");
        assert_route_finding_message(
            "trigger_cases.json",
            fixture,
            "route fixture must be strict JSON with typed fields",
        );
    }

    #[test]
    fn expected_routes_require_the_primary_skill_key() {
        let mut fixture = valid_trigger();
        fixture["cases"][0]["relation"] = json!("negative");
        fixture["cases"][0]["tags"] = json!(["no-skill"]);
        fixture["cases"][0]["expected"]
            .as_object_mut()
            .unwrap()
            .remove("primary_skill");
        assert_route_finding_message(
            "trigger_cases.json",
            fixture,
            "route fixture must be strict JSON with typed fields",
        );
    }

    #[test]
    fn route_fixtures_require_schema_version_one() {
        let mut fixture = valid_trigger();
        fixture["schema_version"] = json!(2);
        assert_route_finding("trigger_cases.json", fixture);
    }

    #[test]
    fn route_fixtures_require_the_directory_target_skill() {
        let mut fixture = valid_trigger();
        fixture["target_skill"] = json!("web");
        assert_route_finding("trigger_cases.json", fixture);
    }

    #[test]
    fn route_fixtures_require_nonempty_case_lists() {
        assert_route_finding("trigger_cases.json", route_fixture(vec![]));
    }

    #[test]
    fn positive_routes_use_the_target_as_primary() {
        let fixture = route_fixture(vec![route_case(
            "q1",
            "positive",
            json!("web"),
            &[],
            &["routing"],
        )]);
        assert_route_finding("trigger_cases.json", fixture);
    }

    #[test]
    fn negative_routes_exclude_the_target_from_all_skills() {
        let fixture = route_fixture(vec![route_case(
            "q1",
            "negative",
            json!("web"),
            &["rust"],
            &["routing"],
        )]);
        assert_route_finding("trigger_cases.json", fixture);
    }

    #[test]
    fn route_skill_references_must_be_known_and_distinct() {
        let unknown = route_fixture(vec![route_case(
            "q1",
            "positive",
            json!("missing"),
            &[],
            &["routing"],
        )]);
        assert_route_finding("trigger_cases.json", unknown);
        let duplicate = route_fixture(vec![route_case(
            "q1",
            "positive",
            json!("rust"),
            &["web", "web"],
            &["routing"],
        )]);
        assert_route_finding("trigger_cases.json", duplicate);
        let overlap = route_fixture(vec![route_case(
            "q1",
            "positive",
            json!("rust"),
            &["rust"],
            &["routing"],
        )]);
        assert_route_finding("trigger_cases.json", overlap);
        let unknown_companion = route_fixture(vec![route_case(
            "q1",
            "positive",
            json!("rust"),
            &["missing"],
            &["routing"],
        )]);
        assert_route_finding("trigger_cases.json", unknown_companion);
    }

    #[test]
    fn no_skill_routes_require_no_companions_and_a_no_skill_tag() {
        let missing_tag = route_fixture(vec![route_case(
            "q1",
            "negative",
            Value::Null,
            &[],
            &["routing"],
        )]);
        assert_route_finding("trigger_cases.json", missing_tag);
    }

    #[test]
    fn route_prompts_and_tags_must_be_nonempty_and_tags_unique() {
        let mut blank_prompt = valid_trigger();
        blank_prompt["cases"][0]["prompt"] = json!("  ");
        assert_route_finding("trigger_cases.json", blank_prompt);
        let mut empty_tags = valid_trigger();
        empty_tags["cases"][0]["tags"] = json!([]);
        assert_route_finding("trigger_cases.json", empty_tags);
        let mut duplicate_tags = valid_trigger();
        duplicate_tags["cases"][0]["tags"] = json!(["routing", "routing"]);
        assert_route_finding("trigger_cases.json", duplicate_tags);
        let mut empty_tag = valid_trigger();
        empty_tag["cases"][0]["tags"] = json!([""]);
        assert_route_finding("trigger_cases.json", empty_tag);
    }

    #[test]
    fn issue_forms_reject_unknown_top_level_keys() {
        let path = Path::new(".github/ISSUE_TEMPLATE/bug.yml");
        let text = "name: Bug\ndescription: Report a bug\nextra: value\nbody:\n  - type: input\n    id: summary\n    attributes:\n      label: Summary\n";
        let mut scan = Scan::default();
        issue(&mut scan, path, text).unwrap();
        assert_eq!(
            scan.findings,
            vec![finding(
                path,
                "issue_form",
                1,
                "unsupported top-level keys: extra"
            )]
        );
    }

    #[test]
    fn issue_body_items_reject_unknown_keys() {
        let path = Path::new(".github/ISSUE_TEMPLATE/bug.yml");
        let text = "name: Bug\ndescription: Report a bug\nbody:\n  - type: input\n    id: summary\n    placeholder: extra\n    attributes:\n      label: Summary\n";
        let mut scan = Scan::default();
        issue(&mut scan, path, text).unwrap();
        assert_eq!(
            scan.findings,
            vec![finding(
                path,
                "issue_body",
                4,
                "body item 1 has unsupported keys: placeholder"
            )]
        );
    }

    #[test]
    fn heading_slugs_strip_emphasis_markers_and_html_tags() {
        assert!(anchors("# A _helpful_ section\n").contains("a-helpful-section"));
        assert!(anchors("# A *helpful* section\n").contains("a-helpful-section"));
        assert!(anchors("# A <em>helpful</em> section\n").contains("a-helpful-section"));
    }
}
