use super::*;
use provenant_supervisor::fs::{Directory, FileKind};
use std::{ffi::OsStr, path::Component};
const MAX_FILES: usize = 256;
const MAX_TOTAL: usize = 16 * 1024 * 1024;
const MAX_VISITED_ENTRIES: usize = 4096;
fn relative<'a>(root: &Path, path: &'a Path) -> Result<&'a Path, WritingError> {
    let path = if path.is_absolute() {
        path.strip_prefix(root)
            .map_err(|_| WritingError::Invalid("path escapes project"))?
    } else {
        path
    };
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(WritingError::Invalid("expected a confined relative path"));
    }
    Ok(path)
}
fn parent(root: &Path, path: &Path) -> Result<(Directory, std::ffi::OsString), WritingError> {
    let mut dir = Directory::open_anchor(root)?;
    let mut parts = path.components().peekable();
    while let Some(Component::Normal(name)) = parts.next() {
        if parts.peek().is_none() {
            return Ok((dir, name.to_owned()));
        }
        dir = dir.open_verified_directory(name)?;
    }
    Err(WritingError::Invalid("empty path"))
}
fn rules(root: &Path, path: &Path) -> Result<Vec<u8>, WritingError> {
    let (dir, name) = parent(root, path)?;
    dir.read_bounded(&name, MAX_RULE_BYTES as u64)
        .map_err(|error| {
            if matches!(error, FsError::TooLarge { .. }) {
                WritingError::RulesLimit
            } else {
                error.into()
            }
        })
}
pub fn inspect(
    product: &Path,
    project: &Path,
    paths: &[PathBuf],
    set: RuleSet,
) -> Result<Inspection, WritingError> {
    if paths.is_empty() || paths.len() > MAX_FILES {
        return Err(WritingError::Invalid("expected 1..256 paths"));
    }
    // Validate selectors before opening any evidence.
    let paths = paths
        .iter()
        .map(|p| relative(project, p).map(Path::to_path_buf))
        .collect::<Result<Vec<_>, _>>()?;
    let asset = Path::new(set.asset());
    let bytes = rules(product, asset)?;
    let matcher = Matcher::from_json(&bytes, set)?;
    let mut inspection = Inspection {
        findings: Vec::new(),
        evidence: vec![(asset.to_owned(), bytes)],
    };
    let mut scan = Scan {
        matcher: &matcher,
        result: &mut inspection,
        seen: BTreeSet::new(),
        total: 0,
        visited: 0,
    };
    for path in paths {
        let (dir, name) = parent(project, &path).map_err(|error| match error {
            WritingError::Io(ref e) if e.is_not_found() => {
                WritingError::Invalid("input path does not exist")
            }
            _ => error,
        })?;
        scan.visit(&dir, &name, &path)?;
    }
    if scan.seen.is_empty() {
        return Err(WritingError::Invalid("no writing files resolved"));
    }
    inspection.findings.sort_by(|a, b| {
        (&a.path, a.finding.start, a.finding.end, &a.finding.rule).cmp(&(
            &b.path,
            b.finding.start,
            b.finding.end,
            &b.finding.rule,
        ))
    });
    Ok(inspection)
}
struct Scan<'a> {
    matcher: &'a Matcher,
    result: &'a mut Inspection,
    seen: BTreeSet<PathBuf>,
    total: usize,
    visited: usize,
}
impl Scan<'_> {
    fn visit(&mut self, dir: &Directory, name: &OsStr, path: &Path) -> Result<(), WritingError> {
        self.visited += 1;
        if self.visited > MAX_VISITED_ENTRIES {
            return Err(WritingError::EvidenceLimit);
        }
        let metadata = dir.metadata_at(name, false).map_err(|e| {
            if e.is_not_found() {
                WritingError::Invalid("input path does not exist")
            } else {
                e.into()
            }
        })?;
        match metadata.kind {
            FileKind::Directory => {
                let child = dir.open_verified_directory(name)?;
                let mut names = Vec::new();
                for entry in child.entries()? {
                    names.push(entry?);
                }
                names.sort();
                for name in names {
                    let path = path.join(&name);
                    let kind = child.metadata_at(&name, false)?.kind;
                    if kind == FileKind::Directory
                        || matches!(
                            path.extension().and_then(|s| s.to_str()),
                            Some("md" | "markdown" | "tex" | "latex" | "sty" | "cls" | "txt")
                        )
                    {
                        self.visit(&child, &name, &path)?;
                    }
                }
            }
            FileKind::Regular => {
                if self
                    .matcher
                    .skip_files
                    .iter()
                    .any(|s| path.file_name().is_some_and(|n| n.eq_ignore_ascii_case(s)))
                    || !self.seen.insert(path.to_owned())
                {
                    return Ok(());
                }
                if self.seen.len() > MAX_FILES {
                    return Err(WritingError::EvidenceLimit);
                }
                let bytes = dir.read_bounded(name, MAX_TEXT_BYTES as u64)?;
                self.total += bytes.len();
                if self.total > MAX_TOTAL {
                    return Err(WritingError::EvidenceLimit);
                }
                let text = std::str::from_utf8(&bytes).map_err(|_| WritingError::Encoding)?;
                self.result
                    .findings
                    .extend(self.matcher.scan(path, text)?.into_iter().map(|finding| {
                        FileFinding {
                            path: path.to_owned(),
                            finding,
                        }
                    }));
                if self.result.findings.len() > MAX_FINDINGS {
                    return Err(WritingError::EvidenceLimit);
                }
                self.result.evidence.push((path.to_owned(), bytes));
            }
            _ => return Err(WritingError::Evidence),
        }
        Ok(())
    }
}
