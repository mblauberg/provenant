//! Harness version compatibility. A bundled table is overlaid by host and
//! project `[[harness_compatibility]]` entries; an assessment is data that the
//! launch path turns into a typed policy warning or configured stop.
use provenant_config::overlays::{OverlayWarning, read_layers};
use provenant_records::Layer;
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, path::Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilityStatus {
    Supported,
    Unsupported,
    Broken,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Assessment {
    pub adapter: String,
    pub version: Option<String>,
    pub status: CompatibilityStatus,
    pub reason: Option<String>,
    pub issue: Option<u64>,
    pub recommended: Option<String>,
    pub source: Layer,
}

/// A three-part version. Missing minor and patch parts are zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct HarnessVersion(u64, u64, u64);

impl HarnessVersion {
    fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = match parts.next() {
            Some(part) => part.parse::<u64>().ok()?,
            None => 0,
        };
        let patch = match parts.next() {
            Some(part) => part.parse::<u64>().ok()?,
            None => 0,
        };
        (parts.next().is_none()).then_some(Self(major, minor, patch))
    }

    /// Read the first `MAJOR[.MINOR[.PATCH]]` token of a `--version` output.
    /// Extra words before or after the token are ordinary CLI decoration.
    fn extract(text: &str) -> Option<(Self, String)> {
        let bytes = text.as_bytes();
        let start = bytes.iter().position(u8::is_ascii_digit)?;
        let mut end = start;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        let mut dots = 0;
        while dots < 2
            && bytes.get(end) == Some(&b'.')
            && bytes.get(end + 1).is_some_and(u8::is_ascii_digit)
        {
            end += 1;
            while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            dots += 1;
        }
        let token = &text[start..end];
        Self::parse(token).map(|version| (version, token.to_owned()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Ge,
    Gt,
    Le,
    Lt,
    Eq,
}

impl Op {
    fn parse(text: &str) -> Option<(Self, &str)> {
        for (symbol, op) in [
            (">=", Self::Ge),
            ("<=", Self::Le),
            (">", Self::Gt),
            ("<", Self::Lt),
            ("=", Self::Eq),
        ] {
            if let Some(rest) = text.strip_prefix(symbol) {
                return Some((op, rest));
            }
        }
        None
    }

    fn holds(self, actual: HarnessVersion, expected: HarnessVersion) -> bool {
        match actual.cmp(&expected) {
            Ordering::Less => matches!(self, Self::Lt | Self::Le),
            Ordering::Equal => !matches!(self, Self::Gt | Self::Lt),
            Ordering::Greater => matches!(self, Self::Gt | Self::Ge),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Range(Vec<(Op, HarnessVersion)>);

impl Range {
    fn parse(text: &str) -> Option<Self> {
        let mut comparators = Vec::new();
        for part in text.split(',') {
            let part = part.trim();
            let (op, version) = Op::parse(part)?;
            comparators.push((op, HarnessVersion::parse(version.trim())?));
        }
        (!comparators.is_empty()).then_some(Self(comparators))
    }

    fn matches(&self, version: HarnessVersion) -> bool {
        self.0
            .iter()
            .all(|(op, expected)| op.holds(version, *expected))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RangeRow {
    range: Range,
    status: CompatibilityStatus,
    reason: Option<String>,
    issue: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    adapter: String,
    ranges: Vec<RangeRow>,
    recommended: Option<String>,
    source: Layer,
}

#[derive(Debug, Deserialize)]
struct Asset {
    #[serde(default)]
    harness_compatibility: Vec<EntryAsset>,
}

#[derive(Debug, Deserialize)]
struct EntryAsset {
    adapter: String,
    #[serde(default)]
    provenant: Option<String>,
    #[serde(default)]
    recommended: Option<String>,
    #[serde(default)]
    ranges: Vec<RangeAsset>,
}

#[derive(Debug, Deserialize)]
struct RangeAsset {
    range: String,
    status: CompatibilityStatus,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    issue: Option<u64>,
}

fn decode_entry(
    row: EntryAsset,
    source: Layer,
    warnings: &mut Vec<OverlayWarning>,
) -> Option<Entry> {
    let key = format!("harness_compatibility[{}]", row.adapter);
    if let Some(constraint) = row.provenant.as_deref() {
        let Some(range) = Range::parse(constraint) else {
            warnings.push(OverlayWarning::new(
                source,
                key,
                format!("invalid provenant constraint: {constraint}"),
            ));
            return None;
        };
        let current =
            HarnessVersion::extract(env!("CARGO_PKG_VERSION")).map(|(version, _)| version);
        if !current.is_some_and(|version| range.matches(version)) {
            return None;
        }
    }
    let mut ranges = Vec::new();
    for item in row.ranges {
        let Some(range) = Range::parse(&item.range) else {
            warnings.push(OverlayWarning::new(
                source,
                key,
                format!("invalid version range: {}", item.range),
            ));
            return None;
        };
        ranges.push(RangeRow {
            range,
            status: item.status,
            reason: item.reason,
            issue: item.issue,
        });
    }
    Some(Entry {
        adapter: row.adapter,
        ranges,
        recommended: row.recommended,
        source,
    })
}

#[derive(Debug, Default)]
pub struct Table {
    entries: Vec<Entry>,
}

impl Table {
    /// The shipped table. It currently has no entries: compatibility facts
    /// arrive through owner overlays or a later bundled revision.
    pub fn bundled() -> Self {
        let mut warnings = Vec::new();
        let mut table = Self::default();
        if let Ok(asset) =
            toml_edit::de::from_str::<Asset>(include_str!("../assets/harness-compatibility.toml"))
        {
            for row in asset.harness_compatibility {
                if let Some(entry) = decode_entry(row, Layer::Default, &mut warnings) {
                    table.insert(entry);
                }
            }
        }
        table
    }

    /// The bundled table with host and project entries replacing rows by
    /// adapter, in that precedence order. Skipped entries are reported.
    pub fn effective(state: &Path, project: Option<&Path>) -> (Self, Vec<OverlayWarning>) {
        let (layers, mut warnings) = read_layers(state, project, "harness_compatibility");
        let mut table = Self::bundled();
        for layer in layers {
            let document: toml_edit::DocumentMut = match layer.text.parse() {
                Ok(document) => document,
                Err(error) => {
                    warnings.push(OverlayWarning::new(
                        layer.source,
                        "harness_compatibility",
                        format!("invalid TOML: {error}"),
                    ));
                    continue;
                }
            };
            let rows: Vec<&toml_edit::Table> = document
                .as_table()
                .get("harness_compatibility")
                .and_then(toml_edit::Item::as_array_of_tables)
                .map_or_else(Vec::new, |rows| rows.iter().collect());
            for row in rows {
                let key = row
                    .get("adapter")
                    .and_then(toml_edit::Item::as_str)
                    .unwrap_or("<unreadable adapter>")
                    .to_owned();
                let decoded = toml_edit::de::from_str::<EntryAsset>(&row.to_string());
                match decoded {
                    Ok(row) => {
                        if let Some(entry) = decode_entry(row, layer.source, &mut warnings) {
                            table.insert(entry);
                        }
                    }
                    Err(error) => warnings.push(OverlayWarning::new(
                        layer.source,
                        format!("harness_compatibility[{key}]"),
                        error.to_string(),
                    )),
                }
            }
        }
        (table, warnings)
    }

    fn insert(&mut self, entry: Entry) {
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|row| row.adapter == entry.adapter)
        {
            *existing = entry;
        } else {
            self.entries.push(entry);
        }
    }

    /// Resolve one adapter's version against the first matching range. An
    /// unreadable version is Unknown; a row with no matching range is Supported.
    pub fn assess(&self, adapter: &str, version: Option<&str>) -> Assessment {
        let parsed = version.and_then(HarnessVersion::extract);
        let recorded = parsed
            .as_ref()
            .map(|(_, token)| token.clone())
            .or_else(|| version.map(str::to_owned));
        let entry = self.entries.iter().find(|row| row.adapter == adapter);
        let mut assessment = Assessment {
            adapter: adapter.to_owned(),
            version: recorded,
            status: CompatibilityStatus::Unknown,
            reason: None,
            issue: None,
            recommended: entry.and_then(|row| row.recommended.clone()),
            source: entry.map_or(Layer::Default, |row| row.source),
        };
        let Some((actual, _)) = parsed else {
            return assessment;
        };
        let Some(entry) = entry else {
            assessment.status = CompatibilityStatus::Supported;
            return assessment;
        };
        for row in &entry.ranges {
            if row.range.matches(actual) {
                assessment.status = row.status;
                assessment.reason = row.reason.clone();
                assessment.issue = row.issue;
                return assessment;
            }
        }
        assessment.status = CompatibilityStatus::Supported;
        assessment
    }
}

/// The policy code for a status; None for Supported and Unknown.
pub fn policy_code(status: CompatibilityStatus) -> Option<&'static str> {
    match status {
        CompatibilityStatus::Unsupported => Some("HARNESS_VERSION_UNSUPPORTED"),
        CompatibilityStatus::Broken => Some("HARNESS_VERSION_BROKEN"),
        CompatibilityStatus::Supported | CompatibilityStatus::Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join("config.toml"), text).unwrap();
        (temp, state)
    }

    #[test]
    fn every_comparator_is_bounded_correctly() {
        let cases = [
            (">=0.50", "0.50.0", true),
            (">=0.50", "0.49.9", false),
            (">0.50", "0.50.0", false),
            (">0.50", "0.50.1", true),
            ("<0.60", "0.59.9", true),
            ("<0.60", "0.60.0", false),
            ("<=0.60", "0.60.0", true),
            ("<=0.60", "0.60.1", false),
            ("=0.61", "0.61.0", true),
            ("=0.61", "0.61.1", false),
        ];
        for (constraint, actual, expected) in cases {
            let range = Range::parse(constraint).unwrap();
            let version = HarnessVersion::extract(actual).unwrap().0;
            assert_eq!(range.matches(version), expected, "{constraint} vs {actual}");
        }
    }

    #[test]
    fn missing_version_parts_are_zero() {
        assert!(
            Range::parse(">=2.9")
                .unwrap()
                .matches(HarnessVersion::parse("2.9.8").unwrap())
        );
        assert!(
            Range::parse("=2")
                .unwrap()
                .matches(HarnessVersion::parse("2.0.0").unwrap())
        );
        assert!(
            !Range::parse("=2")
                .unwrap()
                .matches(HarnessVersion::parse("2.0.1").unwrap())
        );
    }

    #[test]
    fn comma_separated_comparators_all_hold() {
        let range = Range::parse(">=0.50, <0.60").unwrap();
        assert!(range.matches(HarnessVersion::parse("0.55.2").unwrap()));
        assert!(!range.matches(HarnessVersion::parse("0.49.9").unwrap()));
        assert!(!range.matches(HarnessVersion::parse("0.60.0").unwrap()));
    }

    #[test]
    fn first_matching_range_wins() {
        let text = r#"
[[harness_compatibility]]
adapter = "codex"
ranges = [
  { range = ">=0.50", status = "unsupported", reason = "first", issue = 1 },
  { range = "<1.0", status = "broken", reason = "second", issue = 2 },
]
"#;
        let (_temp, state) = state_with(text);
        let (table, warnings) = Table::effective(&state, None);
        assert!(warnings.is_empty(), "{warnings:?}");
        let assessment = table.assess("codex", Some("codex 0.61.0"));
        assert_eq!(assessment.status, CompatibilityStatus::Unsupported);
        assert_eq!(assessment.reason.as_deref(), Some("first"));
        assert_eq!(assessment.issue, Some(1));
        assert_eq!(assessment.version.as_deref(), Some("0.61.0"));
        assert_eq!(assessment.source, Layer::Host);
    }

    #[test]
    fn a_garbage_range_skips_the_entry_with_a_warning() {
        let text = r#"
[[harness_compatibility]]
adapter = "claude"
ranges = [{ range = "about 0.5", status = "broken" }]
"#;
        let (_temp, state) = state_with(text);
        let (table, warnings) = Table::effective(&state, None);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].source, Layer::Host);
        let assessment = table.assess("claude", Some("1.2.3"));
        assert_eq!(assessment.status, CompatibilityStatus::Supported);
        assert_eq!(assessment.source, Layer::Default);
    }

    #[test]
    fn a_project_entry_replaces_the_host_entry_and_reports_its_layer() {
        let (_temp, state) = state_with(
            r#"
[[harness_compatibility]]
adapter = "codex"
ranges = [{ range = ">=0.1", status = "unsupported", reason = "host" }]
"#,
        );
        let project = _temp.path().join("project");
        std::fs::create_dir_all(project.join(".provenant")).unwrap();
        std::fs::write(
            project.join(".provenant").join("config.toml"),
            r#"
[[harness_compatibility]]
adapter = "codex"
provenant = ">=2.9"
recommended = ">=0.60"
ranges = [{ range = ">=0.1", status = "broken", reason = "project", issue = 42 }]
"#,
        )
        .unwrap();
        let (table, warnings) = Table::effective(&state, Some(&project));
        assert!(warnings.is_empty(), "{warnings:?}");
        let assessment = table.assess("codex", Some("codex-cli 0.61.0"));
        assert_eq!(assessment.status, CompatibilityStatus::Broken);
        assert_eq!(assessment.reason.as_deref(), Some("project"));
        assert_eq!(assessment.issue, Some(42));
        assert_eq!(assessment.recommended.as_deref(), Some(">=0.60"));
        assert_eq!(assessment.source, Layer::Project);
    }

    #[test]
    fn unreadable_version_assesses_unknown() {
        let table = Table::bundled();
        assert_eq!(
            table.assess("codex", None).status,
            CompatibilityStatus::Unknown
        );
        assert_eq!(
            table.assess("codex", Some("no digits here")).status,
            CompatibilityStatus::Unknown
        );
        assert_eq!(
            table.assess("codex", Some("codex 0.61.0")).status,
            CompatibilityStatus::Supported
        );
    }

    #[test]
    fn an_entry_outside_this_provenant_version_is_ignored() {
        let text = r#"
[[harness_compatibility]]
adapter = "codex"
provenant = ">=99.0"
ranges = [{ range = ">=0.1", status = "broken" }]
"#;
        let (_temp, state) = state_with(text);
        let (table, warnings) = Table::effective(&state, None);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            table.assess("codex", Some("0.61.0")).status,
            CompatibilityStatus::Supported
        );
    }

    #[test]
    fn status_maps_to_a_policy_code() {
        assert_eq!(policy_code(CompatibilityStatus::Supported), None);
        assert_eq!(policy_code(CompatibilityStatus::Unknown), None);
        assert_eq!(
            policy_code(CompatibilityStatus::Unsupported),
            Some("HARNESS_VERSION_UNSUPPORTED")
        );
        assert_eq!(
            policy_code(CompatibilityStatus::Broken),
            Some("HARNESS_VERSION_BROKEN")
        );
    }
}
