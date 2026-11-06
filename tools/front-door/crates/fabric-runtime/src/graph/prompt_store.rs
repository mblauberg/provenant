//! Content-addressed node prompts beside the checkpoint.
//!
//! A saved checkpoint carries `prompt_digest` in place of `prompt`. The prompt
//! bytes live in `graph-<id>.prompt-<hex>` files in the graph directory, so graph
//! size does not grow with brief size times node count. Checkpoints written
//! with inline prompts still decode.
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

fn nodes_mut(value: &mut Value) -> Vec<&mut Value> {
    let mut found = Vec::new();
    let Some(root) = value.as_object_mut() else {
        return found;
    };
    for (key, part) in root.iter_mut() {
        match (key.as_str(), part) {
            ("execution", Value::Object(execution)) => {
                if let Some(Value::Object(document)) = execution.get_mut("document")
                    && let Some(Value::Array(nodes)) = document.get_mut("nodes")
                {
                    found.extend(nodes.iter_mut());
                }
            }
            ("checkpoint", Value::Object(checkpoint)) => {
                if let Some(Value::Array(expansions)) = checkpoint.get_mut("expansions") {
                    for expansion in expansions {
                        if let Some(Value::Array(nodes)) = expansion.get_mut("nodes") {
                            found.extend(nodes.iter_mut());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    found
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

/// Move every non-empty node prompt into the store, redacted. Blobs are durable
/// before the checkpoint that names them is published.
pub(super) fn externalise(
    directory: &Directory,
    id: &str,
    value: &mut Value,
) -> Result<(), JournalError> {
    let mut wrote = false;
    for node in nodes_mut(value) {
        let Some(object) = node.as_object_mut() else {
            continue;
        };
        let Some(Value::String(prompt)) = object.get("prompt") else {
            continue;
        };
        if prompt.is_empty() {
            continue;
        }
        let prompt = redact(prompt)?;
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
        object.remove("prompt");
        object.insert("prompt_digest".into(), Value::String(digest));
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
    const MARKER: &[u8] = b"\"prompt_digest\"";
    if !bytes.windows(MARKER.len()).any(|window| window == MARKER) {
        return Ok(Cow::Borrowed(bytes));
    }
    let mut value: Value = serde_json::from_slice(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    for node in nodes_mut(&mut value) {
        let Some(object) = node.as_object_mut() else {
            continue;
        };
        let Some(Value::String(digest)) = object.get("prompt_digest") else {
            continue;
        };
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
        if hex(&prompt) != *digest {
            return Err(JournalError::BadDigest);
        }
        let prompt = String::from_utf8(prompt).map_err(|_| JournalError::BadEnvelope)?;
        object.remove("prompt_digest");
        object.insert("prompt".into(), Value::String(prompt));
    }
    serde_json::to_vec(&value)
        .map(Cow::Owned)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error).into())
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
        let digest = value["execution"]["document"]["nodes"][0]["prompt_digest"]
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
}
