use super::super::{CanonicalV1, Directory, Limits, LockMode, PublishMode, prompt_store};
use super::{
    CHECKPOINT_SCHEMA_VERSION, JournalError, MAX_CHECKPOINT_BYTES, MAX_CHECKPOINT_MEMBERS,
    StoredCheckpoint, checkpoint_digest, checkpoint_file_name, decode_checkpoint_limited,
    read_checkpoint_bytes,
};
use std::ffi::{OsStr, OsString};

fn migrate_checkpoint_bytes(bytes: &[u8]) -> Result<Vec<u8>, JournalError> {
    let value: serde_json::Value = provenant_records::decode(
        bytes,
        Limits {
            max_bytes: MAX_CHECKPOINT_BYTES.max(bytes.len() as u64),
            max_depth: 64,
            max_members: MAX_CHECKPOINT_MEMBERS,
        },
    )?;
    let version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64);
    if version == Some(CHECKPOINT_SCHEMA_VERSION) || !matches!(version, Some(2) | Some(3)) {
        // Current records pass through; unknown versions are left for the
        // strict decoder to reject with their own version error.
        return Ok(bytes.to_vec());
    }
    let mut value = value;
    {
        let checkpoint = value.get("checkpoint").ok_or(JournalError::BadEnvelope)?;
        let execution = value.get("execution").ok_or(JournalError::BadEnvelope)?;
        let digest =
            CanonicalV1::record_digest("fabric-graph-checkpoint", 2, &(checkpoint, execution))?;
        if value.get("digest").and_then(serde_json::Value::as_str)
            != Some(digest.to_string().as_str())
        {
            return Err(JournalError::BadDigest);
        }
    }
    if version == Some(2) {
        value = migrate_checkpoint_2_3(value)?;
    }
    if value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        == Some(3)
    {
        value = migrate(3, CHECKPOINT_SCHEMA_VERSION, value)?;
    }
    let mut stored: StoredCheckpoint =
        serde_json::from_value(value).map_err(|_| JournalError::BadEnvelope)?;
    stored.digest = checkpoint_digest(&stored.checkpoint, &stored.execution)?.to_string();
    let migrated = CanonicalV1::bytes(&stored)?.as_bytes().to_vec();
    decode_checkpoint_limited(
        &migrated,
        &stored.checkpoint.graph_id,
        migrated.len() as u64,
    )?;
    Ok(migrated)
}

fn migrate_checkpoint_2_3(mut value: serde_json::Value) -> Result<serde_json::Value, JournalError> {
    fn rename(object: &mut serde_json::Map<String, serde_json::Value>, old: &str, new: &str) {
        if let Some(value) = object.remove(old) {
            object.entry(new.to_owned()).or_insert(value);
        }
    }
    let nodes = value
        .pointer_mut("/checkpoint/nodes")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or(JournalError::BadEnvelope)?;
    for node in nodes {
        let node = node.as_object_mut().ok_or(JournalError::BadEnvelope)?;
        rename(node, "advance_error_code", "advance_code");
        rename(node, "advance_error", "advance_note");
        if node.get("advance_code").and_then(serde_json::Value::as_str)
            == Some("writer_declared_stop")
        {
            node.insert("advance_code".into(), "declared_stop".into());
        }
    }
    let execution = value
        .get_mut("execution")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or(JournalError::BadEnvelope)?;
    rename(execution, "advance_error", "advance_note");
    value["schema_version"] = 3.into();
    Ok(value)
}

/// Pure transform hook for the batch's next graph-store version. Store cutover
/// calls this while holding the graph store's migration lock, then publishes
/// the returned record atomically.
pub fn migrate(
    from: u64,
    to: u64,
    mut record: serde_json::Value,
) -> Result<serde_json::Value, JournalError> {
    if from == to {
        return Ok(record);
    }
    if (from, to) != (3, 4) {
        return Err(JournalError::BadEnvelope);
    }

    // A string warning is only converted on a progress record. Other fields
    // also named `warnings` (for example a result projection) are plain text
    // by contract and must keep their strings.
    fn type_progress_warnings(progress: &mut serde_json::Value) -> Result<(), JournalError> {
        let nodes = progress.as_array_mut().ok_or(JournalError::BadEnvelope)?;
        for node in nodes {
            let node = node.as_object_mut().ok_or(JournalError::BadEnvelope)?;
            let Some(warnings) = node.get_mut("warnings") else {
                continue;
            };
            let warnings = warnings.as_array_mut().ok_or(JournalError::BadEnvelope)?;
            for warning in warnings {
                if let Some(message) = warning.as_str() {
                    *warning = serde_json::json!({
                        "code": "LEGACY_WARNING",
                        "message": message,
                        "source": { "layer": "default" }
                    });
                }
            }
        }
        Ok(())
    }
    let nodes = record
        .pointer_mut("/checkpoint/nodes")
        .ok_or(JournalError::BadEnvelope)?;
    type_progress_warnings(nodes)?;
    if let Some(expansions) = record.pointer_mut("/checkpoint/expansions") {
        let expansions = expansions.as_array_mut().ok_or(JournalError::BadEnvelope)?;
        for expansion in expansions {
            if let Some(nodes) = expansion.pointer_mut("/nodes") {
                type_progress_warnings(nodes)?;
            }
            if let Some(progress) = expansion.pointer_mut("/retired/progress") {
                type_progress_warnings(progress)?;
            }
        }
    }

    fn rewrite_sealed_values(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Array(values) => {
                for value in values {
                    rewrite_sealed_values(value);
                }
            }
            serde_json::Value::Object(object) => {
                if let Some(accepted) = object
                    .get("accept_inconclusive_without_blocking")
                    .and_then(serde_json::Value::as_bool)
                {
                    object.remove("accept_inconclusive_without_blocking");
                    object.insert(
                        "accept_below".into(),
                        serde_json::Value::String(if accepted { "blocking" } else { "off" }.into()),
                    );
                }
                if let Some(isolation) = object.remove("lanes.isolation") {
                    if isolation.as_str() == Some("shared") {
                        object.insert(
                            "lanes.read_source".into(),
                            provenant_config::READ_SOURCE_LIVE.into(),
                        );
                    } else if isolation.as_str() != Some("private") {
                        object.insert("lanes.isolation".into(), isolation);
                    }
                }
                for (key, value) in object.iter_mut() {
                    if (key == "source" || key.ends_with("_source"))
                        && let Some(source) = value.as_str()
                    {
                        let replacement = match source {
                            "shipped_default" | "bundled" | "builtin" => Some("default"),
                            "dispatch" => Some("launch"),
                            _ => None,
                        };
                        if let Some(replacement) = replacement {
                            *value = serde_json::Value::String(replacement.into());
                            continue;
                        }
                    }
                    rewrite_sealed_values(value);
                }
            }
            _ => {}
        }
    }
    // Sealed settings and persisted source tags live in both the checkpoint
    // and the execution side of the envelope, so rewrite the whole record.
    rewrite_sealed_values(&mut record);

    let mut stored: StoredCheckpoint =
        serde_json::from_value(record).map_err(|_| JournalError::BadEnvelope)?;
    stored.schema_version = to;
    stored.digest = checkpoint_digest(&stored.checkpoint, &stored.execution)?.to_string();
    serde_json::to_value(stored).map_err(|_| JournalError::BadEnvelope)
}

/// One cut-over for the graph store. Each checkpoint is transformed under its
/// own journal lock. The marker makes an interrupted pass resumable, and a
/// record already at `to` is skipped without a write.
pub fn migrate_store(state_root: &std::path::Path) -> Result<Vec<String>, JournalError> {
    let graphs = state_root.join("fabric").join("graphs");
    let mut changed = Vec::new();
    let entries = match std::fs::read_dir(&graphs) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(changed),
        Err(error) => return Err(error.into()),
    };
    let marker = graphs.join(format!("migrating-{CHECKPOINT_SCHEMA_VERSION}"));
    let mut prepared = Vec::new();
    for project in entries {
        let project = project?;
        if !project.file_type()?.is_dir() {
            continue;
        }
        let Some(project_name) = project.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let directory = Directory::open_anchor(&project.path())?;
        let names: Vec<OsString> = directory.entries()?.collect::<Result<Vec<_>, _>>()?;
        for name in names {
            let Some(text) = name.to_str() else {
                continue;
            };
            let Some(id) = text
                .strip_prefix("graph-")
                .and_then(|rest| rest.strip_suffix(".json"))
            else {
                continue;
            };
            let lock_name = format!("graph-{id}.lock");
            let guard = directory.try_lock(OsStr::new(&lock_name), LockMode::Exclusive)?;
            let Some(bytes) = read_checkpoint_bytes(&directory, id)? else {
                continue;
            };
            let inline = prompt_store::rehydrate(&directory, id, &bytes)?;
            let migrated = migrate_checkpoint_bytes(&inline)?;
            if migrated.as_slice() != &*inline {
                prepared.push((
                    project.path(),
                    project_name.clone(),
                    text.to_owned(),
                    id.to_owned(),
                    guard,
                    migrated,
                ));
            }
        }
    }
    std::fs::write(&marker, b"")?;
    let result = (|| -> Result<(), JournalError> {
        for (path, project_name, text, id, _guard, migrated) in prepared {
            let directory = Directory::open_anchor(&path)?;
            let mut value: serde_json::Value =
                serde_json::from_slice(&migrated).map_err(|_| JournalError::BadEnvelope)?;
            prompt_store::externalise(&directory, &id, &mut value)?;
            let compact = CanonicalV1::bytes(&value)?;
            if compact.as_bytes().len() as u64 > MAX_CHECKPOINT_BYTES {
                return Err(JournalError::TooLarge);
            }
            directory
                .atomic_write(
                    &checkpoint_file_name(&id),
                    ".graph-migrate.",
                    compact.as_bytes(),
                    PublishMode::Replace,
                )?
                .require_durable()
                .map_err(JournalError::DurabilityUnconfirmed)?;
            changed.push(format!("{project_name}/{text}"));
        }
        Ok(())
    })();
    result?;
    std::fs::remove_file(&marker)?;
    Ok(changed)
}
