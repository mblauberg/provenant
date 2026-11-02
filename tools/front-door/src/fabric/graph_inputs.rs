//! A graph node's upstream results as read-only files in its own directory.
//!
//! Each result is read through the run store's verified, redacted output path,
//! the one `provenant output --full` uses, never from another attempt's files.
//! The lane is granted this directory alone.
use super::{BinaryRunOwner, Factory};
use provenant_core::CancelToken;
use provenant_fabric_runtime::graph::{
    AttemptKey, InputRequest, MaterialisedInput, MaterialisedInputs,
};
use provenant_os::fs::{Directory, PublishMode};
use std::{ffi::OsStr, path::PathBuf};

/// Why a result on another host is not offered.
fn remote_reason(host: &str) -> String {
    format!("on host {host}; remote materialisation tracked in #2665")
}

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

/// The directory for one attempt key, created on first use. Graph and node
/// identifiers are path-safe, so the key names it directly.
fn attempt_directory(
    factory: &Factory,
    key: &AttemptKey,
) -> Result<(PathBuf, Directory), provenant_os::fs::FsError> {
    let base = provenant_cli::run::graph_inputs_directory(factory.context.roots.state.as_path());
    let leaf = format!("{}-{}", key.node_id, key.route_index);
    let relative = [
        factory.context.identity.project.to_string(),
        key.graph_id.clone(),
        leaf,
    ];
    // `fabric` and `graph-inputs` are the base's last two components.
    let mut directory = Directory::open_anchor(factory.context.roots.state.as_path())?;
    for name in ["fabric", "graph-inputs"] {
        directory = open_or_create(&directory, name)?;
    }
    let mut path = base;
    for name in &relative {
        directory = open_or_create(&directory, name)?;
        path.push(name);
    }
    Ok((path, directory))
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

pub(super) fn materialise(
    factory: &Factory,
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
    // Only results on this host are read; the rest are named with the reason.
    let local = |request: &InputRequest| {
        request
            .attempt_ref
            .as_deref()
            .and_then(|reference| reference.split_once('/'))
            .is_some_and(|(owner, _)| owner == host.to_string())
    };
    if !requests.iter().any(local) {
        return MaterialisedInputs {
            directory: None,
            entries: requests
                .iter()
                .map(|request| match request.attempt_ref.as_deref() {
                    Some(reference) => unavailable(
                        request,
                        remote_reason(reference.split_once('/').map_or(reference, |(h, _)| h)),
                    ),
                    None => unavailable(request, "no attempt was recorded for it".into()),
                })
                .collect(),
        };
    }
    let failed = |reason: &str| MaterialisedInputs {
        directory: None,
        entries: requests
            .iter()
            .map(|request| unavailable(request, reason.into()))
            .collect(),
    };
    let owner = BinaryRunOwner {
        factory,
        allow_remote: false,
        actor: None,
        chair: None,
    };
    let Ok(session) = owner.session(cancel) else {
        return failed("the run store could not be opened");
    };
    let Ok((path, directory)) = attempt_directory(factory, key) else {
        return failed("the inputs directory could not be created");
    };
    let call = owner.call(cancel);
    let mut remaining = cap;
    let mut entries = Vec::with_capacity(requests.len());
    for request in requests {
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
        if owner_host != host.to_string() {
            entries.push(unavailable(request, remote_reason(owner_host)));
            continue;
        }
        if remaining == 0 {
            entries.push(MaterialisedInput::Capped {
                label: request.label.clone(),
            });
            continue;
        }
        let Ok(attempt) = attempt.parse() else {
            entries.push(unavailable(
                request,
                "its attempt reference is invalid".into(),
            ));
            continue;
        };
        let (bytes, length) = match read_result(&session, &call, attempt, remaining) {
            Ok(read) => read,
            Err(error) => {
                entries.push(unavailable(
                    request,
                    format!("its result could not be read: {error}"),
                ));
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
        let name = format!("{}.md", request.label);
        let written = directory
            .atomic_write_with_permissions(
                OsStr::new(&name),
                ".input-",
                &content,
                0o444,
                PublishMode::Replace,
            )
            .and_then(|publication| publication.require_durable());
        if let Err(error) = written {
            entries.push(unavailable(
                request,
                format!("its file could not be written: {error}"),
            ));
            continue;
        }
        remaining -= kept.len();
        entries.push(MaterialisedInput::File {
            label: request.label.clone(),
            path: path.join(&name),
            bytes: kept.len() as u64,
            omitted,
        });
    }
    MaterialisedInputs {
        directory: Some(path),
        entries,
    }
}
