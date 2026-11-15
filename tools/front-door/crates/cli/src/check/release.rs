//! Publication checks validate Git evidence and scan the built executable for sensitive bytes.
use super::{CheckError, GitContext, Scan, WorkingFile, checked_bytes, index_records, read_file};
use provenant_confinement::utility::{GitObjectKind, GitRead};
use provenant_core::GitOid;
use provenant_os::exec::Termination;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

const MAX_FILE: u64 = 5 * 1024 * 1024;
const MAX_CACHED_SYMLINK_BYTES: usize = 1024 * 1024;
const MAX_RELEASE_BLOB_BATCH_BYTES: u64 = 8 * 1024 * 1024;
const MAX_RELEASE_BLOB_BATCH_OBJECTS: usize = 512;
const PUBLIC: &[&str] = &[
    "ACKNOWLEDGEMENTS.md",
    "README.md",
    "MAINTAINING.md",
    "SECURITY.md",
    "docs/ARCHITECTURE.md",
    "docs/worktrees.md",
];
const LEGAL: &[&str] = &["LICENSE"];
pub(super) fn inspect(
    git: &GitContext<'_>,
    range: Option<(GitOid, GitOid)>,
) -> Result<Scan, CheckError> {
    let mut scan = Scan {
        findings: Vec::new(),
        evidence: Vec::new(),
    };
    let ports = provenant_config::PortTable::read_for(Some(git.repo.root_path()), git.host_state)
        .map_err(|_| CheckError::Evidence("worktree layout configuration is unreadable"))?;
    let worktree_roots = ports.worktree_layout.roots;
    let head_bytes = release_read(git, GitRead::RevParseHead)?;
    let head = oid_line(&head_bytes)?;
    scan.note("release:head", &head_bytes);
    let mut commits = vec![head];
    if let Some((base, tip)) = range {
        if base == tip {
            return Err(CheckError::Invalid("release range must be nonempty"));
        }
        if !is_ancestor(git, base, tip)? {
            return Err(CheckError::Invalid(
                "release base must be an ancestor of head",
            ));
        }
        let bytes = release_read(git, GitRead::AncestorRange { base, head: tip })?;
        scan.note(format!("release:range:{base}:{tip}"), &bytes);
        let text = utf8(&bytes)?;
        let selected = text.lines().map(parse_oid).collect::<Result<Vec<_>, _>>()?;
        if selected.is_empty() || !selected.contains(&tip) {
            return Err(CheckError::Evidence(
                "Git returned an incomplete release range",
            ));
        }
        commits.extend(selected);
    }
    let index = release_read(git, GitRead::IndexEntries { paths: Vec::new() })?;
    scan.note("release:index", &index);
    let mut tracked = BTreeMap::new();
    for entry in index_records(&index)? {
        if entry.stage != b"0"
            || entry.mode != b"100644" && entry.mode != b"100755" && entry.mode != b"120000"
        {
            return Err(CheckError::Evidence(
                "unmerged or unsupported Git index entry",
            ));
        }
        let oid = utf8(entry.oid)?;
        parse_oid(oid)?;
        let path = utf8(entry.path)?.to_owned();
        valid_path(&path)?;
        let mode = std::str::from_utf8(entry.mode)
            .map_err(|_| CheckError::Evidence("malformed Git index mode"))?
            .to_owned();
        if tracked.insert(path, (mode, oid.to_owned())).is_some() {
            return Err(CheckError::Evidence("duplicate Git index entry"));
        }
    }
    let tracked_modes = tracked
        .iter()
        .map(|(path, (mode, _))| (path.clone(), mode.clone()))
        .collect::<BTreeMap<_, _>>();
    let indexed_directories = tracked_directories(tracked_modes.keys().map(String::as_str));
    for required in LEGAL {
        if !tracked.contains_key(*required) {
            scan.finding(
                "release.legal",
                Path::new(required),
                1,
                "required legal file is not tracked",
            )?;
        }
    }
    for required in PUBLIC {
        if !tracked.contains_key(*required) {
            scan.finding(
                "release.public",
                Path::new(required),
                1,
                "required public file is not tracked",
            )?;
        }
    }
    let mut working_modes = BTreeMap::new();
    let mut working_links = Vec::new();
    for (path, (mode, oid)) in &tracked {
        if mode == "120000" {
            let target = release_read(
                git,
                GitRead::RawObject {
                    kind: GitObjectKind::Blob,
                    oid: parse_oid(oid)?,
                },
            )?;
            if target.len() as u64 > MAX_FILE {
                return Err(CheckError::Evidence("publication blob exceeds 5 MiB"));
            }
            scan.note(format!("release:index-symlink:{path}"), &target);
            if let Some(message) = symlink_problem(
                &symlink_destination(path, &target),
                &tracked_modes,
                &indexed_directories,
            ) {
                scan.finding("release.symlink", Path::new(path), 1, message)?;
            }
        }
    }
    for (path, (_mode, _)) in tracked {
        path_rules(&mut scan, &path, &worktree_roots)?;
        match read_file(git.repo.root_dir(), Path::new(&path), MAX_FILE)? {
            WorkingFile::Bytes(bytes) => {
                working_modes.insert(path.clone(), "100644".to_owned());
                scan.note(format!("release:working:{path}"), &bytes);
            }
            WorkingFile::Symlink {
                path: link_path,
                target,
            } => {
                let link_path = link_path
                    .to_str()
                    .ok_or(CheckError::Evidence("tracked symlink path is not UTF-8"))?
                    .to_owned();
                working_modes.insert(link_path.clone(), "120000".to_owned());
                scan.note(format!("release:working-symlink:{link_path}"), &target);
                if link_path != path {
                    scan.finding(
                        "release.symlink",
                        Path::new(&link_path),
                        1,
                        "tracked path is reached through a symlinked parent",
                    )?;
                }
                working_links.push((link_path.clone(), symlink_destination(&link_path, &target)));
            }
            WorkingFile::Missing => {
                let Some((rule, message)) = required_file(&path) else {
                    return Err(CheckError::Evidence("tracked working file is missing"));
                };
                scan.note(format!("release:working-missing:{path}"), b"missing");
                scan.finding(rule, Path::new(&path), 1, message)?;
            }
        }
    }
    let working_directories = tracked_directories(working_modes.keys().map(String::as_str));
    for (path, destination) in working_links {
        if let Some(message) = symlink_problem(&destination, &working_modes, &working_directories) {
            scan.finding("release.symlink", Path::new(&path), 1, message)?;
        }
    }
    let mut seen_commits = BTreeSet::new();
    let mut seen_blobs = HashSet::new();
    let mut symlink_target_cache = HashMap::<GitOid, Vec<u8>>::new();
    let mut cached_symlink_bytes = 0usize;
    for commit in commits {
        if !seen_commits.insert(commit.to_string()) {
            continue;
        }
        let raw = release_read(
            git,
            GitRead::RawObject {
                kind: GitObjectKind::Commit,
                oid: commit,
            },
        )?;
        scan.note(format!("release:commit:{commit}"), &raw);
        let commit_path = format!("@commit/{commit}");
        emit_matches(&mut scan, &commit_path, &identity_matches(&raw)?)?;
        let bytes = release_read(git, GitRead::AllTreeEntries { oid: commit })?;
        scan.note(format!("release:tree:{commit}"), &bytes);
        let entries = tree_entries(&bytes)?;
        let modes = entries
            .iter()
            .map(|entry| (entry.path.clone(), entry.mode.clone()))
            .collect::<BTreeMap<_, _>>();
        let directories = tracked_directories(modes.keys().map(String::as_str));
        // Legal-file completeness applies to publication heads, not every
        // intermediate historical snapshot in a range.
        if commit == head || range.is_some_and(|(_, tip)| tip == commit) {
            for (required, rule) in LEGAL
                .iter()
                .map(|path| (*path, "release.legal"))
                .chain(PUBLIC.iter().map(|path| (*path, "release.public")))
            {
                if !entries
                    .iter()
                    .any(|entry| entry.path == required && entry.regular())
                {
                    scan.finding(
                        rule,
                        Path::new(required),
                        1,
                        "required file is missing from publication tree",
                    )?;
                }
            }
        }
        let mut requested_blobs = HashMap::<GitOid, u64>::new();
        let mut symlinks = HashMap::<GitOid, Vec<&Entry>>::new();
        for entry in &entries {
            path_rules(&mut scan, &entry.path, &worktree_roots)?;
            if !entry.regular() && entry.mode != "120000" {
                return Err(CheckError::Evidence(
                    "unsupported entry in publication tree",
                ));
            }
            if entry.size > MAX_FILE {
                return Err(CheckError::Evidence("publication blob exceeds 5 MiB"));
            }
            if entry.mode == "120000" {
                symlinks.entry(entry.oid).or_default().push(entry);
                if !symlink_target_cache.contains_key(&entry.oid) {
                    insert_blob_size(&mut requested_blobs, entry.oid, entry.size)?;
                }
            } else if !seen_blobs.contains(&entry.oid) {
                if let Some(raw) = symlink_target_cache.get(&entry.oid) {
                    if raw.len() as u64 != entry.size {
                        return Err(CheckError::Evidence(
                            "publication blob size does not match tree",
                        ));
                    }
                    scan.note(format!("release:blob:{}", entry.oid), raw);
                    seen_blobs.insert(entry.oid);
                } else {
                    insert_blob_size(&mut requested_blobs, entry.oid, entry.size)?;
                }
            }
        }
        for (oid, links) in &symlinks {
            if let Some(raw) = symlink_target_cache.get(oid) {
                if seen_blobs.insert(*oid) {
                    scan.note(format!("release:blob:{oid}"), raw);
                }
                for entry in links {
                    if raw.len() as u64 != entry.size {
                        return Err(CheckError::Evidence(
                            "publication blob size does not match tree",
                        ));
                    }
                    if let Some(message) = symlink_problem(
                        &symlink_destination(&entry.path, raw),
                        &modes,
                        &directories,
                    ) {
                        scan.finding("release.symlink", Path::new(&entry.path), 1, message)?;
                    }
                }
            }
        }
        for batch in blob_batches(requested_blobs) {
            let raw_by_oid = read_blob_batch(git, &batch)?;
            for (oid, size) in batch {
                let raw = raw_by_oid.get(&oid).ok_or(CheckError::Evidence(
                    "Git returned an incomplete blob batch",
                ))?;
                if raw.len() as u64 != size {
                    return Err(CheckError::Evidence(
                        "publication blob size does not match tree",
                    ));
                }
                if seen_blobs.insert(oid) {
                    scan.note(format!("release:blob:{oid}"), raw);
                }
                if let Some(links) = symlinks.get(&oid) {
                    if !symlink_target_cache.contains_key(&oid)
                        && raw.len() <= MAX_CACHED_SYMLINK_BYTES - cached_symlink_bytes
                    {
                        cached_symlink_bytes += raw.len();
                        symlink_target_cache.insert(oid, raw.clone());
                    }
                    for entry in links {
                        if let Some(message) = symlink_problem(
                            &symlink_destination(&entry.path, raw),
                            &modes,
                            &directories,
                        ) {
                            scan.finding("release.symlink", Path::new(&entry.path), 1, message)?;
                        }
                    }
                }
            }
        }
    }
    let artifact = std::env::current_exe()
        .map_err(|_| CheckError::Evidence("running artifact path is unreadable"))?;
    let bytes = std::fs::read(artifact)
        .map_err(|_| CheckError::Evidence("running artifact bytes are unreadable"))?;
    scan.note("release:artifact", &bytes);
    let matches = byte_matches(&bytes);
    emit_matches(&mut scan, "@artifact/provenant", &matches)?;
    scan.findings.sort_by(|a, b| {
        (&a.rule, a.path.as_path_text(), a.line, &a.message).cmp(&(
            &b.rule,
            b.path.as_path_text(),
            b.line,
            &b.message,
        ))
    });
    scan.findings.dedup();
    Ok(scan)
}

fn release_read(git: &GitContext<'_>, operation: GitRead) -> Result<Vec<u8>, CheckError> {
    checked_bytes(git.read_release(operation)?)
}

fn insert_blob_size(
    requested: &mut HashMap<GitOid, u64>,
    oid: GitOid,
    size: u64,
) -> Result<(), CheckError> {
    if requested
        .insert(oid, size)
        .is_some_and(|previous| previous != size)
    {
        return Err(CheckError::Evidence(
            "publication blob size does not match tree",
        ));
    }
    Ok(())
}

fn blob_batches(blobs: HashMap<GitOid, u64>) -> Vec<Vec<(GitOid, u64)>> {
    let mut batches = Vec::new();
    let mut batch = Vec::new();
    let mut batch_bytes = 0_u64;
    let mut blobs = blobs.into_iter().collect::<Vec<_>>();
    blobs.sort_by_key(|(oid, _)| oid.to_string());
    for blob in blobs {
        if !batch.is_empty()
            && (batch.len() == MAX_RELEASE_BLOB_BATCH_OBJECTS
                || batch_bytes.saturating_add(blob.1) > MAX_RELEASE_BLOB_BATCH_BYTES)
        {
            batches.push(std::mem::take(&mut batch));
            batch_bytes = 0;
        }
        batch_bytes = batch_bytes.saturating_add(blob.1);
        batch.push(blob);
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    batches
}

fn read_blob_batch(
    git: &GitContext<'_>,
    expected: &[(GitOid, u64)],
) -> Result<HashMap<GitOid, Vec<u8>>, CheckError> {
    let bytes = release_read(
        git,
        GitRead::BlobBatch {
            oids: expected.iter().map(|(oid, _)| *oid).collect(),
        },
    )?;
    parse_blob_batch(&bytes, expected)
}

fn parse_blob_batch(
    bytes: &[u8],
    expected: &[(GitOid, u64)],
) -> Result<HashMap<GitOid, Vec<u8>>, CheckError> {
    let mut cursor = 0_usize;
    let mut blobs = HashMap::with_capacity(expected.len());
    for (oid, expected_size) in expected {
        let header_end = cursor
            + bytes[cursor..]
                .iter()
                .position(|byte| *byte == b'\n')
                .ok_or(CheckError::Evidence("malformed Git blob batch"))?;
        let header = utf8(&bytes[cursor..header_end])?;
        let mut fields = header.split_ascii_whitespace();
        let expected_oid = oid.to_string();
        if fields.next() != Some(expected_oid.as_str()) || fields.next() != Some("blob") {
            return Err(CheckError::Evidence(
                "Git returned an unexpected blob batch entry",
            ));
        }
        let size = fields
            .next()
            .and_then(|field| field.parse::<u64>().ok())
            .ok_or(CheckError::Evidence("malformed Git blob batch size"))?;
        if fields.next().is_some() || size != *expected_size || size > MAX_FILE {
            return Err(CheckError::Evidence(
                "Git blob batch size does not match tree",
            ));
        }
        cursor = header_end + 1;
        let length = usize::try_from(size)
            .map_err(|_| CheckError::Evidence("Git blob batch size is not representable"))?;
        let data_end = cursor
            .checked_add(length)
            .ok_or(CheckError::Evidence("malformed Git blob batch"))?;
        if bytes.get(data_end) != Some(&b'\n') {
            return Err(CheckError::Evidence("truncated Git blob batch"));
        }
        if blobs
            .insert(*oid, bytes[cursor..data_end].to_vec())
            .is_some()
        {
            return Err(CheckError::Evidence("duplicate Git blob batch entry"));
        }
        cursor = data_end + 1;
    }
    if cursor != bytes.len() {
        return Err(CheckError::Evidence(
            "Git returned trailing blob batch data",
        ));
    }
    Ok(blobs)
}

fn is_ancestor(git: &GitContext<'_>, base: GitOid, head: GitOid) -> Result<bool, CheckError> {
    let outcome = git.read_release(GitRead::MergeBaseIsAncestor {
        ancestor: base,
        tip: head,
    })?;
    match outcome.termination {
        Termination::Exit(0) => Ok(true),
        Termination::Exit(1) => Ok(false),
        _ => Err(CheckError::Evidence(
            "Git could not validate release ancestry",
        )),
    }
}

fn utf8(bytes: &[u8]) -> Result<&str, CheckError> {
    std::str::from_utf8(bytes).map_err(|_| CheckError::Evidence("release evidence is not UTF-8"))
}
fn parse_oid(text: &str) -> Result<GitOid, CheckError> {
    GitOid::parse(text).map_err(|_| CheckError::Evidence("Git returned a malformed object ID"))
}
fn oid_line(bytes: &[u8]) -> Result<GitOid, CheckError> {
    let text = utf8(bytes)?;
    parse_oid(text.strip_suffix('\n').unwrap_or(text))
}

fn valid_path(path: &str) -> Result<(), CheckError> {
    if path.is_empty()
        || path.starts_with('/')
        || path
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return Err(CheckError::Evidence("Git returned an invalid tracked path"));
    }
    Ok(())
}
fn records(bytes: &[u8]) -> Result<impl Iterator<Item = &str>, CheckError> {
    if !bytes.is_empty() && !bytes.ends_with(&[0]) {
        return Err(CheckError::Evidence(
            "Git returned unterminated path records",
        ));
    }
    Ok(utf8(bytes)?.split_terminator('\0'))
}
fn required_file(path: &str) -> Option<(&'static str, &'static str)> {
    if LEGAL.contains(&path) {
        Some(("release.legal", "required legal file is missing"))
    } else if PUBLIC.contains(&path) {
        Some(("release.public", "required public file is missing"))
    } else {
        None
    }
}
#[derive(Clone)]
struct ResolvedSymlinkTarget {
    path: String,
    traversed_directories: Vec<String>,
}
type SymlinkDestination = Result<ResolvedSymlinkTarget, &'static str>;

/// Resolve a Git symlink target lexically. This deliberately does not consult
/// the host filesystem, so historical trees are judged only against their own
/// tracked paths.
fn symlink_destination(path: &str, target: &[u8]) -> SymlinkDestination {
    let target = std::str::from_utf8(target).map_err(|_| "symlink target is not valid UTF-8")?;
    if target.is_empty() || target.contains('\0') {
        return Err("symlink target is empty or malformed");
    }
    let drive_prefix = |value: &str| {
        value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphabetic())
            && value.as_bytes().get(1) == Some(&b':')
    };
    let mut portable_target = target;
    while let Some(rest) = portable_target.strip_prefix("./") {
        portable_target = rest;
    }
    if target.starts_with('/') || target.starts_with('\\') || drive_prefix(portable_target) {
        return Err("symlink target must be relative");
    }
    if target.contains('\\') {
        return Err("symlink target contains a nonportable path separator");
    }
    if target.split('/').any(|component| component.contains(':')) {
        return Err("symlink target contains a nonportable path component");
    }
    if target.ends_with('/') {
        return Err("symlink target must name a tracked regular file");
    }

    let mut resolved = path
        .rsplit_once('/')
        .map(|(parent, _)| parent.split('/').map(str::to_owned).collect::<Vec<_>>())
        .unwrap_or_default();
    let mut traversed_directories = (1..=resolved.len())
        .map(|length| resolved[..length].join("/"))
        .collect::<Vec<_>>();
    let components = target.split('/').collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        match *component {
            "" | "." => {}
            ".." => {
                if resolved.pop().is_none() {
                    return Err("symlink target escapes the repository tree");
                }
            }
            value => resolved.push(value.to_owned()),
        }
        if !matches!(*component, "" | "." | "..") && index + 1 < components.len() {
            traversed_directories.push(resolved.join("/"));
        }
    }
    if resolved.is_empty() {
        return Err("symlink target must name a tracked regular file");
    }
    Ok(ResolvedSymlinkTarget {
        path: resolved.join("/"),
        traversed_directories,
    })
}

fn symlink_problem(
    destination: &SymlinkDestination,
    tracked_modes: &BTreeMap<String, String>,
    tracked_directories: &BTreeSet<String>,
) -> Option<&'static str> {
    match destination {
        Err(message) => Some(*message),
        Ok(target) => {
            for directory in &target.traversed_directories {
                match tracked_modes.get(directory).map(String::as_str) {
                    Some("120000") => {
                        return Some(
                            "symlink target traverses another symlink; chains and cycles are not portable",
                        );
                    }
                    Some(_) => return Some("symlink target traverses a tracked non-directory"),
                    None if !has_tracked_descendant(directory, tracked_directories) => {
                        return Some("symlink target traverses an untracked directory");
                    }
                    None => {}
                }
            }
            match tracked_modes.get(&target.path).map(String::as_str) {
                Some("100644" | "100755") => None,
                Some("120000") => {
                    Some("symlink target is another symlink; chains and cycles are not portable")
                }
                Some(_) => Some("symlink target is not a regular tracked file"),
                None => Some("symlink target is not tracked in this tree"),
            }
        }
    }
}

fn tracked_directories<'a>(paths: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
    let mut directories = BTreeSet::new();
    for path in paths {
        for (separator, _) in path.match_indices('/') {
            directories.insert(path[..separator].to_owned());
        }
    }
    directories
}

fn has_tracked_descendant(path: &str, tracked_directories: &BTreeSet<String>) -> bool {
    tracked_directories.contains(path)
}

struct Entry {
    mode: String,
    oid: GitOid,
    size: u64,
    path: String,
}
impl Entry {
    fn regular(&self) -> bool {
        matches!(self.mode.as_str(), "100644" | "100755")
    }
}
fn tree_entries(bytes: &[u8]) -> Result<Vec<Entry>, CheckError> {
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    for record in records(bytes)? {
        let (header, path) = record
            .split_once('\t')
            .ok_or(CheckError::Evidence("malformed Git tree"))?;
        valid_path(path)?;
        let parts: Vec<_> = header.split_ascii_whitespace().collect();
        if parts.len() != 4 || parts[1] != "blob" || !seen.insert(path.to_owned()) {
            return Err(CheckError::Evidence(
                "unsupported or duplicate Git tree entry",
            ));
        }
        entries.push(Entry {
            mode: parts[0].to_owned(),
            oid: parse_oid(parts[2])?,
            size: parts[3]
                .parse()
                .map_err(|_| CheckError::Evidence("malformed Git blob size"))?,
            path: path.to_owned(),
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

fn path_rules(scan: &mut Scan, path: &str, worktree_roots: &[PathBuf]) -> Result<(), CheckError> {
    let relative = Path::new(path);
    let private = worktree_roots.iter().any(|root| relative.starts_with(root))
        || path.split('/').any(|part| {
            matches!(
                part,
                ".agent-run" | ".pytest_cache" | ".DS_Store" | ".ssh" | ".aws"
            )
        })
        || path == ".claude/settings.local.json"
        || path == "plugins/marketplace.json"
        || path.split('/').any(|part| {
            part == ".env"
                || part.starts_with(".env.") && !matches!(part, ".env.example" | ".env.template")
        });
    if private {
        scan.finding(
            "release.private-path",
            Path::new(path),
            1,
            "private path must not be published",
        )?;
    }
    Ok(())
}

type Match = (&'static str, usize, &'static str);
// Metadata and byte patterns share one definition so matcher indices cannot drift.
const BYTE_RULES: &[(&str, &str, &str)] = &[
    (
        "release.home-path",
        "personal absolute home path",
        r"(?-u)(?:^|[^A-Za-z0-9._-])/(?:Users|home)/[A-Za-z0-9._-]+/",
    ),
    // Regex syntax separates the marker parts in the executable being scanned.
    (
        "release.private-key",
        "possible private key",
        r"(?-u)-----BEGIN (?:RSA |EC |OPENSSH |DSA |ENCRYPTED )?PRIVATE KEY-----",
    ),
    (
        "release.github-token",
        "possible GitHub token",
        r"(?-u)(?:^|[^A-Za-z0-9_])(?:gho_|ghp_|ghu_|ghs_|ghr_|github_pat_)[A-Za-z0-9_]{20,}(?:$|[^A-Za-z0-9_])",
    ),
    (
        "release.openai-key",
        "possible OpenAI key",
        r"(?-u)(?:^|[^A-Za-z0-9_])sk-[A-Za-z0-9_-]{20,}(?:$|[^A-Za-z0-9_])",
    ),
    (
        "release.anthropic-key",
        "possible Anthropic key",
        r"(?-u)(?:^|[^A-Za-z0-9_])sk-ant-[A-Za-z0-9_-]{20,}(?:$|[^A-Za-z0-9_])",
    ),
    (
        "release.aws-key",
        "possible AWS access key",
        r"(?-u)(?:^|[^A-Za-z0-9_])AKIA[A-Z0-9]{16}(?:$|[^A-Za-z0-9_])",
    ),
];
#[allow(clippy::expect_used)] // Compiled patterns; the byte-rule tests build this set.
static BYTE_MATCHERS: std::sync::LazyLock<regex::bytes::RegexSet> =
    std::sync::LazyLock::new(|| {
        regex::bytes::RegexSet::new(BYTE_RULES.iter().map(|(_, _, pattern)| *pattern))
            .expect("release byte rules are valid")
    });
fn identity_email(identity: &str) -> Option<&str> {
    let open = identity.rfind('<')?;
    let close = identity[open + 1..].find('>')? + open + 1;
    Some(&identity[open + 1..close])
}
fn personal_email(email: &str) -> bool {
    let Some((_, domain)) = email.rsplit_once('@') else {
        return false;
    };
    let domain = domain.to_ascii_lowercase();
    ["gmail", "outlook", "hotmail", "icloud", "yahoo"]
        .into_iter()
        .any(|provider| {
            domain.strip_prefix(provider).is_some_and(|rest| {
                rest.strip_prefix('.').is_some_and(|suffix| {
                    !suffix.is_empty()
                        && suffix.bytes().all(|b| b.is_ascii_alphabetic() || b == b'.')
                })
            })
        })
}
fn identity_matches(raw: &[u8]) -> Result<Vec<Match>, CheckError> {
    let text = utf8(raw)?;
    let mut matches = Vec::new();
    for (line_number, line) in text.lines().take_while(|line| !line.is_empty()).enumerate() {
        let (role, identity) = if let Some(identity) = line.strip_prefix("author ") {
            ("author", identity)
        } else if let Some(identity) = line.strip_prefix("committer ") {
            ("committer", identity)
        } else {
            continue;
        };
        if identity_email(identity).is_some_and(personal_email) {
            matches.push((
                "release.personal-email",
                line_number + 1,
                if role == "author" {
                    "commit author exposes a personal email"
                } else {
                    "commit committer exposes a personal email"
                },
            ));
        }
    }
    Ok(matches)
}
fn emit_matches(scan: &mut Scan, path: &str, matches: &[Match]) -> Result<(), CheckError> {
    for &(rule, line, message) in matches {
        scan.finding(rule, Path::new(path), line, message)?;
    }
    Ok(())
}
fn byte_matches(bytes: &[u8]) -> Vec<Match> {
    BYTE_MATCHERS
        .matches(bytes)
        .iter()
        .map(|index| {
            let (rule, message, _) = BYTE_RULES[index];
            (rule, 1, message)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_rules_match_sensitive_values_in_binary_data() {
        let input = [
            b"note\n/Users/".as_slice(),
            b"person/private\n",
            b"ghp_",
            &[b'a'; 20],
        ]
        .concat();
        let matches = byte_matches(&input);
        assert!(matches.iter().any(|m| m.0 == "release.home-path"));
        assert!(matches.iter().any(|m| m.0 == "release.github-token"));
        let has_rule =
            |bytes: &[u8], rule: &str| byte_matches(bytes).iter().any(|finding| finding.0 == rule);
        assert!(!has_rule(b"ghp_too_short", "release.github-token"));
        assert!(!has_rule(b"/home/", "release.home-path"));
        assert!(!has_rule(
            b"(subpath \"/fixture/home/.claude/projects\")",
            "release.home-path"
        ));
        assert!(has_rule(
            b"(subpath \"/home/person/.claude\")",
            "release.home-path"
        ));
        assert!(has_rule(b"/Users/person/x", "release.home-path"));
        assert!(!has_rule(
            b"xghp_aaaaaaaaaaaaaaaaaaaa",
            "release.github-token"
        ));
        let private_key = ["-----BEGIN ", "RSA ", "PRIVATE KEY-----"].concat();
        assert!(has_rule(private_key.as_bytes(), "release.private-key"));
        assert!(byte_matches(&[0xff]).is_empty());
        let binary = [
            &[0x89, b'P', b'N', b'G', 0xff, b'\n'][..],
            b"ghp_",
            &[b'a'; 20],
        ]
        .concat();
        assert!(
            byte_matches(&binary)
                .iter()
                .any(|m| m.0 == "release.github-token")
        );
    }

    #[test]
    fn binary_release_scan_distinguishes_token_markers_from_token_values() {
        // This models the NUL-separated production marker tables.
        let marker_table = b"ghp_\0gho_\0ghu_\0ghs_\0ghr_\0github_pat_\0";
        assert!(
            !byte_matches(marker_table)
                .iter()
                .any(|finding| finding.0 == "release.github-token")
        );

        let token = b"ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        assert!(
            byte_matches(token)
                .iter()
                .any(|finding| finding.0 == "release.github-token")
        );
    }
    #[test]
    fn tree_and_index_parsers_preserve_tabs_and_newlines_in_names() {
        let input = format!("100644 blob {} 2\tname\twith\nspaces\0", "a".repeat(40));
        let entries = tree_entries(input.as_bytes()).unwrap();
        assert_eq!(entries[0].path, "name\twith\nspaces");
        assert!(tree_entries(input.trim_end_matches('\0').as_bytes()).is_err());
        let index = format!("100644 {} 0\tname\twith\nspaces\0", "a".repeat(40));
        let entries = index_records(index.as_bytes()).unwrap();
        assert_eq!(entries[0].path, b"name\twith\nspaces");
        assert!(index_records(index.trim_end_matches('\0').as_bytes()).is_err());
        let unmerged = format!("100644 {} 1\tname\0", "a".repeat(40));
        assert_eq!(index_records(unmerged.as_bytes()).unwrap()[0].stage, b"1");
    }

    #[test]
    fn commit_identity_rule_matches_the_previous_personal_provider_set() {
        let raw = b"tree 0000000000000000000000000000000000000000\nauthor Fixture <Person@GMAIL.com> 1 +0000\ncommitter Fixture <person@gmail.com> 1 +0000\n\nmessage\n";
        let found = identity_matches(raw).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(
            found[0],
            (
                "release.personal-email",
                2,
                "commit author exposes a personal email"
            )
        );
        assert_eq!(
            found[1],
            (
                "release.personal-email",
                3,
                "commit committer exposes a personal email"
            )
        );
        for email in [
            "person@gmail.example",
            "person@outlook.co.uk",
            "person@hotmail.com",
            "person@icloud.com",
            "person@yahoo.co.nz",
        ] {
            assert!(personal_email(email));
        }
        for email in [
            "person@notgmail.example",
            "person@company.com",
            "not-an-email",
        ] {
            assert!(!personal_email(email));
        }
    }
}
