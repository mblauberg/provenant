//! Publication checks use the same byte rules for files, raw blobs and commits.
use super::{CheckError, GitContext, Scan};
use provenant_confinement::utility::{GitObjectKind, GitRead, UtilityBudget, run_read_utility};
use provenant_core::GitOid;
use provenant_supervisor::exec::Termination;
use provenant_supervisor::fs::{FileKind, Metadata, SafeDir};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

const MAX_FILE: u64 = 5 * 1024 * 1024;
const LEGAL: &[&str] = &[
    "LICENSE",
    "NOTICE",
    "THIRD_PARTY_NOTICES.md",
    "LICENSES/grill-me-pocock-MIT.txt",
    "LICENSES/impeccable-APACHE-2.0.txt",
    "LICENSES/skill-optimizer-MIT.txt",
    "LICENSES/ui-ux-pro-max-MIT.txt",
];

pub(super) fn inspect(
    git: &GitContext<'_>,
    range: Option<(GitOid, GitOid)>,
) -> Result<Scan, CheckError> {
    let mut scan = Scan {
        findings: Vec::new(),
        evidence: Vec::new(),
    };
    let head_bytes = git.read(GitRead::RevParseHead)?;
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
        let bytes = git.read(GitRead::AncestorRange { base, head: tip })?;
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
    let index = git.read(GitRead::IndexEntries { paths: Vec::new() })?;
    scan.note("release:index", &index);
    let tracked = index_entries(&index)?;
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
    for (path, mode) in tracked {
        path_rules(&mut scan, &path)?;
        if mode == "120000" {
            scan.finding(
                "release.symlink",
                Path::new(&path),
                1,
                "tracked symlink is not portable",
            )?;
        }
        match read_working(git.repo.root_dir(), &path, 0)? {
            Working::Bytes(bytes) => {
                scan.note(format!("release:working:{path}"), &bytes);
                emit_matches(&mut scan, &path, &byte_matches(&bytes)?)?;
            }
            Working::Symlink(target) => {
                scan.note(format!("release:working-symlink:{path}"), &target);
                emit_matches(&mut scan, &path, &byte_matches(&target)?)?;
                scan.finding(
                    "release.symlink",
                    Path::new(&path),
                    1,
                    "tracked symlink is not portable",
                )?;
            }
            Working::Missing if LEGAL.contains(&path.as_str()) => {
                scan.note(format!("release:working-missing:{path}"), b"missing");
                scan.finding(
                    "release.legal",
                    Path::new(&path),
                    1,
                    "required legal file is missing",
                )?;
            }
            Working::Missing => {
                return Err(CheckError::Evidence("tracked working file is missing"));
            }
        }
    }
    let mut seen_commits = BTreeSet::new();
    let mut blobs = HashMap::<GitOid, Vec<Match>>::new();
    for commit in commits {
        if !seen_commits.insert(commit.to_string()) {
            continue;
        }
        let raw = git.read(GitRead::RawObject {
            kind: GitObjectKind::Commit,
            oid: commit,
        })?;
        scan.note(format!("release:commit:{commit}"), &raw);
        let commit_path = format!("@commit/{commit}");
        emit_matches(&mut scan, &commit_path, &byte_matches(&raw)?)?;
        let bytes = git.read(GitRead::AllTreeEntries { oid: commit })?;
        scan.note(format!("release:tree:{commit}"), &bytes);
        let entries = tree_entries(&bytes)?;
        // Legal-file completeness applies to publication heads, not every
        // intermediate historical snapshot in a range.
        if commit == head || range.is_some_and(|(_, tip)| tip == commit) {
            for required in LEGAL {
                if !entries
                    .iter()
                    .any(|entry| entry.path == *required && entry.regular())
                {
                    scan.finding(
                        "release.legal",
                        Path::new(required),
                        1,
                        "required legal file is missing from publication tree",
                    )?;
                }
            }
        }
        for entry in entries {
            path_rules(&mut scan, &entry.path)?;
            if entry.mode == "120000" {
                scan.finding(
                    "release.symlink",
                    Path::new(&entry.path),
                    1,
                    "publication tree contains a symlink",
                )?;
            } else if !entry.regular() {
                return Err(CheckError::Evidence(
                    "unsupported entry in publication tree",
                ));
            }
            if entry.size > MAX_FILE {
                return Err(CheckError::Evidence("publication blob exceeds 5 MiB"));
            }
            if let std::collections::hash_map::Entry::Vacant(slot) = blobs.entry(entry.oid) {
                let raw = git.read(GitRead::RawObject {
                    kind: GitObjectKind::Blob,
                    oid: entry.oid,
                })?;
                if raw.len() as u64 != entry.size {
                    return Err(CheckError::Evidence(
                        "publication blob size does not match tree",
                    ));
                }
                scan.note(format!("release:blob:{}", entry.oid), &raw);
                slot.insert(byte_matches(&raw)?);
            }
            if let Some(matches) = blobs.get(&entry.oid) {
                emit_matches(&mut scan, &entry.path, matches)?;
            }
        }
    }
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

fn is_ancestor(git: &GitContext<'_>, base: GitOid, head: GitOid) -> Result<bool, CheckError> {
    let outcome = run_read_utility(
        git.executor,
        GitRead::MergeBaseIsAncestor {
            ancestor: base,
            tip: head,
        },
        git.repo,
        git.runtime,
        UtilityBudget::bounded(Duration::from_secs(5))?,
        git.cancel,
    )?;
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
fn index_entries(bytes: &[u8]) -> Result<BTreeMap<String, String>, CheckError> {
    let mut entries = BTreeMap::new();
    for record in records(bytes)? {
        let (header, path) = record
            .split_once('\t')
            .ok_or(CheckError::Evidence("malformed Git index"))?;
        valid_path(path)?;
        let parts: Vec<_> = header.split(' ').collect();
        if parts.len() != 3
            || parts[2] != "0"
            || !matches!(parts[0], "100644" | "100755" | "120000")
        {
            return Err(CheckError::Evidence(
                "unmerged or unsupported Git index entry",
            ));
        }
        parse_oid(parts[1])?;
        if entries
            .insert(path.to_owned(), parts[0].to_owned())
            .is_some()
        {
            return Err(CheckError::Evidence("duplicate Git index entry"));
        }
    }
    Ok(entries)
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

fn path_rules(scan: &mut Scan, path: &str) -> Result<(), CheckError> {
    let private = path.split('/').any(|part| {
        matches!(
            part,
            ".agent-run" | ".worktrees" | ".pytest_cache" | ".DS_Store" | ".ssh" | ".aws"
        )
    }) || path == ".claude/settings.local.json"
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

enum Working {
    Bytes(Vec<u8>),
    Symlink(Vec<u8>),
    Missing,
}
fn read_working(dir: &dyn SafeDir, path: &str, depth: usize) -> Result<Working, CheckError> {
    if depth > 128 {
        return Err(CheckError::Evidence("tracked path exceeds depth bound"));
    }
    let (part, rest) = match path.split_once('/') {
        Some((part, rest)) => (part, Some(rest)),
        None => (path, None),
    };
    let name = OsStr::new(part);
    let before = match dir.metadata_at(name, false) {
        Ok(value) => value,
        Err(error) if error.is_not_found() => return Ok(Working::Missing),
        Err(error) => return Err(error.into()),
    };
    if before.kind == FileKind::Symlink {
        let target = dir.read_link(name)?;
        if !same_metadata(&before, &dir.metadata_at(name, false)?) {
            return Err(CheckError::Evidence("tracked symlink changed"));
        }
        return Ok(Working::Symlink(
            target.as_os_str().as_encoded_bytes().to_vec(),
        ));
    }
    if let Some(rest) = rest {
        let child = dir.open_verified_directory(name)?;
        if !before.same_identity(&child.metadata()?) {
            return Err(CheckError::Evidence("tracked directory changed"));
        }
        let result = read_working(child.as_ref(), rest, depth + 1)?;
        if !before.same_identity(&dir.metadata_at(name, false)?) {
            return Err(CheckError::Evidence("tracked directory changed"));
        }
        return Ok(result);
    }
    if before.kind != FileKind::Regular {
        return Err(CheckError::Evidence(
            "tracked working entry is not a regular file",
        ));
    }
    if before.size > MAX_FILE {
        return Err(CheckError::Evidence("tracked working file exceeds 5 MiB"));
    }
    let mut file = dir.open_read(name)?;
    if !same_metadata(&before, &file.metadata()?) {
        return Err(CheckError::Evidence("tracked file changed"));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CheckError::Evidence("tracked file could not be read"))?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(CheckError::Evidence("tracked working file exceeds 5 MiB"));
    }
    if bytes.len() as u64 != before.size
        || !same_metadata(&before, &file.metadata()?)
        || !same_metadata(&before, &dir.metadata_at(name, false)?)
    {
        return Err(CheckError::Evidence("tracked file changed"));
    }
    Ok(Working::Bytes(bytes))
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

struct ByteRule {
    id: &'static str,
    message: &'static str,
    matches: fn(&[u8]) -> bool,
}
type Match = (&'static str, usize, &'static str);
const RULES: &[ByteRule] = &[
    ByteRule {
        id: "release.home-path",
        message: "personal absolute home path",
        matches: home_path,
    },
    ByteRule {
        id: "release.private-key",
        message: "possible private key",
        matches: private_key,
    },
    ByteRule {
        id: "release.github-token",
        message: "possible GitHub token",
        matches: github_token,
    },
    ByteRule {
        id: "release.openai-key",
        message: "possible OpenAI key",
        matches: openai_key,
    },
    ByteRule {
        id: "release.anthropic-key",
        message: "possible Anthropic key",
        matches: anthropic_key,
    },
    ByteRule {
        id: "release.aws-key",
        message: "possible AWS access key",
        matches: aws_key,
    },
];
fn byte_matches(bytes: &[u8]) -> Result<Vec<Match>, CheckError> {
    if bytes.len() as u64 > MAX_FILE {
        return Err(CheckError::Evidence("release content exceeds 5 MiB"));
    }
    utf8(bytes)?;
    let mut matches = Vec::new();
    for (line, bytes) in bytes.split(|b| *b == b'\n').enumerate() {
        for rule in RULES {
            if (rule.matches)(bytes) {
                matches.push((rule.id, line + 1, rule.message));
            }
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
fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|part| part == needle)
}
fn home_path(bytes: &[u8]) -> bool {
    [b"/Users/".as_slice(), b"/home/".as_slice()]
        .iter()
        .any(|prefix| {
            bytes.windows(prefix.len()).enumerate().any(|(at, part)| {
                if part != *prefix {
                    return false;
                }
                let tail = &bytes[at + prefix.len()..];
                let count = tail
                    .iter()
                    .take_while(|b| b.is_ascii_alphanumeric() || b"._-".contains(b))
                    .count();
                count > 0 && tail.get(count) == Some(&b'/')
            })
        })
}
fn private_key(bytes: &[u8]) -> bool {
    ["", "RSA ", "EC ", "OPENSSH ", "DSA ", "ENCRYPTED "]
        .iter()
        .any(|kind| {
            let marker = ["-----BEGIN ", kind, "PRIVATE KEY-----"].concat();
            contains(bytes, marker.as_bytes())
        })
}
fn word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}
fn token(bytes: &[u8], prefix: &[u8], min: usize, allowed: fn(u8) -> bool, exact: bool) -> bool {
    bytes.windows(prefix.len()).enumerate().any(|(at, part)| {
        if part != prefix || at > 0 && word(bytes[at - 1]) {
            return false;
        }
        let tail = &bytes[at + prefix.len()..];
        let count = tail.iter().take_while(|b| allowed(**b)).count();
        count >= min && (!exact || count == min) && tail.get(count).is_none_or(|b| !word(*b))
    })
}
fn github_token(bytes: &[u8]) -> bool {
    ["gho_", "ghp_", "ghu_", "ghs_", "ghr_", "github_pat_"]
        .iter()
        .any(|prefix| token(bytes, prefix.as_bytes(), 20, word, false))
}
fn key_byte(byte: u8) -> bool {
    word(byte) || byte == b'-'
}
fn openai_key(bytes: &[u8]) -> bool {
    token(bytes, b"sk-", 20, key_byte, false)
}
fn anthropic_key(bytes: &[u8]) -> bool {
    token(bytes, b"sk-ant-", 20, key_byte, false)
}
fn aws_key(bytes: &[u8]) -> bool {
    token(
        bytes,
        b"AKIA",
        16,
        |b| b.is_ascii_uppercase() || b.is_ascii_digit(),
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_checks_the_same_raw_bytes_for_every_source() {
        let input = [
            b"note\n/Users/".as_slice(),
            b"person/private\n",
            b"ghp_",
            &[b'a'; 20],
        ]
        .concat();
        let matches = byte_matches(&input).unwrap();
        assert!(
            matches
                .iter()
                .any(|m| m.0 == "release.home-path" && m.1 == 2)
        );
        assert!(
            matches
                .iter()
                .any(|m| m.0 == "release.github-token" && m.1 == 3)
        );
        assert!(!github_token(b"ghp_too_short"));
        assert!(!home_path(b"/home/"));
        assert!(byte_matches(&[0xff]).is_err());
    }
    #[test]
    fn tree_parser_preserves_tabs_and_newlines_in_names() {
        let input = format!("100644 blob {} 2\tname\twith\nspaces\0", "a".repeat(40));
        let entries = tree_entries(input.as_bytes()).unwrap();
        assert_eq!(entries[0].path, "name\twith\nspaces");
        assert!(tree_entries(input.trim_end_matches('\0').as_bytes()).is_err());
        assert!(index_entries(format!("100644 {} 1\tname\0", "a".repeat(40)).as_bytes()).is_err());
    }
}
