//! Deterministic checks of tracked skill, documentation and harness assets.
use super::{CheckError, GitContext, Scan, read_file};
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

pub(super) fn inspect(git: &GitContext<'_>) -> Result<Scan, CheckError> {
    let index = git.read(GitRead::IndexEntries { paths: Vec::new() })?;
    let mut scan = Scan::default();
    scan.note("tracked-index", &index);
    let mut paths = BTreeSet::new();
    for record in index
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or(CheckError::Evidence("malformed Git index"))?;
        let header = std::str::from_utf8(
            record
                .get(..tab)
                .ok_or(CheckError::Evidence("index header"))?,
        )
        .map_err(|_| CheckError::Evidence("index header encoding"))?;
        if header.split_whitespace().nth(2) != Some("0") {
            return Err(CheckError::Evidence("unmerged Git index"));
        }
        let path = PathBuf::from(OsString::from_vec(
            record
                .get(tab + 1..)
                .ok_or(CheckError::Evidence("index path"))?
                .to_vec(),
        ));
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
        let bytes = read_file(git.repo.root_dir(), path, FILE_LIMIT)?;
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
            markdown(&mut scan, path, text, &files, &paths, git)?;
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
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("trigger_cases.json" | "regression_cases.json")
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
    for field in ["icon_small", "icon_large", "brand_color"] {
        if let Some(value) = string(interface.get(field))
            && field.starts_with("icon_")
        {
            let resolved = resolve(skill, value);
            if let Some(asset) = resolved.filter(|asset| paths.contains(asset)) {
                let bytes = read_file(git.repo.root_dir(), &asset, FILE_LIMIT)?;
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
    for item in items {
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
    note: Option<String>,
}
#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Relation {
    Positive,
    Negative,
    Boundary,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expected {
    primary_skill: Option<String>,
    companion_skills: Vec<String>,
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
    let mut ids = BTreeSet::new();
    let mut valid =
        route.schema_version == 1 && route.target_skill == skill && !route.cases.is_empty();
    for case in route.cases {
        let companions: BTreeSet<_> = case.expected.companion_skills.iter().collect();
        let tags: BTreeSet<_> = case.tags.iter().collect();
        let primary = case.expected.primary_skill.as_deref();
        valid &= !case.id.is_empty()
            && ids.insert(case.id)
            && !case.prompt.trim().is_empty()
            && !case.tags.is_empty()
            && tags.len() == case.tags.len()
            && case.tags.iter().all(|tag| !tag.is_empty())
            && primary.is_none_or(|s| skills.contains(s))
            && companions.len() == case.expected.companion_skills.len()
            && companions
                .iter()
                .all(|s| skills.contains(*s) && Some(s.as_str()) != primary)
            && (primary.is_some()
                || (companions.is_empty() && case.tags.iter().any(|t| t == "no-skill")))
            && match case.relation {
                Relation::Positive => primary == Some(skill),
                Relation::Negative => {
                    primary != Some(skill) && !companions.iter().any(|s| s.as_str() == skill)
                }
                Relation::Boundary => case
                    .tags
                    .iter()
                    .any(|t| matches!(t.as_str(), "adjacent" | "composition")),
            }
            && case.note.as_ref().is_none_or(|s| !s.trim().is_empty());
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
fn anchors(text: &str) -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    let mut fence = None;
    for (_, line) in prose(text, &mut fence) {
        let trimmed = line.trim_start_matches('#');
        if trimmed.len() == line.len() || !trimmed.starts_with(' ') {
            continue;
        }
        let base: String = trimmed
            .trim()
            .trim_end_matches('#')
            .trim()
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
                let bytes = read_file(git.repo.root_dir(), &destination, FILE_LIMIT)?;
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
                && files
                    .get(&destination)
                    .is_none_or(|text| !anchors(text).contains(anchor))
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
