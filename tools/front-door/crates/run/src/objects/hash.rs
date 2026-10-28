//! Explicit object sets: never enumerate an alternate's unrelated objects.
use super::*;
use provenant_os::exec::{CaptureFilter, FilterError};
use sha2::Digest as _;

/// Stream names without a fixed history-size limit. Connectivity retains none.
#[derive(Default)]
pub(super) struct Names {
    pub(super) names: BTreeSet<String>,
    pending: Vec<u8>,
    discard: bool,
}
impl CaptureFilter for Names {
    fn filter(&mut self, bytes: &[u8]) -> std::result::Result<Vec<u8>, FilterError> {
        for byte in bytes {
            if *byte == b'\n' {
                let name = std::str::from_utf8(&self.pending).map_err(|_| FilterError)?;
                let name = GitOid::parse(name).map_err(|_| FilterError)?;
                if !self.discard {
                    self.names.insert(name.to_string());
                }
                self.pending.clear();
            } else {
                if self.pending.len() == 64 {
                    return Err(FilterError);
                }
                self.pending.push(*byte);
            }
        }
        Ok(Vec::new())
    }
}

pub(super) fn verify(
    git: &Path,
    view: &Path,
    cwd: &Path,
    head: GitOid,
    alternates: &[&Path],
    check: Verification<'_>,
) -> Result<()> {
    let mut names = names(git, view, cwd, head, alternates, check)?;
    // Check staged duplicates with the shared-first search order as well.
    names.extend(check.staged.iter().map(ToString::to_string));
    verify_names(git, view, cwd, alternates, check.deadline, names)
}

pub(super) fn names(
    git: &Path,
    view: &Path,
    cwd: &Path,
    head: GitOid,
    alternates: &[&Path],
    check: Verification<'_>,
) -> Result<BTreeSet<String>> {
    let mut names = Names {
        discard: matches!(check.mode, VerificationMode::Connectivity),
        ..Names::default()
    };
    let (list_args, list_alternates, input) = match check.mode {
        VerificationMode::Local => (
            args(&[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname)",
            ]),
            &[][..],
            None,
        ),
        VerificationMode::Reachable | VerificationMode::Connectivity => (
            args(&["rev-list", "--objects", "--no-object-names", "--stdin"]),
            alternates,
            Some(
                std::iter::once(format!("{head}\n"))
                    .chain(check.existing.iter().map(|tip| format!("^{tip}\n")))
                    .collect::<String>(),
            ),
        ),
    };
    command_stream(
        git,
        view,
        cwd,
        &list_args,
        list_alternates,
        Some(check.deadline),
        input.as_deref().map(str::as_bytes),
        Some(&mut names),
    )?;
    if !names.pending.is_empty() {
        return Err(invalid());
    }
    Ok(names.names)
}

fn verify_names(
    git: &Path,
    view: &Path,
    cwd: &Path,
    alternates: &[&Path],
    deadline: Deadline,
    names: BTreeSet<String>,
) -> Result<()> {
    if names.is_empty() {
        return Ok(());
    }
    let names: Vec<_> = names
        .into_iter()
        .map(|name| GitOid::parse(&name).map_err(|_| invalid()))
        .collect::<Result<_>>()?;
    let input = names
        .iter()
        .map(|name| format!("{name}\n"))
        .collect::<String>();
    let mut batch = Batch::new(names);
    command_stream(
        git,
        view,
        cwd,
        &args(&["cat-file", "--batch"]),
        alternates,
        Some(deadline),
        Some(input.as_bytes()),
        Some(&mut batch),
    )?;
    if batch.failed
        || batch.index != batch.names.len()
        || !batch.header.is_empty()
        || batch.object.is_some()
    {
        return Err(invalid());
    }
    if batch.structured.is_empty() {
        return Ok(());
    }
    // Hashes above bind every byte to its requested name. Git's strict pack
    // parser supplies format and reference-type checks for commits/trees/tags.
    // Blobs have no internal format, and stay streamed rather than being copied.
    // Rebuild the index, never trust pack-objects' index as verification evidence.
    // A separate directory also owns partial packs on every error path.
    let held = Directory::open_anchor(view)?;
    let temporary = staging_name()?;
    held.mkdir(&temporary, 0o700)?;
    let path = view.join(&temporary);
    let result = strict(
        git,
        view,
        cwd,
        alternates,
        deadline,
        &path,
        &batch.structured,
    );
    let cleanup = crate::artifacts::remove_entry_tree(&held, &temporary);
    result?;
    cleanup?;
    Ok(())
}

fn strict(
    git: &Path,
    view: &Path,
    cwd: &Path,
    alternates: &[&Path],
    deadline: Deadline,
    temporary: &Path,
    structured: &str,
) -> Result<()> {
    let prefix = temporary.join("strict-check");
    let mut pack = args(&[
        "pack-objects",
        "--no-reuse-object",
        "--no-reuse-delta",
        "--window=0",
        "--threads=1",
    ]);
    pack.push(prefix.into());
    let digest = command_stream(
        git,
        view,
        cwd,
        &pack,
        alternates,
        Some(deadline),
        Some(structured.as_bytes()),
        None,
    )?;
    let digest = std::str::from_utf8(&digest).map_err(|_| invalid())?.trim();
    GitOid::parse(digest).map_err(|_| invalid())?;
    let held = Directory::open_anchor(temporary)?;
    held.unlink(OsStr::new(&format!("strict-check-{digest}.idx")))?;
    let mut index = args(&["index-pack", "--strict", "--threads=1"]);
    index.push(temporary.join(format!("strict-check-{digest}.pack")).into());
    command_stream(
        git,
        view,
        cwd,
        &index,
        alternates,
        Some(deadline),
        None,
        None,
    )?;
    Ok(())
}

enum ObjectHash {
    Sha1(Box<sha1collisiondetection::Sha1CD>),
    Sha256(sha2::Sha256),
}
impl ObjectHash {
    fn new(name: GitOid) -> Self {
        if name.to_string().len() == 40 {
            Self::Sha1(Box::default())
        } else {
            Self::Sha256(sha2::Sha256::new())
        }
    }
    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha1(hash) => hash.update(bytes),
            Self::Sha256(hash) => hash.update(bytes),
        }
    }
    fn finish(self) -> Option<String> {
        Some(match self {
            Self::Sha1(hash) => hash
                .finalize_cd()
                .ok()?
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            Self::Sha256(hash) => hash
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        })
    }
}

struct Object {
    remaining: u64,
    hash: ObjectHash,
}
pub(super) struct Batch {
    names: Vec<GitOid>,
    index: usize,
    header: Vec<u8>,
    object: Option<Object>,
    structured: String,
    failed: bool,
}
impl Batch {
    fn new(names: Vec<GitOid>) -> Self {
        Self {
            names,
            index: 0,
            header: Vec::new(),
            object: None,
            structured: String::new(),
            failed: false,
        }
    }
    fn frame(&mut self, mut bytes: &[u8]) -> Option<()> {
        while !bytes.is_empty() {
            if let Some(object) = &mut self.object {
                if object.remaining != 0 {
                    let size = usize::try_from(object.remaining)
                        .unwrap_or(usize::MAX)
                        .min(bytes.len());
                    object.hash.update(&bytes[..size]);
                    object.remaining -= size as u64;
                    bytes = &bytes[size..];
                } else {
                    let object = self.object.take()?;
                    if bytes[0] != b'\n'
                        || object.hash.finish()? != self.names.get(self.index)?.to_string()
                    {
                        return None;
                    }
                    self.index += 1;
                    bytes = &bytes[1..];
                }
            } else {
                let byte = bytes[0];
                bytes = &bytes[1..];
                if byte != b'\n' {
                    if self.header.len() >= 128 {
                        return None;
                    }
                    self.header.push(byte);
                    continue;
                }
                let header = std::str::from_utf8(&self.header).ok()?;
                let fields: Vec<_> = header.split(' ').collect();
                let name = self.names.get(self.index)?;
                if fields.len() != 3
                    || fields[0] != name.to_string()
                    || !matches!(fields[1], "blob" | "tree" | "commit" | "tag")
                {
                    return None;
                }
                let size = fields[2].parse::<u64>().ok()?;
                let mut hash = ObjectHash::new(*name);
                hash.update(format!("{} {size}\0", fields[1]).as_bytes());
                if fields[1] != "blob" {
                    self.structured.push_str(&format!("{name}\n"));
                }
                self.object = Some(Object {
                    remaining: size,
                    hash,
                });
                self.header.clear();
            }
        }
        Some(())
    }
}
impl CaptureFilter for Batch {
    fn filter(&mut self, bytes: &[u8]) -> std::result::Result<Vec<u8>, FilterError> {
        if self.failed || self.frame(bytes).is_none() {
            self.failed = true;
            return Err(FilterError);
        }
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collision_finalisation_is_a_verification_failure() {
        // First five SHA-1 blocks of the published SHAttered PDF pair, also
        // shipped as test vectors by sha1collisiondetection 0.3.4. Their collision
        // blocks trigger detection without storing the rest of either PDF.
        let pair = [
            include_bytes!("testdata/shattered-1-prefix.bin"),
            include_bytes!("testdata/shattered-2-prefix.bin"),
        ];
        let mut digests = Vec::new();
        for prefix in pair {
            let mut ordinary = sha1collisiondetection::Sha1CD::configure()
                .detect_collisions(false)
                .build();
            ordinary.update(prefix);
            digests.push(ordinary.finalize_cd().expect("ordinary digest"));
            let mut hash = ObjectHash::Sha1(Box::default());
            hash.update(prefix);
            assert!(
                hash.finish().is_none(),
                "collision must not become a digest"
            );
            let name = digests
                .last()
                .expect("ordinary digest")
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let mut hash = ObjectHash::Sha1(Box::default());
            hash.update(prefix);
            let mut batch = Batch::new(vec![GitOid::parse(&name).expect("digest OID")]);
            batch.object = Some(Object { remaining: 0, hash });
            assert!(batch.filter(b"\n").is_err());
            assert!(batch.failed);
        }
        assert_eq!(digests[0], digests[1]);
    }
}
