//! A graph node's upstream results as read-only files in its own directory.
//!
//! Each result is read through the run store's verified, redacted output path,
//! the one `provenant output --full` uses, never from another attempt's files.
//! The lane is granted this directory alone.
use super::{
    AttemptTarget, BinaryRunOwner, FacadeError, Factory, OutputBytes, OutputInput, OutputPart,
    OutputResult,
};
use provenant_core::{CancelToken, WaitBudget};
use provenant_fabric_runtime::graph::{
    AttemptKey, InputRequest, MaterialisedInput, MaterialisedInputs, UPSTREAM_INPUT_LIST_LIMIT,
};
use provenant_os::fs::{Directory, PublishMode};
use std::{ffi::OsStr, path::PathBuf};

fn open_or_create(parent: &Directory, name: &str) -> Result<Directory, provenant_os::fs::FsError> {
    let name = OsStr::new(name);
    match parent.open_verified_directory(name) {
        Ok(child) => Ok(child),
        Err(error) if error.is_not_found() => {
            match parent.mkdir(name, 0o700) {
                Ok(()) => parent.sync()?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error),
            }
            parent.open_verified_directory(name)
        }
        Err(error) => Err(error),
    }
}

/// The directory for one graph node, created on first use. It is named by a
/// digest of project, graph and node, which the host recomputes to grant it, so every
/// route of a node shares it: the files are the same for each.
fn attempt_directory(factory: &Factory, key: &AttemptKey) -> Option<(PathBuf, Directory)> {
    let state = factory.context.roots.state.as_path();
    let path = provenant_cli::run::graph_node_inputs_directory(
        state,
        factory.context.identity.project,
        &key.graph_id,
        &key.node_id,
    )?;
    let mut directory = Directory::open_anchor(state).ok()?;
    for component in path.strip_prefix(state).ok()?.components() {
        directory = open_or_create(&directory, &component.as_os_str().to_string_lossy()).ok()?;
    }
    Some((path, directory))
}

/// The file for one upstream: a digest of its label, so labels that differ
/// only by case do not share a file on a case-insensitive filesystem. The
/// prompt maps each label to its file.
pub(super) fn file_name(label: &str) -> String {
    let digest = provenant_records::CanonicalV1::file_digest(label.as_bytes());
    format!("{}.md", provenant_core::hex(digest.bytes()))
}

/// The longest prefix of `bytes` that is at most `max` long and whole UTF-8.
fn whole_prefix(bytes: &[u8], max: usize) -> &[u8] {
    let cut = &bytes[..bytes.len().min(max)];
    match std::str::from_utf8(cut) {
        Ok(_) => cut,
        Err(error) => &cut[..error.valid_up_to()],
    }
}

/// Read the redacted result of `attempt`, at most `max` bytes of it, and its
/// whole length. Pages stop as soon as `max` is reached.
fn read_result(
    session: &provenant_cli::run::Session,
    call: &provenant_run::Request<'_>,
    attempt: provenant_core::AttemptId,
    max: usize,
) -> Result<(Vec<u8>, u64), provenant_run::RunError> {
    let mut bytes = Vec::new();
    let mut offset = 0;
    loop {
        let chunk = session.service().output(
            attempt,
            provenant_run::ArtifactPart::Result,
            provenant_run::ByteRange {
                offset,
                max_bytes: provenant_records::run::OUTPUT_LIMIT,
            },
            call,
        )?;
        bytes.extend_from_slice(&chunk.bytes);
        if chunk.eof || bytes.len() >= max || chunk.next_offset <= offset {
            return Ok((bytes, chunk.len));
        }
        offset = chunk.next_offset;
    }
}

fn read_failure_reason(error: &FacadeError) -> String {
    let projection = error.diagnostic();
    let detail = projection.detail.as_deref().unwrap_or(&projection.code);
    let action = projection
        .next
        .as_deref()
        .unwrap_or("inspect the input source");
    provenant_records::diagnostic::safe_text(&format!(
        "its result could not be read [{}]; {action}; {detail}",
        projection.code
    ))
}

/// Page through the same verified and redacted peer output used by the CLI.
fn read_remote(
    owner: &mut BinaryRunOwner<'_>,
    target: AttemptTarget,
    budget: WaitBudget,
    cancel: &CancelToken,
    max: usize,
) -> Result<(Vec<u8>, u64), FacadeError> {
    let mut bytes = Vec::new();
    let mut offset = 0;
    let mut expected_digest = None;
    loop {
        let result = owner.output(
            OutputInput::Read {
                extra: Default::default(),
                target: target.clone(),
                part: OutputPart::Result,
                offset,
                max_bytes: (max - bytes.len()).min(provenant_records::run::OUTPUT_LIMIT as usize)
                    as u64,
            },
            budget,
            cancel,
        )?;
        let OutputResult::Chunk {
            extra,
            target: returned_target,
            part,
            offset: returned,
            next_offset,
            eof,
            digest,
            bytes: page,
            ..
        } = result
        else {
            return Err(FacadeError::StateCorrupt);
        };
        if returned_target != target
            || !matches!(part, OutputPart::Result)
            || returned != offset
            || expected_digest.is_some_and(|previous| previous != digest)
        {
            return Err(FacadeError::StateCorrupt);
        }
        expected_digest = Some(digest);
        let page = match page {
            OutputBytes::Utf8(text) => text.into_bytes(),
            OutputBytes::Base64(text) => provenant_fabric_runtime::handoff::decode_base64(&text)
                .ok_or(FacadeError::StateCorrupt)?,
        };
        if next_offset != offset + page.len() as u64 || page.len() > max - bytes.len() {
            return Err(FacadeError::StateCorrupt);
        }
        bytes.extend_from_slice(&page);
        if eof {
            return Ok((bytes, next_offset));
        }
        if next_offset <= offset {
            return Err(FacadeError::StateCorrupt);
        }
        if bytes.len() == max {
            let length = extra
                .get("length")
                .and_then(serde_json::Value::as_u64)
                .filter(|length| *length > next_offset)
                .ok_or_else(|| {
                    FacadeError::io("peer did not report the full redacted result length")
                })?;
            return Ok((bytes, length));
        }
        offset = next_offset;
    }
}

pub(super) fn materialise(
    factory: &Factory,
    budget: WaitBudget,
    cancel: &CancelToken,
    key: &AttemptKey,
    requests: &[InputRequest],
    cap: usize,
) -> MaterialisedInputs {
    let host = factory.context.identity.host;
    let unavailable = |request: &InputRequest, reason: String| MaterialisedInput::Unavailable {
        label: request.label.clone(),
        reason,
    };
    let failed = |reason: &str| MaterialisedInputs {
        directory: None,
        index: None,
        entries: requests
            .iter()
            .map(|request| unavailable(request, reason.into()))
            .collect(),
    };
    let mut owner = BinaryRunOwner {
        factory,
        allow_remote: true,
        actor: None,
        chair: None,
    };
    let session = owner.session(cancel).ok();
    if session.is_none()
        && requests
            .iter()
            .all(|request| request.inline_findings.is_none())
    {
        return failed("the run store could not be opened");
    }
    let Some((path, directory)) = attempt_directory(factory, key) else {
        return failed("the inputs directory could not be created");
    };
    let call = provenant_run::Request {
        clock: &factory.clock,
        cancel,
    };
    let mut remaining = cap;
    let mut entries = Vec::with_capacity(requests.len());
    for request in requests {
        if remaining == 0 {
            entries.push(MaterialisedInput::Capped {
                label: request.label.clone(),
            });
            continue;
        }
        let read = if let Some(findings) = &request.inline_findings {
            let text = format!(
                "```findings\n{}\n```\n",
                serde_json::json!({"schema_version":1,"findings":findings})
            );
            let bytes = text.into_bytes();
            let length = bytes.len() as u64;
            Ok((bytes, length))
        } else {
            let Some((owner_host, attempt)) = request
                .attempt_ref
                .as_deref()
                .and_then(|reference| reference.split_once('/'))
            else {
                entries.push(unavailable(
                    request,
                    "no attempt was recorded for it".into(),
                ));
                continue;
            };
            let Some(session) = &session else {
                entries.push(unavailable(
                    request,
                    "the run store could not be opened".into(),
                ));
                continue;
            };
            let Ok(attempt) = attempt.parse() else {
                entries.push(unavailable(
                    request,
                    "its attempt reference is invalid".into(),
                ));
                continue;
            };
            if owner_host == host.to_string() {
                read_result(session, &call, attempt, remaining).map_err(super::run_error)
            } else {
                match owner_host.parse() {
                    Ok(remote_host) => read_remote(
                        &mut owner,
                        AttemptTarget {
                            extra: Default::default(),
                            host: Some(remote_host),
                            attempt,
                        },
                        budget,
                        cancel,
                        remaining,
                    ),
                    Err(_) => Err(FacadeError::InvalidInput { detail: None }),
                }
            }
        };
        let (bytes, length) = match read {
            Ok(read) => read,
            Err(error) => {
                entries.push(unavailable(request, read_failure_reason(&error)));
                continue;
            }
        };
        let kept = whole_prefix(&bytes, remaining);
        let omitted = length.saturating_sub(kept.len() as u64);
        let mut content = kept.to_vec();
        if omitted > 0 {
            content.extend_from_slice(
                format!(
                    "\n[provenant: cut at the per-node input limit of {cap} bytes; {omitted} more bytes of this result are not included]\n"
                )
                .as_bytes(),
            );
        }
        let name = file_name(&request.label);
        let written = directory
            .atomic_write_with_permissions(
                OsStr::new(&name),
                ".input-",
                &content,
                0o444,
                PublishMode::Replace,
            )
            .and_then(|publication| publication.require_durable());
        if written.is_err() {
            entries.push(unavailable(
                request,
                "its file could not be written; inspect the host input directory".into(),
            ));
            continue;
        }
        // A cut ends the budget even when a UTF-8 boundary left a few bytes,
        // so later results are reported as capped rather than written as stubs.
        remaining = if omitted > 0 {
            0
        } else {
            remaining - kept.len()
        };
        entries.push(MaterialisedInput::File {
            label: request.label.clone(),
            path: path.join(&name),
            bytes: kept.len() as u64,
            omitted,
        });
    }
    // Keep the ordinary prompt listing bounded while retaining every source
    // and attempt reference in a small read-only index in the inputs directory.
    let needs_index = entries.len() > UPSTREAM_INPUT_LIST_LIMIT
        || entries
            .iter()
            .any(|entry| !matches!(entry, MaterialisedInput::File { omitted: 0, .. }));
    let index = if needs_index {
        let rows: Vec<_> = requests.iter().zip(&entries).map(|(request, entry)| {
            let (file, status, omitted, reason) = match entry {
                MaterialisedInput::File { path, omitted: 0, .. } => (Some(path), "complete", None, None),
                MaterialisedInput::File { path, omitted, .. } => (Some(path), "capped", Some(*omitted), None),
                MaterialisedInput::Capped { .. } => (None, "capped", None, None),
                MaterialisedInput::Unavailable { reason, .. } => (None, "unavailable", None, Some(reason)),
            };
            serde_json::json!({"graph_id":key.graph_id, "label":request.label, "attempt_ref":request.attempt_ref, "finding_prefix":request.finding_prefix, "path":file, "status":status, "omitted":omitted, "reason":reason})
        }).collect();
        let bytes = serde_json::to_vec(&rows).ok();
        bytes
            .and_then(|bytes| {
                directory
                    .atomic_write_with_permissions(
                        OsStr::new("upstream-index.json"),
                        ".input-",
                        &bytes,
                        0o444,
                        PublishMode::Replace,
                    )
                    .and_then(|publication| publication.require_durable())
                    .ok()
            })
            .map(|_| path.join("upstream-index.json"))
    } else {
        None
    };
    MaterialisedInputs {
        directory: Some(path),
        index,
        entries,
    }
}

#[cfg(test)]
mod diagnostic_tests {
    #[test]
    fn input_read_failure_omits_raw_error_and_host_path() {
        use super::{FacadeError, read_failure_reason};
        let error = FacadeError::Io {
            operation: format!("TOKEN_SENTINEL\u{1b}{}", "é".repeat(1000)),
            path: Some("/private/host".into()),
            delivery_limit: None,
            delivery_stage: None,
            resource: None,
            input_failure: None,
            cause: None,
        };
        let reason = read_failure_reason(&error);
        assert!(reason.contains("io"));
        assert!(!reason.contains("TOKEN_SENTINEL"));
        assert!(!reason.contains("/private/host"));
        assert!(!reason.chars().any(char::is_control));
        assert!(reason.len() <= provenant_records::diagnostic::MAX_DIAGNOSTIC_BYTES);
    }
}
