//! Publication checks use the same byte rules for files, raw blobs and commits.
use super::{CheckError, GitContext, Scan, WorkingFile, checked_bytes, index_records, read_file};
use provenant_confinement::utility::{GitObjectKind, GitRead};
use provenant_core::GitOid;
use provenant_os::exec::Termination;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

const MAX_FILE: u64 = 5 * 1024 * 1024;
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
        if tracked.insert(path, mode).is_some() {
            return Err(CheckError::Evidence("duplicate Git index entry"));
        }
    }
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
    for (path, mode) in tracked {
        path_rules(&mut scan, &path)?;
        let mut symlink = mode == "120000";
        match read_file(git.repo.root_dir(), Path::new(&path), MAX_FILE)? {
            WorkingFile::Bytes(bytes) => {
                scan.note(format!("release:working:{path}"), &bytes);
                emit_matches(&mut scan, &path, &byte_matches(&bytes)?)?;
            }
            WorkingFile::Symlink(target) => {
                symlink = true;
                scan.note(format!("release:working-symlink:{path}"), &target);
                emit_matches(&mut scan, &path, &byte_matches(&target)?)?;
            }
            WorkingFile::Missing => {
                let Some((rule, message)) = required_file(&path) else {
                    return Err(CheckError::Evidence("tracked working file is missing"));
                };
                scan.note(format!("release:working-missing:{path}"), b"missing");
                scan.finding(rule, Path::new(&path), 1, message)?;
            }
        }
        if symlink {
            scan.finding(
                "release.symlink",
                Path::new(&path),
                1,
                "tracked symlink is not portable",
            )?;
        }
    }
    let mut seen_commits = BTreeSet::new();
    let mut blobs = HashMap::<GitOid, Vec<Match>>::new();
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
        emit_matches(&mut scan, &commit_path, &byte_matches(&raw)?)?;
        emit_matches(&mut scan, &commit_path, &identity_matches(&raw)?)?;
        let bytes = release_read(git, GitRead::AllTreeEntries { oid: commit })?;
        scan.note(format!("release:tree:{commit}"), &bytes);
        let entries = tree_entries(&bytes)?;
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
        for entry in entries {
            path_rules(&mut scan, &entry.path)?;
            if entry.mode == "120000" {
                scan.finding(
                    "release.symlink",
                    Path::new(&entry.path),
                    1,
                    "tracked symlink is not portable",
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
                let raw = release_read(
                    git,
                    GitRead::RawObject {
                        kind: GitObjectKind::Blob,
                        oid: entry.oid,
                    },
                )?;
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

fn release_read(git: &GitContext<'_>, operation: GitRead) -> Result<Vec<u8>, CheckError> {
    checked_bytes(git.read_release(operation)?)
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

type Match = (&'static str, usize, &'static str);
type ByteRule = (&'static str, &'static str, fn(&[u8]) -> bool);
const RULES: &[ByteRule] = &[
    (
        "release.home-path",
        "personal absolute home path",
        home_path,
    ),
    ("release.private-key", "possible private key", private_key),
    (
        "release.github-token",
        "possible GitHub token",
        github_token,
    ),
    ("release.openai-key", "possible OpenAI key", openai_key),
    (
        "release.anthropic-key",
        "possible Anthropic key",
        anthropic_key,
    ),
    ("release.aws-key", "possible AWS access key", aws_key),
];
fn byte_matches(bytes: &[u8]) -> Result<Vec<Match>, CheckError> {
    if bytes.len() as u64 > MAX_FILE {
        return Err(CheckError::Evidence("release content exceeds 5 MiB"));
    }
    // Rules match bytes, so binary content is scanned rather than refused.
    let mut matches = Vec::new();
    for (line, bytes) in bytes.split(|b| *b == b'\n').enumerate() {
        for &(id, message, matches_rule) in RULES {
            if matches_rule(bytes) {
                matches.push((id, line + 1, message));
            }
        }
    }
    Ok(matches)
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
        assert!(byte_matches(&[0xff]).unwrap().is_empty());
        let binary = [
            &[0x89, b'P', b'N', b'G', 0xff, b'\n'][..],
            b"ghp_",
            &[b'a'; 20],
        ]
        .concat();
        assert!(
            byte_matches(&binary)
                .unwrap()
                .iter()
                .any(|m| m.0 == "release.github-token" && m.1 == 2)
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
