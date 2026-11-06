//! Content-addressed graph text beside the checkpoint.
//!
//! A saved checkpoint carries an envelope-owned map of text references. The
//! bytes live in `graph-<id>.prompt-<hex>` files in the graph directory, so graph
//! size does not grow with brief size times node count. Inline prompts and
//! older node-local `prompt_digest` references still decode.
use super::*;
use serde_json::Value;
use std::borrow::Cow;

const DIGEST_PREFIX: &str = "sha256:";

fn prefix(id: &str) -> String {
    format!("graph-{id}.prompt-")
}

fn hex(prompt: &[u8]) -> String {
    let digest = CanonicalV1::file_digest(prompt).to_string();
    digest
        .strip_prefix(DIGEST_PREFIX)
        .unwrap_or(&digest)
        .to_owned()
}

// The envelope owns this map, outside user documents and parameters. JSON
// pointers identify only the strings we replaced, so user keys such as
// `brief_digest` remain ordinary data.
const STORED_TEXT: &str = "stored_text";

fn text_paths(value: &Value, path: &str, found: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                let escaped = key.replace('~', "~0").replace('/', "~1");
                let path = format!("{path}/{escaped}");
                if value.as_str().is_some_and(|text| {
                    !text.is_empty()
                        && (matches!(
                            key.as_str(),
                            "prompt" | "brief" | "spec" | "fragment" | "excerpt"
                        ) || text.len() > 4096)
                }) {
                    found.push(path);
                } else {
                    text_paths(value, &path, found);
                }
            }
        }
        Value::Array(array) => {
            for (index, value) in array.iter().enumerate() {
                text_paths(value, &format!("{path}/{index}"), found);
            }
        }
        _ => {}
    }
}

/// Canonical size after text becomes fixed-width references, without I/O.
pub(super) fn compacted_len<T: serde::Serialize>(value: &T) -> usize {
    let Ok(mut value) = serde_json::to_value(value) else {
        return usize::MAX;
    };
    let mut paths = Vec::new();
    text_paths(&value, "", &mut paths);
    let mut references = serde_json::Map::new();
    for path in paths {
        let Some(text) = value.pointer_mut(&path) else {
            return usize::MAX;
        };
        *text = Value::Null;
        references.insert(path, Value::String("f".repeat(64)));
    }
    // The longest enclosing pointer is the immutable execution document;
    // checkpoint pointers use the shorter `/checkpoint` prefix. Both prefixes
    // contain only ASCII characters that canonical JSON leaves unescaped.
    let prefix_reserve = references.len().saturating_mul("/execution/document".len());
    if !references.is_empty() {
        let Some(object) = value.as_object_mut() else {
            return usize::MAX;
        };
        object.insert(STORED_TEXT.into(), Value::Object(references));
    }
    // The actual envelope prefixes these local pointers with execution or
    // checkpoint paths. Reserve their escaped canonical bytes too.
    canonical_len(&value).saturating_add(prefix_reserve)
}

/// The prompt as it is persisted: the same known credential shapes that capture
/// redacts. Redaction runs before the digest, so the stored bytes and their
/// digest agree.
fn redact(prompt: &str) -> Result<String, JournalError> {
    let safe = provenant_confinement::redact::known_shapes(prompt.as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "prompt redaction failed")
    })?;
    String::from_utf8(safe)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error).into())
}

/// Move graph briefs, final blocks and excerpts into the store, redacted. Blobs are durable
/// before the checkpoint that names them is published.
pub(super) fn externalise(
    directory: &Directory,
    id: &str,
    value: &mut Value,
) -> Result<(), JournalError> {
    let mut wrote = false;
    let mut paths = Vec::new();
    text_paths(value, "", &mut paths);
    let mut references = serde_json::Map::new();
    for path in paths {
        let text = value.pointer_mut(&path).ok_or(JournalError::BadEnvelope)?;
        let prompt = redact(text.as_str().ok_or(JournalError::BadEnvelope)?)?;
        let digest = hex(prompt.as_bytes());
        let name: OsString = format!("{}{digest}", prefix(id)).into();
        if directory.metadata_at(&name, false).is_err() {
            directory
                .atomic_write(
                    &name,
                    ".graph-prompt.",
                    prompt.as_bytes(),
                    PublishMode::Replace,
                )?
                .require_durable()
                .map_err(JournalError::DurabilityUnconfirmed)?;
            wrote = true;
        }
        *text = Value::Null;
        references.insert(path, Value::String(digest));
    }
    if !references.is_empty() {
        let root = value.as_object_mut().ok_or(JournalError::BadEnvelope)?;
        if root.contains_key(STORED_TEXT) {
            return Err(JournalError::BadEnvelope);
        }
        root.insert(STORED_TEXT.into(), Value::Object(references));
    }
    if wrote {
        directory
            .sync()
            .map_err(JournalError::DurabilityUnconfirmed)?;
    }
    Ok(())
}

/// Restore inline prompts from the store. Bytes without digests are returned
/// untouched.
pub(super) fn rehydrate<'a>(
    directory: &Directory,
    id: &str,
    bytes: &'a [u8],
) -> Result<Cow<'a, [u8]>, JournalError> {
    const MARKERS: [&[u8]; 2] = [b"\"prompt_digest\"", b"\"stored_text\""];
    if !MARKERS
        .iter()
        .any(|marker| bytes.windows(marker.len()).any(|window| window == *marker))
    {
        return Ok(Cow::Borrowed(bytes));
    }
    let mut value: Value = serde_json::from_slice(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if let Some(references) = value
        .as_object_mut()
        .and_then(|root| root.remove(STORED_TEXT))
    {
        let references = references.as_object().ok_or(JournalError::BadEnvelope)?;
        for (path, digest) in references {
            let text = value.pointer_mut(path).ok_or(JournalError::BadEnvelope)?;
            if !text.is_null() {
                return Err(JournalError::BadEnvelope);
            }
            *text = Value::String(read_text(
                directory,
                id,
                digest.as_str().ok_or(JournalError::BadEnvelope)?,
            )?);
        }
    }
    // Old writers used node-local `prompt_digest`. Restrict legacy decoding
    // to schema node locations, never arbitrary parameter tables.
    for path in ["/execution/document/nodes", "/checkpoint/expansions"] {
        if let Some(Value::Array(nodes)) = value.pointer_mut(path) {
            for node in nodes {
                if path.ends_with("expansions") {
                    if let Some(Value::Array(nodes)) = node.get_mut("nodes") {
                        for node in nodes {
                            restore_legacy_node(directory, id, node)?;
                        }
                    }
                } else {
                    restore_legacy_node(directory, id, node)?;
                }
            }
        }
    }
    serde_json::to_vec(&value)
        .map(Cow::Owned)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error).into())
}

fn read_text(directory: &Directory, id: &str, digest: &str) -> Result<String, JournalError> {
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(JournalError::BadEnvelope);
    }
    let name: OsString = format!("{}{digest}", prefix(id)).into();
    let prompt = directory
        .read_stable(&name, MAX_CHECKPOINT_BYTES)
        .map_err(|error| {
            if error.is_not_found() {
                JournalError::BadEnvelope
            } else {
                error.into()
            }
        })?;
    if hex(&prompt) != digest {
        return Err(JournalError::BadDigest);
    }
    String::from_utf8(prompt).map_err(|_| JournalError::BadEnvelope)
}

fn restore_legacy_node(
    directory: &Directory,
    id: &str,
    node: &mut Value,
) -> Result<(), JournalError> {
    if let Some(object) = node.as_object_mut() {
        if let Some(Value::String(digest)) = object.get("prompt_digest") {
            let prompt = read_text(directory, id, digest)?;
            object.remove("prompt_digest");
            object.insert("prompt".into(), Value::String(prompt));
        }
        for (container, members) in [("composite", "nodes"), ("loop", "body")] {
            if let Some(Value::Array(nodes)) = object
                .get_mut(container)
                .and_then(|part| part.get_mut(members))
            {
                for child in nodes {
                    restore_legacy_node(directory, id, child)?;
                }
            }
        }
    }
    Ok(())
}

/// Remove the graph's prompt blobs. Returns whether any were removed.
pub(super) fn collect(directory: &Directory, id: &str) -> Result<bool, JournalError> {
    let prefix = prefix(id);
    let mut names = Vec::new();
    for entry in directory.entries()? {
        let name = entry?;
        if name.to_str().is_some_and(|name| {
            name.strip_prefix(&prefix)
                .is_some_and(|rest| rest.len() == 64)
        }) {
            names.push(name);
        }
    }
    for name in &names {
        match directory.unlink(name) {
            Ok(()) => {}
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(!names.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stored_prompts_are_redacted_before_their_digest_is_taken() {
        let temp = tempfile::tempdir().unwrap();
        let directory = Directory::open_anchor(temp.path()).unwrap();
        let secret = "AKIAIOSFODNN7EXAMPLE";
        let mut value = json!({"execution": {"document": {"nodes": [
            {"id": "a", "prompt": format!("deploy with {secret} now")}
        ]}}});
        externalise(&directory, "g", &mut value).unwrap();
        let digest = value[STORED_TEXT]["/execution/document/nodes/0/prompt"]
            .as_str()
            .unwrap()
            .to_owned();
        let stored = directory
            .read_stable(&OsString::from(format!("{}{digest}", prefix("g"))), 1 << 20)
            .unwrap();
        let stored = String::from_utf8(stored).unwrap();
        assert!(!stored.contains(secret), "{stored}");
        assert_eq!(hex(stored.as_bytes()), digest);
        let persisted = serde_json::to_vec(&value).unwrap();
        let restored = rehydrate(&directory, "g", &persisted).unwrap();
        let restored: Value = serde_json::from_slice(&restored).unwrap();
        assert_eq!(
            restored["execution"]["document"]["nodes"][0]["prompt"],
            stored
        );
    }

    #[test]
    fn submission_snapshots_and_composite_briefs_share_one_blob() {
        let temp = tempfile::tempdir().unwrap();
        let directory = Directory::open_anchor(temp.path()).unwrap();
        let brief = "work ".repeat(200_000);
        let original = json!({"execution":{"document":{
            "nodes":[{"id":"work","composite":{"nodes":[{"id":"child","prompt":brief}]}}],
            "provenance":{"parameters":{"brief":brief,"brief_digest":"user field","prompt_digest":"another user field","spec":brief}},
            "submission_patch":{"nodes":[{"id":"work","with":{"brief":brief},
                "composite":{"nodes":[{"id":"child","prompt":brief}]}}]}
        }},"checkpoint":{"expansions":[{"patterns":[{"parameters":{"brief":brief}}]}]}});
        let mut stored = original.clone();
        externalise(&directory, "snapshots", &mut stored).unwrap();
        let bytes = serde_json::to_vec(&stored).unwrap();
        assert!(bytes.len() < 4_096);
        assert_eq!(directory.entries().unwrap().count(), 1);
        let restored = rehydrate(&directory, "snapshots", &bytes).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&restored).unwrap(),
            original
        );
    }

    #[test]
    fn small_pending_fragments_are_stored_by_reference_and_reload() {
        let temp = tempfile::tempdir().unwrap();
        let directory = Directory::open_anchor(temp.path()).unwrap();
        let original = json!({"checkpoint":{"nodes":[{
            "id":"agent", "fragment":"{\"schema_version\":1,\"nodes\":[]}",
            "outputs":{"result":{"excerpt":"A short final result", "digest":"sha256:unchanged"}}
        }]}});
        let mut stored = original.clone();
        externalise(&directory, "fragment", &mut stored).unwrap();
        assert!(stored["checkpoint"]["nodes"][0]["fragment"].is_null());
        assert!(stored["checkpoint"]["nodes"][0]["outputs"]["result"]["excerpt"].is_null());
        assert_eq!(
            stored[STORED_TEXT]["/checkpoint/nodes/0/outputs/result/excerpt"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        assert_eq!(
            stored[STORED_TEXT]["/checkpoint/nodes/0/fragment"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
        let persisted = serde_json::to_vec(&stored).unwrap();
        let restored = rehydrate(&directory, "fragment", &persisted).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&restored).unwrap(),
            original
        );
    }
}
