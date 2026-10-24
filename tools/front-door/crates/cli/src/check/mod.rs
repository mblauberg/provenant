//! Check composition and stateless, evidence-bound pagination.
use crate::grammar::{Page, Rules, absolute};
use clap::{Subcommand, builder::TypedValueParser};
use provenant_confinement::utility::run_release_read_utility;
use provenant_confinement::{
    GitRead, ReadOnlyRepo, UtilityBudget, UtilityRuntime, run_read_utility,
};
use provenant_core::{CancelToken, GitOid, RelativePath};
use provenant_records::{CanonicalV1, FileDigest, Finding, Outcome, PathWire};
use provenant_supervisor::{
    exec::{ExecOutcome, Executor, Termination},
    fs::{FileKind, FsError, Metadata, SafeDir},
};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;
#[cfg(test)]
mod dates;
mod harness;
mod release;
#[derive(Debug, Subcommand)]
pub enum Check {
    Config(Page),
    Install {
        #[arg(long, value_parser = clap::builder::PathBufValueParser::new().try_map(absolute))]
        source: Option<PathBuf>,
        #[command(flatten)]
        page: Page,
    },
    Harness(Page),
    Release {
        #[arg(long, requires = "head")]
        base: Option<GitOid>,
        #[arg(long, requires = "base")]
        head: Option<GitOid>,
        #[command(flatten)]
        page: Page,
    },
    Writing {
        #[arg(required = true, num_args = 1..=256)]
        paths: Vec<PathBuf>,
        #[arg(long, value_enum, default_value = "general")]
        rules: Rules,
        #[command(flatten)]
        page: Page,
    },
}

#[derive(Debug, Error)]
pub enum CheckError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("required check evidence is unreadable or incomplete: {0}")]
    Evidence(&'static str),
    #[error(transparent)]
    Fs(#[from] FsError),
    #[error(transparent)]
    Utility(#[from] provenant_confinement::utility::UtilityError),
    #[error(transparent)]
    Path(#[from] provenant_records::PathWireError),
    #[error(transparent)]
    Canonical(#[from] provenant_records::CanonicalError),
}
impl CheckError {
    pub fn outcome(&self) -> Outcome {
        match self {
            Self::Invalid(_) => Outcome::Invalid,
            _ => Outcome::Error,
        }
    }
}
#[derive(Default)]
pub(super) struct Scan {
    pub findings: Vec<Finding>,
    pub evidence: Vec<(String, FileDigest)>,
}
impl Scan {
    pub(super) fn note(&mut self, label: impl Into<String>, bytes: &[u8]) {
        self.evidence
            .push((label.into(), CanonicalV1::file_digest(bytes)));
    }
    pub(super) fn finding(
        &mut self,
        rule: &str,
        path: &Path,
        line: usize,
        message: &str,
    ) -> Result<(), CheckError> {
        self.findings.push(Finding {
            rule: rule.into(),
            path: wire(path)?,
            line,
            message: message.into(),
        });
        Ok(())
    }
}
fn wire(path: &Path) -> Result<PathWire, CheckError> {
    PathWire::from_relative(
        &RelativePath::parse(path).map_err(provenant_records::PathWireError::from)?,
    )
    .map_err(Into::into)
}
/// Inputs include the selected root and the full rule bundle identity; no cursor
/// acknowledges or stores a private snapshot. The caller rechecks every page.
#[derive(Debug, Serialize)]
pub struct CheckPage {
    pub findings: Vec<Finding>,
    pub count: u64,
    pub offset: u64,
    pub next_cursor: Option<String>,
    pub input_digest: FileDigest,
}
impl CheckPage {
    pub fn outcome(&self) -> Outcome {
        if self.count == 0 {
            Outcome::Ok
        } else {
            Outcome::Failed
        }
    }
    pub fn human(&self) -> Result<String, serde_json::Error> {
        let mut text = String::new();
        for finding in &self.findings {
            text.push_str(&format!(
                "{}:{}: {}: {}\n",
                serde_json::to_string(&finding.path)?,
                finding.line,
                finding.rule,
                finding.message
            ));
        }
        text.push_str(&format!("{} finding(s)\n", self.count));
        if let Some(cursor) = &self.next_cursor {
            text.push_str(&format!("next cursor: {cursor}\n"));
        }
        Ok(text)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u64,
    digest: String,
    offset: u64,
}
fn paginate(
    mut scan: Scan,
    selector: impl Serialize,
    page: &Page,
) -> Result<CheckPage, CheckError> {
    if !(1..=100).contains(&page.limit) {
        return Err(CheckError::Invalid("check limit must be 1..100"));
    }
    // Source digest binds rule code, configuration assets and external fixture
    // bundle, not just a manually incremented rule number.
    let digest = CanonicalV1::record_digest(
        "check-input",
        1,
        &(selector, &scan.evidence, &scan.findings),
    )?;
    let input_digest = CanonicalV1::file_digest(&digest.as_bytes()[..]);
    let offset = match &page.cursor {
        None => 0,
        Some(token) => {
            if token.len() > 512 {
                return Err(CheckError::Invalid("invalid check cursor"));
            }
            let cursor: Cursor = serde_json::from_str(token)
                .map_err(|_| CheckError::Invalid("invalid check cursor"))?;
            if cursor.version != 1 || cursor.digest != input_digest.to_string() {
                return Err(CheckError::Invalid(
                    "check inputs, rules or selectors changed; restart pagination",
                ));
            }
            cursor.offset
        }
    };
    let count = scan.findings.len() as u64;
    if offset > count || (offset == count && offset != 0) {
        return Err(CheckError::Invalid("check cursor is outside the findings"));
    }
    let mut result = CheckPage {
        findings: Vec::new(),
        count,
        offset,
        next_cursor: None,
        input_digest,
    };
    for finding in scan
        .findings
        .drain(..)
        .skip(offset as usize)
        .take(page.limit as usize)
    {
        result.findings.push(finding);
        result.next_cursor = next_cursor(&result)?;
        // Check the actual envelope/LF against the shared renderer's bound.
        // All check command names are at most this length. Human formatting is
        // smaller than its JSON envelope because it retains the same path form.
        let envelope =
            provenant_records::CommandResult::new("check harness", result.outcome(), &result);
        match crate::render(&envelope, crate::RenderMode::Json, &mut std::io::sink()) {
            Ok(()) => {}
            Err(crate::RenderError::TooLarge) if result.findings.len() > 1 => {
                result.findings.pop();
                result.next_cursor = next_cursor(&result)?;
                break;
            }
            // One oversized item is handed intact to the edge's existing
            // overflow publisher; it emits RESULT_TOO_LARGE with an artifact.
            Err(crate::RenderError::TooLarge) => break,
            Err(_) => return Err(CheckError::Evidence("finding encoding failed")),
        }
    }
    Ok(result)
}
fn next_cursor(page: &CheckPage) -> Result<Option<String>, CheckError> {
    let end = page.offset + page.findings.len() as u64;
    if end < page.count {
        serde_json::to_string(&Cursor {
            version: 1,
            digest: page.input_digest.to_string(),
            offset: end,
        })
        .map(Some)
        .map_err(|_| CheckError::Evidence("cursor encoding"))
    } else {
        Ok(None)
    }
}

pub struct GitContext<'a> {
    pub repo: &'a ReadOnlyRepo,
    pub runtime: &'a UtilityRuntime,
    pub executor: &'a dyn Executor,
    pub cancel: &'a CancelToken,
}
impl GitContext<'_> {
    pub(super) fn read(&self, op: GitRead) -> Result<Vec<u8>, CheckError> {
        checked_bytes(self.read_mode(op, false)?)
    }
    pub(super) fn read_release(&self, op: GitRead) -> Result<ExecOutcome, CheckError> {
        self.read_mode(op, true)
    }
    fn read_mode(&self, op: GitRead, release: bool) -> Result<ExecOutcome, CheckError> {
        let budget = UtilityBudget::new(
            Duration::from_secs(5),
            Duration::from_millis(250),
            Duration::from_secs(2),
            64 * 1024 * 1024,
            64 * 1024,
        )?;
        let read = if release {
            run_release_read_utility
        } else {
            run_read_utility
        };
        Ok(read(
            self.executor,
            op,
            self.repo,
            self.runtime,
            budget,
            self.cancel,
        )?)
    }
}
fn checked_bytes(output: ExecOutcome) -> Result<Vec<u8>, CheckError> {
    if output.termination != Termination::Exit(0) {
        return Err(CheckError::Evidence("Git read failed"));
    }
    Ok(output.stdout.bytes)
}

/// A tracked input is read without following links and with metadata checked
/// around every component and the final byte read.
pub(super) enum WorkingFile {
    Bytes(Vec<u8>),
    Symlink(Vec<u8>),
    Missing,
}
pub(super) fn read_file(
    root: &dyn SafeDir,
    path: &Path,
    limit: u64,
) -> Result<WorkingFile, CheckError> {
    let relative = RelativePath::parse(path).map_err(provenant_records::PathWireError::from)?;
    let mut components = relative.as_path().components().peekable();
    read_file_at(root, &mut components, limit, 0)
}
fn read_file_at<'a>(
    directory: &dyn SafeDir,
    components: &mut std::iter::Peekable<std::path::Components<'a>>,
    limit: u64,
    depth: usize,
) -> Result<WorkingFile, CheckError> {
    if depth > 128 {
        return Err(CheckError::Evidence("tracked path exceeds depth bound"));
    }
    let component = components
        .next()
        .ok_or(CheckError::Evidence("empty tracked path"))?;
    let name = match component {
        std::path::Component::Normal(name) => name,
        _ => return Err(CheckError::Evidence("invalid tracked path component")),
    };
    let before = match directory.metadata_at(name, false) {
        Ok(value) => value,
        Err(error) if error.is_not_found() => return Ok(WorkingFile::Missing),
        Err(error) => return Err(error.into()),
    };
    if before.kind == FileKind::Symlink {
        let target = directory.read_link(name)?;
        if !same_metadata(&before, &directory.metadata_at(name, false)?) {
            return Err(CheckError::Evidence("tracked symlink changed"));
        }
        return Ok(WorkingFile::Symlink(
            target.as_os_str().as_encoded_bytes().to_vec(),
        ));
    }
    if components.peek().is_some() {
        if before.kind != FileKind::Directory {
            return Err(CheckError::Evidence(
                "tracked path component is not a directory",
            ));
        }
        let child = directory.open_verified_directory(name)?;
        if !before.same_identity(&child.metadata()?) {
            return Err(CheckError::Evidence("tracked directory changed"));
        }
        let result = read_file_at(child.as_ref(), components, limit, depth + 1)?;
        if !same_metadata(&before, &directory.metadata_at(name, false)?) {
            return Err(CheckError::Evidence("tracked directory changed"));
        }
        return Ok(result);
    }
    if before.kind != FileKind::Regular {
        return Err(CheckError::Evidence("tracked input is not a regular file"));
    }
    if before.size > limit {
        return Err(CheckError::Evidence("tracked input exceeds its byte bound"));
    }
    let mut file = directory.open_read(name)?;
    let opened = file.metadata()?;
    if !same_metadata(&before, &opened) {
        return Err(CheckError::Evidence(
            "tracked input changed during inspection",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CheckError::Evidence("file read failed"))?;
    if bytes.len() as u64 > limit
        || bytes.len() as u64 != before.size
        || !same_metadata(&before, &file.metadata()?)
        || !same_metadata(&before, &directory.metadata_at(name, false)?)
    {
        return Err(CheckError::Evidence(
            "tracked input changed during inspection",
        ));
    }
    Ok(WorkingFile::Bytes(bytes))
}
fn same_metadata(a: &Metadata, b: &Metadata) -> bool {
    a.same_identity(b)
        && a.kind == b.kind
        && a.size == b.size
        && a.modified_ns == b.modified_ns
        && a.mode == b.mode
        && a.uid == b.uid
        && a.links == b.links
}

/// Parse NUL-delimited `git ls-files --stage -z` records once for all checks.
pub(super) struct IndexRecord<'a> {
    pub mode: &'a [u8],
    pub oid: &'a [u8],
    pub stage: &'a [u8],
    pub path: &'a [u8],
}
pub(super) fn index_records(bytes: &[u8]) -> Result<Vec<IndexRecord<'_>>, CheckError> {
    if !bytes.is_empty() && !bytes.ends_with(&[0]) {
        return Err(CheckError::Evidence(
            "Git returned unterminated index records",
        ));
    }
    let content = if bytes.ends_with(&[0]) {
        &bytes[..bytes.len() - 1]
    } else {
        bytes
    };
    if content.is_empty() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for record in content.split(|byte| *byte == 0) {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or(CheckError::Evidence("malformed Git index"))?;
        let header = &record[..tab];
        let mut fields = header
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|field| !field.is_empty());
        let mode = fields
            .next()
            .ok_or(CheckError::Evidence("malformed Git index"))?;
        let oid = fields
            .next()
            .ok_or(CheckError::Evidence("malformed Git index"))?;
        let stage = fields
            .next()
            .ok_or(CheckError::Evidence("malformed Git index"))?;
        if fields.next().is_some() {
            return Err(CheckError::Evidence("malformed Git index"));
        }
        entries.push(IndexRecord {
            mode,
            oid,
            stage,
            path: &record[tab + 1..],
        });
    }
    Ok(entries)
}
/// Library owners supply config/install inspections. CLI retains only a page.
pub fn config_page(
    inspection: provenant_config::ConfigInspection,
    selector: impl Serialize,
    page: &Page,
) -> Result<CheckPage, CheckError> {
    let mut scan = Scan {
        findings: inspection.check.findings,
        evidence: vec![("config".into(), inspection.input_digest)],
    };
    if let Some(error) = inspection.error {
        use provenant_config::ConfigError;
        let (rule, line) = match error {
            ConfigError::Syntax { location } => ("config.syntax", location.line),
            ConfigError::MissingVersion { location }
            | ConfigError::UnsupportedVersion { location } => ("config.version", location.line),
            ConfigError::Schema { location, .. }
            | ConfigError::CollectionLimit { location }
            | ConfigError::Domain { location, .. } => ("config.schema", location.line),
            _ => return Err(CheckError::Evidence("config inspection failed")),
        };
        scan.finding(
            rule,
            Path::new("config.toml"),
            line,
            "configuration violates the typed TOML schema",
        )?;
    }
    paginate(scan, selector, page)
}
pub fn install_page(
    inspection: provenant_config::InstallInspection,
    home: &Path,
    state: &Path,
    selector: impl Serialize,
    page: &Page,
) -> Result<CheckPage, CheckError> {
    let mut scan = Scan::default();
    scan.evidence
        .push(("install".into(), inspection.input_digest));
    if inspection.error.is_some() {
        scan.finding(
            "install.invalid",
            Path::new("state/install/receipt.json"),
            1,
            "installation evidence violates its recorded schema or identity",
        )?;
    }
    for finding in inspection.findings {
        let path = finding.path.to_path_buf()?;
        let (prefix, relative) = if let Ok(p) = path.strip_prefix(state) {
            ("state", p)
        } else if let Ok(p) = path.strip_prefix(home) {
            ("home", p)
        } else {
            (
                "absolute",
                path.strip_prefix("/")
                    .map_err(|_| CheckError::Evidence("invalid install finding path"))?,
            )
        };
        scan.finding(
            &finding.rule,
            &Path::new(prefix).join(relative),
            1,
            "managed installation differs from its recorded identity",
        )?;
    }
    paginate(scan, selector, page)
}
pub fn harness_page(
    git: &GitContext<'_>,
    selector: impl Serialize,
    page: &Page,
) -> Result<CheckPage, CheckError> {
    paginate(harness::inspect(git)?, selector, page)
}
pub fn release_page(
    git: &GitContext<'_>,
    range: Option<(GitOid, GitOid)>,
    selector: impl Serialize,
    page: &Page,
) -> Result<CheckPage, CheckError> {
    paginate(release::inspect(git, range)?, selector, page)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pages_fit_encoded_byte_bound_and_resume_at_the_delivered_offset()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut scan = Scan::default();
        let path = PathBuf::from(format!(
            "{}SKILL.md",
            format!("{}/", "\u{1}".repeat(200)).repeat(14)
        ));
        for line in 1..=100 {
            scan.finding("markdown_link", &path, line, "missing tracked local link")?;
        }
        let page = paginate(
            scan,
            "fixture rules",
            &Page {
                limit: 100,
                cursor: None,
            },
        )?;
        assert!(!page.findings.is_empty() && page.findings.len() < 100);
        let cursor: Cursor = serde_json::from_str(page.next_cursor.as_deref().ok_or("cursor")?)?;
        assert_eq!(cursor.offset, page.findings.len() as u64);
        let mut bytes = Vec::new();
        crate::render(
            &provenant_records::CommandResult::new("check harness", page.outcome(), &page),
            crate::RenderMode::Json,
            &mut bytes,
        )?;
        assert!(bytes.len() <= crate::MAX_RESULT_BYTES);
        Ok(())
    }
}
