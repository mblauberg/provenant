//! Explicit object sets: never enumerate an alternate's unrelated objects.
use super::*;
use provenant_os::exec::{CaptureFilter, FilterError};
use sha2::Digest as _;

// Bound host memory for object names independently of streamed object contents.
pub(super) const LIST_LIMIT: usize = 64 * 1024 * 1024;

pub(super) fn verify(
    git: &Path,
    view: &Path,
    cwd: &Path,
    head: GitOid,
    alternates: &[&Path],
    check: Verification,
) -> Result<()> {
    let (list_args, list_alternates) = match check.mode {
        VerificationMode::Local => (
            args(&[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname)",
            ]),
            &[][..],
        ),
        VerificationMode::Reachable | VerificationMode::Connectivity => (
            args(&[
                "rev-list",
                "--objects",
                "--no-object-names",
                &head.to_string(),
            ]),
            alternates,
        ),
    };
    let list = command_stream(
        git,
        view,
        cwd,
        &list_args,
        list_alternates,
        Some(check.deadline),
        None,
        None,
    )?;
    let mut names = BTreeSet::new();
    for name in list
        .split(|byte| *byte == b'\n')
        .filter(|name| !name.is_empty())
    {
        let name = std::str::from_utf8(name).map_err(|_| invalid())?;
        GitOid::parse(name).map_err(|_| invalid())?;
        names.insert(name.to_owned());
    }
    if matches!(check.mode, VerificationMode::Connectivity) || names.is_empty() {
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
        Some(check.deadline),
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
    let prefix = view.join("strict-check");
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
        Some(check.deadline),
        Some(batch.structured.as_bytes()),
        None,
    )?;
    let digest = std::str::from_utf8(&digest).map_err(|_| invalid())?.trim();
    GitOid::parse(digest).map_err(|_| invalid())?;
    let held = Directory::open_anchor(view)?;
    held.unlink(OsStr::new(&format!("strict-check-{digest}.idx")))?;
    let mut index = args(&["index-pack", "--strict", "--threads=1"]);
    index.push(view.join(format!("strict-check-{digest}.pack")).into());
    command_stream(
        git,
        view,
        cwd,
        &index,
        alternates,
        Some(check.deadline),
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
