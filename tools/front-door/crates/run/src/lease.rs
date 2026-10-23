//! Single-driver lease policy, safe filesystem adapter and typed CLI boundary.
use crate::pending::{self, python_decimal_digit};
use jiff::Timestamp;
use provenant_core::py::{
    python_fromisoformat, python_json_eq, python_repr, python_str, python_strip, python_truth,
    DateTimeError,
};
use provenant_records::{
    checked_json::{self, DecodeError, NumberOutOfRange},
    python_json::{dumps, DumpsOptions, EncodeError},
};
use provenant_supervisor::fs::{LocalFs, SafeDir, SafeFile, SafeFs};
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
use thiserror::Error;

const TEMPORARY_ATTEMPTS: usize = 128;
const MAX_RECORD_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Acquire,
    Renew,
    Transfer,
    Takeover,
    Release,
}

impl Action {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "acquire" => Some(Self::Acquire),
            "renew" => Some(Self::Renew),
            "transfer" => Some(Self::Transfer),
            "takeover" => Some(Self::Takeover),
            "release" => Some(Self::Release),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct Request<'a> {
    pub action: Action,
    pub holder: &'a str,
    pub ttl_seconds: Number,
    pub expected_generation: Option<Number>,
    pub target: &'a str,
    pub handoff: &'a str,
}

/// The record keeps the source's permissive JSON values for retained fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lease {
    pub schema_version: i64,
    pub status: String,
    pub holder: Value,
    pub previous_holder: Value,
    pub generation: Number,
    pub updated_at: String,
    pub expires_at: String,
    pub handoff_evidence: Value,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid lease JSON: {0}")]
    Json(#[source] serde_json::Error),
    #[error("could not encode lease JSON: {0}")]
    Encode(#[from] EncodeError),
    #[error("lease JSON is not UTF-8: {0}")]
    Encoding(#[source] std::str::Utf8Error),
    #[error("lease JSON integer is outside i64/u64 range: {0}")]
    NumberOutOfRange(#[from] NumberOutOfRange),
    #[error("lease record exceeds the {limit}-byte limit: {path}")]
    InputTooLarge { path: PathBuf, limit: u64 },
    #[error("lease generation is invalid")]
    Generation,
    #[error("generation mismatch: expected {expected}, found {display}", display = python_str(.found))]
    Mismatch { expected: Number, found: Value },
    #[error("active lease belongs to {0}; use renew")]
    Active(String),
    #[error("lease belongs to {0}; use takeover with generation and handoff evidence")]
    Owned(String),
    #[error("only the active holder may renew")]
    Renew,
    #[error("active holder and --target are required for transfer")]
    Transfer,
    #[error(
        "expired lease takeover requires a new holder, expected generation and handoff evidence"
    )]
    Takeover,
    #[error("only the holder may release")]
    Release,
    #[error("invalid lease timestamp: {0}")]
    Timestamp(#[from] DateTimeError),
    #[error("naive lease expiry cannot be compared with UTC now")]
    NaiveTimestamp,
    #[error("lease record must be a JSON object")]
    Record,
    #[error("lease path has no filename: {0}")]
    Path(PathBuf),
    #[error("invalid lease record field: {0}")]
    Field(&'static str),
}

fn io_error(path: &Path, source: io::Error) -> Error {
    Error::Io {
        path: path.into(),
        source,
    }
}

fn integer(value: &Value) -> Result<Number, Error> {
    match value {
        Value::Bool(value) => Ok(Number::from(i64::from(*value))),
        Value::Number(number) if number.as_i64().is_some() || number.as_u64().is_some() => {
            Ok(number.clone())
        }
        _ => Err(Error::Generation),
    }
}

fn increment(value: &Number) -> Result<Number, Error> {
    if let Some(value) = value.as_i64() {
        if let Some(next) = value.checked_add(1) {
            return Ok(Number::from(next));
        }
        if value == i64::MAX {
            return Ok(Number::from((i64::MAX as u64) + 1));
        }
    }
    if let Some(value) = value.as_u64() {
        return value.checked_add(1).map(Number::from).ok_or_else(|| {
            Error::NumberOutOfRange(NumberOutOfRange {
                value: "18446744073709551616".into(),
            })
        });
    }
    Err(Error::Generation)
}

fn number_value(number: &Number) -> Value {
    Value::Number(number.clone())
}

fn generation(state: &Value) -> Result<Number, Error> {
    match state.get("generation") {
        None => Ok(Number::from(0)),
        Some(value) => integer(value),
    }
}

fn empty_string() -> Value {
    Value::String(String::new())
}

fn field<'a>(state: &'a Value, name: &str, fallback: &'a Value) -> &'a Value {
    state.get(name).unwrap_or(fallback)
}

fn iso_timestamp(value: &str) -> Result<Timestamp, Error> {
    let normalized = value.replace('Z', "+00:00");
    let datetime = python_fromisoformat(&normalized)?;
    let timestamp = datetime.timestamp().map_err(|error| match error {
        DateTimeError::Naive => Error::NaiveTimestamp,
        other => Error::Timestamp(other),
    })?;
    timestamp.to_jiff().map_err(Error::Timestamp)
}

fn stamp(instant: Timestamp) -> Result<String, Error> {
    let instant = Timestamp::from_microsecond(instant.as_microsecond())
        .map_err(|_| Error::Timestamp(DateTimeError::TimeRange))?;
    let mut value = instant.strftime("%Y-%m-%dT%H:%M:%S").to_string();
    let micros = instant.subsec_nanosecond() / 1000;
    if micros != 0 {
        value.push_str(&format!(".{micros:06}"));
    }
    value.push('Z');
    // Python datetime.isoformat refuses years outside its 1..9999 range.
    python_fromisoformat(&value).map_err(Error::Timestamp)?;
    Ok(value)
}

#[derive(Clone, Copy)]
enum ClockPurpose {
    Expiry,
    Updated,
}

/// Apply source policy. Each clock read occurs only on the branch that used it.
fn transition_with_sources(
    state: &Value,
    request: &Request<'_>,
    load_evidence: impl FnOnce() -> Result<Option<Value>, Error>,
    mut clock: impl FnMut(ClockPurpose) -> Timestamp,
) -> Result<Lease, Error> {
    if !state.is_object() {
        return Err(Error::Record);
    }
    let generation = generation(state)?;
    if let Some(expected) = &request.expected_generation {
        if expected.as_i64().is_none() && expected.as_u64().is_none() {
            return Err(Error::Generation);
        }
        if !python_json_eq(&number_value(&generation), &number_value(expected)) {
            return Err(Error::Mismatch {
                expected: expected.clone(),
                found: state
                    .get("generation")
                    .cloned()
                    .unwrap_or_else(|| Value::from(0)),
            });
        }
    }

    let empty = empty_string();
    let current = field(state, "holder", &empty);
    let status = state.get("status").unwrap_or(&Value::Null);
    let expires = state.get("expires_at").unwrap_or(&empty);
    let active = if python_json_eq(status, &Value::String("active".into())) && python_truth(expires)
    {
        let Value::String(expires) = expires else {
            return Err(Error::Field("expires_at"));
        };
        let expiry = iso_timestamp(expires)?;
        let checked_at = clock(ClockPurpose::Expiry);
        let checked_at = Timestamp::from_microsecond(checked_at.as_microsecond())
            .map_err(|_| Error::Timestamp(DateTimeError::TimeRange))?;
        expiry > checked_at
    } else {
        false
    };

    let new_holder = match request.action {
        Action::Acquire => {
            if active {
                return Err(Error::Active(python_str(current)));
            }
            if !state.as_object().is_some_and(|object| object.is_empty())
                && python_truth(current)
                && !python_json_eq(current, &Value::String(request.holder.into()))
            {
                return Err(Error::Owned(python_str(current)));
            }
            Value::String(request.holder.into())
        }
        Action::Renew => {
            if !active || !python_json_eq(current, &Value::String(request.holder.into())) {
                return Err(Error::Renew);
            }
            Value::String(request.holder.into())
        }
        Action::Transfer => {
            if !active
                || !python_json_eq(current, &Value::String(request.holder.into()))
                || request.target.is_empty()
            {
                return Err(Error::Transfer);
            }
            Value::String(request.target.into())
        }
        Action::Takeover => {
            let evidence = load_evidence()?;
            let valid = evidence.as_ref().is_some_and(|value| {
                value
                    .get("schema_version")
                    .is_some_and(|version| python_json_eq(version, &Value::from(1)))
                    && value
                        .get("from_holder")
                        .is_some_and(|holder| python_json_eq(holder, current))
                    && value.get("to_holder").is_some_and(|holder| {
                        python_json_eq(holder, &Value::String(request.holder.into()))
                    })
                    && value.get("generation").is_some_and(|found| {
                        request
                            .expected_generation
                            .as_ref()
                            .is_some_and(|expected| python_json_eq(found, &number_value(expected)))
                    })
                    && value.get("approved_by").is_some_and(python_truth)
            });
            if active
                || !python_truth(current)
                || python_json_eq(current, &Value::String(request.holder.into()))
                || request.expected_generation.is_none()
                || !valid
            {
                return Err(Error::Takeover);
            }
            Value::String(request.holder.into())
        }
        Action::Release => {
            if !python_json_eq(current, &Value::String(request.holder.into())) {
                return Err(Error::Release);
            }
            Value::String(String::new())
        }
    };

    let updated_at = clock(ClockPurpose::Updated);
    let expires_at = if request.action == Action::Release {
        String::new()
    } else {
        let ttl_seconds = request
            .ttl_seconds
            .as_i64()
            .or_else(|| {
                request
                    .ttl_seconds
                    .as_u64()
                    .and_then(|value| i64::try_from(value).ok())
            })
            .ok_or(Error::Timestamp(DateTimeError::TimeRange))?;
        let duration = jiff::SignedDuration::from_secs(ttl_seconds);
        stamp(
            updated_at
                .checked_add(duration)
                .map_err(|_| Error::Timestamp(DateTimeError::TimeRange))?,
        )?
    };
    let previous_holder = if !python_json_eq(current, &new_holder) {
        current.clone()
    } else {
        state
            .get("previous_holder")
            .cloned()
            .unwrap_or_else(empty_string)
    };
    let handoff_evidence = if request.action == Action::Takeover {
        Value::String(request.handoff.into())
    } else {
        state
            .get("handoff_evidence")
            .cloned()
            .unwrap_or_else(empty_string)
    };

    Ok(Lease {
        schema_version: 1,
        status: if request.action == Action::Release {
            "released"
        } else {
            "active"
        }
        .into(),
        holder: new_holder,
        previous_holder,
        generation: increment(&generation)?,
        updated_at: stamp(updated_at)?,
        expires_at,
        handoff_evidence,
    })
}

/// Apply policy with explicit instants for deterministic callers and tests.
pub fn transition(
    state: &Value,
    request: &Request<'_>,
    evidence: Option<&Value>,
    checked_at: Timestamp,
    updated_at: Timestamp,
) -> Result<Lease, Error> {
    transition_with_sources(
        state,
        request,
        || Ok(evidence.cloned()),
        |purpose| match purpose {
            ClockPurpose::Expiry => checked_at,
            ClockPurpose::Updated => updated_at,
        },
    )
}

fn decode_json(bytes: &[u8]) -> Result<Value, Error> {
    let text = std::str::from_utf8(bytes).map_err(Error::Encoding)?;
    match checked_json::from_str(text) {
        Ok(value) => Ok(value),
        Err(DecodeError::Json(error)) => Err(Error::Json(error)),
        Err(DecodeError::NumberOutOfRange(error)) => Err(Error::NumberOutOfRange(error)),
    }
}

fn read_all(mut file: Box<dyn SafeFile>, path: &Path) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(path, error))?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(Error::InputTooLarge {
            path: path.into(),
            limit: MAX_RECORD_BYTES,
        });
    }
    Ok(bytes)
}

fn open_parent(
    safe_fs: &impl SafeFs,
    path: &Path,
) -> Result<(Box<dyn SafeDir>, OsString, PathBuf), Error> {
    let name = path
        .file_name()
        .ok_or_else(|| Error::Path(path.into()))?
        .to_os_string();
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|error| io_error(parent, error))?;
    let canonical_parent = fs::canonicalize(parent).map_err(|error| io_error(parent, error))?;
    let directory = safe_fs
        .open_anchor(&canonical_parent)
        .map_err(|error| io_error(&canonical_parent, error))?;
    Ok((directory, name, canonical_parent))
}

fn is_symlink_loop(error: &io::Error) -> bool {
    #[cfg(target_os = "linux")]
    {
        error.raw_os_error() == Some(40)
    }
    #[cfg(target_os = "macos")]
    {
        error.raw_os_error() == Some(62)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        error
            .to_string()
            .starts_with("Too many levels of symbolic links")
    }
}

fn open_state_file(
    safe_fs: &impl SafeFs,
    directory: &dyn SafeDir,
    name: &OsStr,
    path: &Path,
) -> Result<Option<Box<dyn SafeFile>>, Error> {
    match directory.open_read(name) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        // Path.read_text follows an existing final symlink. Resolve its target
        // through the host adapter while publication still replaces the link.
        Err(error) if is_symlink_loop(&error) => match fs::canonicalize(path) {
            Ok(target) => safe_fs
                .open_source(&target)
                .map(Some)
                .map_err(|error| io_error(path, error)),
            Err(_) => Ok(None),
        },
        Err(error) => Err(io_error(path, error)),
    }
}

fn encode_record(record: &impl Serialize, indent: Option<usize>) -> Result<Vec<u8>, Error> {
    let mut bytes = dumps(
        record,
        DumpsOptions {
            sort_keys: true,
            ensure_ascii: true,
            indent,
            compact: false,
        },
    )?
    .into_bytes();
    bytes.push(b'\n');
    Ok(bytes)
}

fn publish_record(
    safe_fs: &impl SafeFs,
    directory: &dyn SafeDir,
    name: &OsStr,
    destination_path: &Path,
    record: &impl Serialize,
) -> Result<(), Error> {
    let bytes = encode_record(record, Some(2))?;
    publish_lease(safe_fs, directory, name, destination_path, &bytes)
}

/// Publish a JSON value with the source's sorted, ASCII-only, indented format.
pub fn write_atomic(path: &Path, value: &Value) -> Result<(), Error> {
    let (directory, name, canonical_parent) = open_parent(&LocalFs, path)?;
    let destination_path = canonical_parent.join(&name);
    publish_record(
        &LocalFs,
        directory.as_ref(),
        &name,
        &destination_path,
        value,
    )
}

/// Stage beside the destination, sync the file, then replace atomically.
/// This matches the source's file fsync and rename order without a directory fsync.
fn publish_lease(
    safe_fs: &impl SafeFs,
    directory: &dyn SafeDir,
    destination: &OsStr,
    destination_path: &Path,
    bytes: &[u8],
) -> Result<(), Error> {
    let mut prefix = OsString::from(".");
    prefix.push(destination);
    prefix.push(".");
    for _ in 0..TEMPORARY_ATTEMPTS {
        let random = safe_fs
            .temporary_name()
            .map_err(|error| io_error(destination_path, error))?;
        let mut temporary = prefix.clone();
        temporary.push(random);
        let mut file = match directory.create_new(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_error(destination_path, error)),
        };
        let write_result = file.write_all(bytes).and_then(|()| file.sync());
        drop(file);
        if let Err(error) = write_result {
            cleanup_temporary(directory, &temporary, destination_path)?;
            return Err(io_error(destination_path, error));
        }
        if let Err(error) = directory.replace(&temporary, destination) {
            cleanup_temporary(directory, &temporary, destination_path)?;
            return Err(io_error(destination_path, error));
        }
        return Ok(());
    }
    Err(Error::Io {
        path: destination_path.into(),
        source: io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not reserve a unique lease staging file",
        ),
    })
}

fn cleanup_temporary(
    directory: &dyn SafeDir,
    name: &OsStr,
    destination_path: &Path,
) -> Result<(), Error> {
    match directory.unlink(name) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(destination_path, error)),
    }
}

fn contained_evidence(
    safe_fs: &impl SafeFs,
    directory_path: &Path,
    handoff: &str,
) -> Result<Option<Value>, Error> {
    let requested = if handoff.is_empty() {
        PathBuf::from("/")
    } else {
        PathBuf::from(handoff)
    };
    let Ok(evidence_path) = fs::canonicalize(&requested) else {
        return Ok(None);
    };
    let Ok(canonical_root) = fs::canonicalize(directory_path) else {
        return Ok(None);
    };
    if evidence_path.strip_prefix(&canonical_root).is_err() {
        return Ok(None);
    }
    let Ok(file) = safe_fs.open_source(&evidence_path) else {
        return Ok(None);
    };
    let bytes = match read_all(file, &evidence_path) {
        Ok(bytes) => bytes,
        Err(Error::Io { .. }) => return Ok(None),
        Err(error) => return Err(error),
    };
    match decode_json(&bytes) {
        Ok(value) => Ok(Some(value)),
        Err(Error::Json(_) | Error::Encoding(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Perform the transition while retaining the sidecar lock through publication.
pub fn mutate(
    path: &Path,
    request: &Request<'_>,
    clock: impl FnMut() -> Timestamp,
) -> Result<Lease, Error> {
    mutate_with_fs(&LocalFs, path, request, clock)
}

/// Injectable safe filesystem adapter for policy tests and alternate hosts.
pub fn mutate_with_fs(
    safe_fs: &impl SafeFs,
    path: &Path,
    request: &Request<'_>,
    mut clock: impl FnMut() -> Timestamp,
) -> Result<Lease, Error> {
    let mut lock_name = path.as_os_str().to_owned();
    lock_name.push(".lock");
    let lock_path = PathBuf::from(lock_name);
    let _lock = pending::lock(&lock_path).map_err(|error| io_error(&lock_path, error))?;
    let (directory, name, canonical_parent) = open_parent(safe_fs, path)?;
    let destination_path = canonical_parent.join(&name);
    let (state, ordered_holder) =
        match open_state_file(safe_fs, directory.as_ref(), &name, &destination_path)? {
            Some(file) => {
                let bytes = read_all(file, &destination_path)?;
                let state = decode_json(&bytes)?;
                let text = std::str::from_utf8(&bytes).map_err(Error::Encoding)?;
                let holder = provenant_records::ordered_json::from_str(text)
                    .ok()
                    .and_then(|value| value.get("holder").map(|holder| holder.python_str()));
                (state, holder)
            }
            None => (serde_json::json!({}), None),
        };
    let lease = transition_with_sources(
        &state,
        request,
        || {
            if request.action == Action::Takeover {
                contained_evidence(safe_fs, &canonical_parent, request.handoff)
            } else {
                Ok(None)
            }
        },
        |_| clock(),
    )
    .map_err(|error| match (error, ordered_holder) {
        (Error::Active(_), Some(holder)) => Error::Active(holder),
        (Error::Owned(_), Some(holder)) => Error::Owned(holder),
        (error, _) => error,
    })?;
    publish_record(
        safe_fs,
        directory.as_ref(),
        &name,
        &destination_path,
        &lease,
    )?;
    Ok(lease)
}

const USAGE: &str = "usage: lease.py [-h] --holder HOLDER [--target TARGET] [--ttl TTL] [--expected-generation EXPECTED_GENERATION] [--handoff-evidence HANDOFF_EVIDENCE] {acquire,renew,transfer,takeover,release} lease";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandArgs {
    pub action: Action,
    pub lease: PathBuf,
    pub holder: String,
    pub target: String,
    pub ttl_seconds: Number,
    pub expected_generation: Option<Number>,
    pub handoff: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliOutcome {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ArgsError {
    #[error("argument --{0} needs a value")]
    MissingValue(String),
    #[error("unknown argument: {0}")]
    Unknown(String),
    #[error("invalid choice: {0}")]
    Action(String),
    #[error("the action and lease path are required")]
    MissingPositionals,
    #[error("too many positional arguments")]
    TooManyPositionals,
    #[error("argument --holder is required")]
    MissingHolder,
    #[error("argument --{0}: invalid int value: {1}")]
    Integer(&'static str, String),
    #[error("argument --{0}: expected a UTF-8 value")]
    Utf8(String),
    #[error(transparent)]
    NumberOutOfRange(#[from] NumberOutOfRange),
    #[error("argument --ttl must be positive")]
    Ttl,
    #[error("help requested")]
    Help,
}

fn parse_cli_integer(name: &'static str, value: &str) -> Result<Number, ArgsError> {
    let trimmed = python_strip(value);
    let mut chars = trimmed.chars().peekable();
    let mut cleaned = String::with_capacity(trimmed.len());
    if matches!(chars.peek(), Some('+' | '-')) {
        if let Some(sign) = chars.next() {
            cleaned.push(sign);
        }
    }
    let mut previous_digit = false;
    let mut saw_digit = false;
    let remaining: Vec<char> = chars.collect();
    for (index, ch) in remaining.iter().copied().enumerate() {
        if let Some(digit) = python_decimal_digit(ch) {
            cleaned.push(char::from(b'0' + digit));
            previous_digit = true;
            saw_digit = true;
        } else if ch == '_'
            && previous_digit
            && remaining
                .get(index + 1)
                .is_some_and(|next| python_decimal_digit(*next).is_some())
        {
            previous_digit = false;
        } else {
            return Err(ArgsError::Integer(name, python_repr(value)));
        }
    }
    if !saw_digit || !previous_digit {
        return Err(ArgsError::Integer(name, python_repr(value)));
    }
    if let Ok(value) = cleaned.parse::<i64>() {
        return Ok(Number::from(value));
    }
    if let Ok(value) = cleaned.parse::<u64>() {
        return Ok(Number::from(value));
    }
    Err(ArgsError::NumberOutOfRange(NumberOutOfRange {
        value: cleaned,
    }))
}

fn negative_number_argument(value: &str) -> bool {
    // Python 3.14 argparse recognizes the prefix -\.?\d, not the whole token.
    let Some(value) = value.strip_prefix('-') else { return false; };
    value.strip_prefix('.').unwrap_or(value).chars().next()
        .is_some_and(|ch| python_decimal_digit(ch).is_some())
}
fn option_argument(value: &str) -> bool {
    value != "-" && value.starts_with('-') && !negative_number_argument(value)
}

pub fn parse_args(args: &[OsString]) -> Result<CommandArgs, ArgsError> {
    let mut positionals = Vec::new();
    let mut holder = None;
    let mut target = String::new();
    let mut ttl_seconds = Number::from(900);
    let mut expected_generation = None;
    let mut handoff = String::new();
    let mut index = 0;
    let mut positional_only = false;

    while index < args.len() {
        let token = args[index].to_str();
        if positional_only || token.is_none() {
            positionals.push(args[index].clone());
            index += 1;
            continue;
        }
        let token = token.ok_or(ArgsError::Utf8("argument".into()))?;
        if token == "--" {
            positional_only = true;
            index += 1;
            continue;
        }
        if token == "-h" || token == "--help" {
            return Err(ArgsError::Help);
        }
        if let Some(option) = token.strip_prefix("--") {
            let (name, inline_value) = option
                .split_once('=')
                .map_or((option, None), |(name, value)| (name, Some(value)));
            let next_value = if let Some(value) = inline_value {
                value.to_owned()
            } else {
                let next = args
                    .get(index + 1)
                    .ok_or_else(|| ArgsError::MissingValue(name.to_owned()))?;
                let value = next.to_str().ok_or(ArgsError::Utf8(name.to_owned()))?;
                if option_argument(value) {
                    return Err(ArgsError::MissingValue(name.to_owned()));
                }
                index += 1;
                value.to_owned()
            };
            match name {
                "holder" => holder = Some(next_value),
                "target" => target = next_value,
                "handoff-evidence" => handoff = next_value,
                "ttl" => {
                    let number = parse_cli_integer("ttl", &next_value)?;
                    ttl_seconds = number;
                }
                "expected-generation" => {
                    expected_generation =
                        Some(parse_cli_integer("expected-generation", &next_value)?)
                }
                _ => return Err(ArgsError::Unknown(token.to_owned())),
            }
            index += 1;
            continue;
        }
        if option_argument(token) {
            return Err(ArgsError::Unknown(token.to_owned()));
        }
        positionals.push(args[index].clone());
        index += 1;
    }

    if positionals.len() < 2 {
        return Err(ArgsError::MissingPositionals);
    }
    if positionals.len() > 2 {
        return Err(ArgsError::TooManyPositionals);
    }
    let action_text = positionals[0]
        .to_str()
        .ok_or(ArgsError::Utf8("action".into()))?;
    let action =
        Action::parse(action_text).ok_or_else(|| ArgsError::Action(python_repr(action_text)))?;
    let lease = PathBuf::from(&positionals[1]);
    let holder = holder.ok_or(ArgsError::MissingHolder)?;
    let positive_ttl = match ttl_seconds.as_i64() {
        Some(value) => value > 0,
        None => ttl_seconds.as_u64().is_some_and(|value| value > 0),
    };
    if !positive_ttl {
        return Err(ArgsError::Ttl);
    }
    Ok(CommandArgs {
        action,
        lease,
        holder,
        target,
        ttl_seconds,
        expected_generation,
        handoff,
    })
}

fn usage_error(error: &ArgsError) -> CliOutcome {
    let message = match error {
        ArgsError::Help => {
            return CliOutcome {
                exit_code: 0,
                stdout: format!("{USAGE}\n"),
                stderr: String::new(),
            };
        }
        ArgsError::MissingValue(name) => format!("argument --{name} needs a value"),
        ArgsError::Unknown(value) => format!("unrecognized arguments: {value}"),
        ArgsError::Action(value) => format!(
            "argument action: invalid choice: {value} (choose from 'acquire', 'renew', 'transfer', 'takeover', 'release')"
        ),
        ArgsError::MissingPositionals => "the action and lease path are required".into(),
        ArgsError::TooManyPositionals => "too many positional arguments".into(),
        ArgsError::MissingHolder => "the following arguments are required: --holder".into(),
        ArgsError::Integer(name, value) => {
            format!("argument --{name}: invalid int value: {value}")
        }
        ArgsError::Utf8(name) => format!("argument --{name}: expected a UTF-8 value"),
        ArgsError::NumberOutOfRange(error) => error.to_string(),
        ArgsError::Ttl => "--ttl must be positive".into(),
    };
    CliOutcome {
        exit_code: 2,
        stdout: String::new(),
        stderr: format!("{USAGE}\nlease.py: error: {message}\n"),
    }
}

/// Parse and execute the Python command contract without coupling this crate to
/// the root binary.
pub fn run_cli(args: &[OsString], clock: impl FnMut() -> Timestamp) -> CliOutcome {
    let parsed = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(error) => return usage_error(&error),
    };
    let request = Request {
        action: parsed.action,
        holder: &parsed.holder,
        ttl_seconds: parsed.ttl_seconds,
        expected_generation: parsed.expected_generation,
        target: &parsed.target,
        handoff: &parsed.handoff,
    };
    match mutate(&parsed.lease, &request, clock) {
        Ok(lease) => match dumps(
            &lease,
            DumpsOptions {
                sort_keys: true,
                ensure_ascii: true,
                indent: None,
                compact: false,
            },
        ) {
            Ok(value) => CliOutcome {
                exit_code: 0,
                stdout: format!("{value}\n"),
                stderr: String::new(),
            },
            Err(error) => CliOutcome {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("FAIL: {error}\n"),
            },
        },
        Err(error) => CliOutcome {
            exit_code: 1,
            stdout: String::new(),
            stderr: format!("FAIL: {error}\n"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{fs, sync::Barrier, thread};

    fn instant() -> Timestamp {
        "2026-10-23T01:02:03.000004Z".parse().unwrap()
    }

    fn request(action: Action) -> Request<'static> {
        Request {
            action,
            holder: "claude",
            ttl_seconds: Number::from(60),
            expected_generation: None,
            target: "",
            handoff: "",
        }
    }

    fn lease_value() -> Value {
        serde_json::to_value(
            transition(
                &json!({}),
                &request(Action::Acquire),
                None,
                instant(),
                instant(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn acquire_renew_transfer_and_release_follow_source_policy() {
        let acquired = lease_value();
        assert_eq!(acquired["generation"], 1);
        assert_eq!(acquired["holder"], "claude");
        assert_eq!(
            transition(
                &acquired,
                &request(Action::Acquire),
                None,
                instant(),
                instant()
            )
            .unwrap_err()
            .to_string(),
            "active lease belongs to claude; use renew"
        );
        let moved = transition(
            &acquired,
            &Request {
                action: Action::Transfer,
                target: "codex",
                expected_generation: Some(Number::from(1)),
                ..request(Action::Transfer)
            },
            None,
            instant(),
            instant(),
        )
        .unwrap();
        assert_eq!(moved.holder, json!("codex"));
        assert_eq!(moved.previous_holder, json!("claude"));
        assert_eq!(moved.generation, Number::from(2));
        assert!(matches!(
            transition(
                &serde_json::to_value(&moved).unwrap(),
                &request(Action::Renew),
                None,
                instant(),
                instant()
            ),
            Err(Error::Renew)
        ));
        let released = transition(
            &serde_json::to_value(&moved).unwrap(),
            &Request {
                holder: "codex",
                action: Action::Release,
                ..request(Action::Release)
            },
            None,
            instant(),
            instant(),
        )
        .unwrap();
        assert_eq!(released.status, "released");
        assert_eq!(released.holder, json!(""));
        assert_eq!(released.previous_holder, json!("codex"));
    }

    #[test]
    fn takeover_requires_expired_lease_and_contained_approval() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("LEASE.json");
        let evidence_path = root.path().join("handoff.json");
        let mut old = serde_json::to_value(
            transition(
                &json!({}),
                &request(Action::Acquire),
                None,
                instant(),
                instant(),
            )
            .unwrap(),
        )
        .unwrap();
        old["expires_at"] = json!("2026-01-01T00:00:00Z");
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let evidence = json!({
            "schema_version": 1.0,
            "from_holder": "claude",
            "to_holder": "codex",
            "generation": 1.0,
            "approved_by": ["operator"],
        });
        fs::write(&evidence_path, evidence.to_string()).unwrap();
        let moved = mutate_with_fs(
            &LocalFs,
            &path,
            &Request {
                action: Action::Takeover,
                holder: "codex",
                expected_generation: Some(Number::from(1)),
                handoff: evidence_path.to_str().unwrap(),
                ..request(Action::Takeover)
            },
            instant,
        )
        .unwrap();
        assert_eq!(moved.holder, json!("codex"));
        assert_eq!(
            moved.handoff_evidence,
            json!(evidence_path.to_str().unwrap())
        );
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::write(outside.path(), evidence.to_string()).unwrap();
        assert!(matches!(
            mutate_with_fs(
                &LocalFs,
                &path,
                &Request {
                    action: Action::Takeover,
                    holder: "new",
                    expected_generation: Some(moved.generation.clone()),
                    handoff: outside.path().to_str().unwrap(),
                    ..request(Action::Takeover)
                },
                instant,
            ),
            Err(Error::Takeover)
        ));
    }

    #[test]
    fn reading_a_lease_follows_final_symlinks_then_replaces_the_link() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("LEASE.json");
        let target = root.path().join("target.json");
        let old = lease_value();
        fs::write(&target, serde_json::to_vec(&old).unwrap()).unwrap();
        symlink(&target, &path).unwrap();
        let renewed = mutate(
            &path,
            &Request {
                action: Action::Renew,
                holder: "claude",
                ..request(Action::Renew)
            },
            instant,
        )
        .unwrap();
        assert_eq!(renewed.generation, Number::from(2));
        assert!(!fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(target).unwrap()).unwrap(),
            old
        );
    }

    #[test]
    fn retained_holder_and_evidence_fields_keep_permissive_json_values() {
        let mut old = lease_value();
        old["previous_holder"] = json!({"provider": ["claude", 1]});
        old["handoff_evidence"] = Value::Null;
        let renewed =
            transition(&old, &request(Action::Renew), None, instant(), instant()).unwrap();
        assert_eq!(renewed.previous_holder, old["previous_holder"]);
        assert_eq!(renewed.handoff_evidence, Value::Null);
        old["holder"] = json!(false);
        old["expires_at"] = json!("");
        let reacquired =
            transition(&old, &request(Action::Acquire), None, instant(), instant()).unwrap();
        assert_eq!(reacquired.previous_holder, json!(false));
    }

    #[test]
    fn python_integer_rules_accept_bool_and_increment_exactly() {
        let mut state = json!({"generation":true});
        let first = transition(
            &state,
            &request(Action::Acquire),
            None,
            instant(),
            instant(),
        )
        .unwrap();
        assert_eq!(first.generation, Number::from(2));
        state["generation"] = json!(1.0);
        assert!(matches!(
            transition(
                &state,
                &request(Action::Acquire),
                None,
                instant(),
                instant()
            ),
            Err(Error::Generation)
        ));
        let max = json!({"generation":u64::MAX});
        assert!(matches!(
            transition(&max, &request(Action::Acquire), None, instant(), instant()),
            Err(Error::NumberOutOfRange(_))
        ));
    }

    #[test]
    fn timestamp_parsing_uses_python_fromisoformat_and_rejects_naive_comparison() {
        assert_eq!(
            stamp("2026-10-23T01:02:03.000000Z".parse().unwrap()).unwrap(),
            "2026-10-23T01:02:03Z"
        );
        assert_eq!(
            stamp("2026-10-23T01:02:03.123000Z".parse().unwrap()).unwrap(),
            "2026-10-23T01:02:03.123000Z"
        );
        assert_eq!(
            stamp("2026-10-23T01:02:03.123004Z".parse().unwrap()).unwrap(),
            "2026-10-23T01:02:03.123004Z"
        );
        let mut state = lease_value();
        state["expires_at"] = json!("2026-10-23T01:02:04Z");
        assert!(transition(&state, &request(Action::Renew), None, instant(), instant()).is_ok());
        state["expires_at"] = json!("2026-10-23T01:02:04");
        assert!(matches!(
            transition(&state, &request(Action::Renew), None, instant(), instant()),
            Err(Error::NaiveTimestamp)
        ));
    }

    #[test]
    fn malformed_and_out_of_range_state_json_are_typed_errors() {
        assert!(matches!(decode_json(b"{"), Err(Error::Json(_))));
        assert!(matches!(decode_json(&[0xff]), Err(Error::Encoding(_))));
        assert!(matches!(
            decode_json(br#"{"unknown":18446744073709551616}"#),
            Err(Error::NumberOutOfRange(_))
        ));
        for value in [json!(null), json!([]), json!("bad")] {
            assert!(matches!(
                transition(
                    &value,
                    &request(Action::Acquire),
                    None,
                    instant(),
                    instant()
                ),
                Err(Error::Record)
            ));
        }
    }

    #[test]
    fn takeover_evidence_is_loaded_only_after_generation_and_expiry_checks() {
        use std::cell::Cell;

        let state = json!({"status":"active","holder":"old","generation":2,"expires_at":"2026-01-01T00:00:00Z"});
        let load_count = Cell::new(0);
        let clock_count = Cell::new(0);
        let result = transition_with_sources(
            &state,
            &Request {
                action: Action::Takeover,
                holder: "new",
                ttl_seconds: Number::from(60),
                expected_generation: Some(Number::from(1)),
                target: "",
                handoff: "does-not-matter.json",
            },
            || {
                load_count.set(load_count.get() + 1);
                Ok(None)
            },
            |_| {
                clock_count.set(clock_count.get() + 1);
                instant()
            },
        );
        assert!(matches!(result, Err(Error::Mismatch { .. })));
        assert_eq!(clock_count.get(), 0);
        assert_eq!(load_count.get(), 0);
    }

    #[test]
    fn clock_is_sampled_only_on_branches_that_call_source_now() {
        use std::cell::Cell;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("lease.json");
        let calls = Cell::new(0);
        let mut counted_now = || {
            calls.set(calls.get() + 1);
            instant()
        };
        mutate(&path, &request(Action::Acquire), &mut counted_now).unwrap();
        assert_eq!(calls.get(), 1);

        calls.set(0);
        let wrong_holder = Request {
            holder: "someone else",
            action: Action::Renew,
            ..request(Action::Renew)
        };
        assert!(matches!(
            mutate(&path, &wrong_holder, &mut counted_now),
            Err(Error::Renew)
        ));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn simultaneous_acquires_are_serialized_by_the_sidecar_lock() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("LEASE.json");
        let barrier = Barrier::new(2);
        let results = thread::scope(|scope| {
            ["claude", "codex"]
                .into_iter()
                .map(|holder| {
                    let path = &path;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        mutate_with_fs(
                            &LocalFs,
                            path,
                            &Request {
                                action: Action::Acquire,
                                holder,
                                ..request(Action::Acquire)
                            },
                            instant,
                        )
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let persisted: Lease = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(persisted.generation, Number::from(1));
    }

    #[test]
    fn cli_arguments_and_outcomes_are_typed() {
        assert_eq!(
            parse_cli_integer("ttl", "\u{1c}+١_٢\u{1f}").unwrap(),
            Number::from(12)
        );
        assert!(matches!(
            parse_cli_integer("ttl", "²"),
            Err(ArgsError::Integer(..))
        ));
        let parsed = parse_args(&[
            "acquire".into(),
            "lease.json".into(),
            "--holder=claude".into(),
            "--ttl".into(),
            "60".into(),
            "--expected-generation".into(),
            "18446744073709551615".into(),
        ])
        .unwrap();
        assert_eq!(parsed.action, Action::Acquire);
        assert_eq!(parsed.expected_generation, Some(Number::from(u64::MAX)));
        assert!(matches!(
            parse_args(&[
                "acquire".into(),
                "lease".into(),
                "--holder".into(),
                "c".into(),
                "--ttl".into(),
                "0".into()
            ]),
            Err(ArgsError::Ttl)
        ));
        let root = tempfile::tempdir().unwrap();
        let outcome = run_cli(
            &[
                "acquire".into(),
                root.path().join("lease").into_os_string(),
                "--holder".into(),
                "c".into(),
            ],
            instant,
        );
        assert_eq!(outcome.exit_code, 0);
        assert!(outcome.stderr.is_empty());
        let bad = run_cli(&[], instant);
        assert_eq!(bad.exit_code, 2);
        assert!(bad.stderr.contains("lease.py: error:"));
    }

    #[test]
    fn differential_matches_python_policy_and_published_bytes() {
        let Some(mut command) = crate::oracle::python() else {
            return;
        };
        let script = r#"
import json,sys
from datetime import datetime,timezone
from pathlib import Path
sys.path.insert(0,str(Path(sys.argv[1]).parent))
import lease as m
m.now=lambda:datetime(2026,10,23,1,2,3,4,tzinfo=timezone.utc)
ops=json.loads(sys.argv[3])
path=Path(sys.argv[2])/'python.json'
rows=[]; files=[]
for op in ops:
 try:
  rows.append(m.mutate(path,op['action'],op['holder'],op['ttl'],op['expected'],op['target'],op['handoff']))
 except ValueError as e:
  rows.append(str(e))
 files.append(path.read_text() if path.exists() else '')
takeover_path=Path(sys.argv[2])/'takeover.json'
takeover_path.write_text(json.dumps({'status':'active','holder':'old','generation':1,'expires_at':'2026-01-01T00:00:00Z'}))
evidence_path=Path(sys.argv[2])/'evidence.json'
evidence_path.write_text(json.dumps({'schema_version':1.0,'from_holder':'old','to_holder':'new','generation':1.0,'approved_by':['operator']}))
takeover=m.mutate(takeover_path,'takeover','new',60,1,'',str(evidence_path))
write_path=Path(sys.argv[2])/'write-helper.json'
m.write_atomic(write_path,{'z':['é',1.5],'a':{'😀':True}})
print(json.dumps({'rows':rows,'bytes':files,'takeover':takeover,'takeover_bytes':takeover_path.read_text(),'write_helper_bytes':write_path.read_text()},sort_keys=True))
"#;
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../skills/orchestrate/scripts/lease.py");
        let root = tempfile::tempdir().unwrap();
        let operations = json!([
            {"action":"acquire","holder":"cläu😀de","ttl":60,"expected":null,"target":"","handoff":""},
            {"action":"acquire","holder":"cläu😀de","ttl":60,"expected":null,"target":"","handoff":""},
            {"action":"renew","holder":"wrong","ttl":60,"expected":1,"target":"","handoff":""},
            {"action":"transfer","holder":"cläu😀de","ttl":60,"expected":1,"target":"codex","handoff":""},
            {"action":"release","holder":"codex","ttl":60,"expected":2,"target":"","handoff":""}
        ]);
        use crate::oracle::CommandOutput;
        let output = command
            .args(["-c", script])
            .arg(&source)
            .arg(root.path())
            .arg(operations.to_string())
            .env("TZ", "UTC")
            .bounded_output()
            .unwrap();
        assert!(
            output.status.success(),
            "oracle: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected: Value = serde_json::from_slice(&output.stdout).unwrap();

        let rust_path = root.path().join("rust.json");
        let mut rows = Vec::new();
        let mut files = Vec::new();
        for op in operations.as_array().unwrap() {
            let action = Action::parse(op["action"].as_str().unwrap()).unwrap();
            let holder = op["holder"].as_str().unwrap();
            let target = op["target"].as_str().unwrap();
            let handoff = op["handoff"].as_str().unwrap();
            let expected_generation = op["expected"].as_i64().map(Number::from);
            let request = Request {
                action,
                holder,
                ttl_seconds: Number::from(op["ttl"].as_i64().unwrap()),
                expected_generation,
                target,
                handoff,
            };
            match mutate(&rust_path, &request, instant) {
                Ok(lease) => rows.push(serde_json::to_value(lease).unwrap()),
                Err(error) => rows.push(Value::String(error.to_string())),
            }
            files.push(fs::read_to_string(&rust_path).unwrap_or_default());
        }
        let write_path = root.path().join("write-helper.json");
        write_atomic(&write_path, &json!({"z":["é",1.5],"a":{"😀":true}})).unwrap();

        let takeover_path = root.path().join("rust-takeover.json");
        fs::write(
            &takeover_path,
            json!({
                "status": "active",
                "holder": "old",
                "generation": 1,
                "expires_at": "2026-01-01T00:00:00Z"
            })
            .to_string(),
        )
        .unwrap();
        let evidence_path = root.path().join("evidence.json");
        fs::write(
            &evidence_path,
            json!({
                "schema_version": 1.0,
                "from_holder": "old",
                "to_holder": "new",
                "generation": 1.0,
                "approved_by": ["operator"]
            })
            .to_string(),
        )
        .unwrap();
        let takeover = mutate(
            &takeover_path,
            &Request {
                action: Action::Takeover,
                holder: "new",
                ttl_seconds: Number::from(60),
                expected_generation: Some(Number::from(1)),
                target: "",
                handoff: evidence_path.to_str().unwrap(),
            },
            instant,
        )
        .unwrap();
        assert_eq!(
            json!({
                "rows": rows,
                "bytes": files,
                "takeover": takeover,
                "takeover_bytes": fs::read_to_string(takeover_path).unwrap(),
                "write_helper_bytes": fs::read_to_string(write_path).unwrap()
            }),
            expected
        );
    }
}

#[cfg(test)]
mod parity_regressions {
    use super::*;
    #[test]
    fn timestamp_auto_format_always_uses_six_fractional_digits() {
        for text in ["2026-10-23T01:02:03.001000Z", "2026-10-23T01:02:03.999000Z"] {
            let instant: Timestamp = text.parse().unwrap();
            assert_eq!(stamp(instant).unwrap(), text);
        }
    }
    #[test]
    fn invalid_utf8_handoff_is_ignored_like_python_value_error() {
        let root = tempfile::tempdir().unwrap();
        let evidence = root.path().join("evidence.json");
        fs::write(&evidence, [255]).unwrap();
        assert!(
            contained_evidence(&LocalFs, root.path(), evidence.to_str().unwrap())
                .unwrap()
                .is_none()
        );
    }
}

#[cfg(test)]
mod review_regressions {
    use super::*;
    fn request() -> Request<'static> {
        Request {
            action: Action::Acquire,
            holder: "new",
            ttl_seconds: Number::from(60),
            expected_generation: None,
            target: "",
            handoff: "",
        }
    }
    fn now() -> Timestamp {
        "2026-10-23T01:02:03Z".parse().unwrap()
    }
    #[test]
    fn option_tokens_cannot_become_a_lease_holder() {
        for flag in ["--target", "--help", "-h", "-x"] {
            let args: Vec<OsString> = ["acquire", "lease.json", "--holder", flag]
                .into_iter()
                .map(Into::into)
                .collect();
            assert!(
                matches!(parse_args(&args), Err(ArgsError::MissingValue(_))),
                "{flag}"
            );
        }
        for value in ["-", "-1", "-٢", "-.1", "-1.5", "-1.", "-1x", "-.5x"] {
            let args: Vec<OsString> = ["acquire", "lease.json", "--holder", value]
                .into_iter()
                .map(Into::into)
                .collect();
            assert_eq!(parse_args(&args).unwrap().holder, value);
        }
    }
    #[test]
    fn bool_generation_mismatch_uses_python_rendering() {
        let req = Request {
            expected_generation: Some(Number::from(2)),
            ..request()
        };
        let error = transition(
            &serde_json::json!({"generation":true}),
            &req,
            None,
            now(),
            now(),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "generation mismatch: expected 2, found True"
        );
    }
    #[test]
    fn retained_holder_diagnostics_preserve_input_object_order() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("lease.json");
        fs::write(
            &path,
            r#"{"status":"active","holder":{"z":1,"a":2},"expires_at":"2027-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        let error = mutate(&path, &request(), now).unwrap_err();
        assert_eq!(
            error.to_string(),
            "active lease belongs to {'z': 1, 'a': 2}; use renew"
        );
    }
    #[cfg(unix)]
    #[test]
    fn positional_lease_path_retains_os_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(OsString::from_vec(b"lease-\xff".to_vec()));
        let args = vec![
            "acquire".into(),
            path.clone().into_os_string(),
            "--holder".into(),
            "h".into(),
        ];
        assert_eq!(parse_args(&args).unwrap().lease, path);
    }
    #[test]
    fn review_edges_match_source_diagnostics() {
        let Some(mut command) = crate::oracle::python() else {
            return;
        };
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../skills/orchestrate/scripts/lease.py");
        let root = tempfile::tempdir().unwrap();
        let script = r#"
import json,sys
from pathlib import Path
from datetime import datetime,timezone
sys.path.insert(0,str(Path(sys.argv[1]).parent))
import lease as m
m.now=lambda:datetime(2026,10,23,1,2,3,tzinfo=timezone.utc)
path=Path(sys.argv[2])/'source.json'
rows=[]
for raw,expected in [('{"generation":true}',2),('{"status":"active","holder":{"z":1,"a":2},"expires_at":"2027-01-01T00:00:00Z"}',None)]:
 path.write_text(raw)
 try: m.mutate(path,'acquire','new',60,expected,'','')
 except ValueError as error: rows.append(str(error))
print(json.dumps(rows))
"#;
        use crate::oracle::CommandOutput;
        let output = command
            .args(["-c", script])
            .arg(source)
            .arg(root.path())
            .bounded_output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
        let path = root.path().join("rust.json");
        let mut actual = Vec::new();
        for (raw, generation) in [
            (r#"{"generation":true}"#, Some(Number::from(2))),
            (
                r#"{"status":"active","holder":{"z":1,"a":2},"expires_at":"2027-01-01T00:00:00Z"}"#,
                None,
            ),
        ] {
            fs::write(&path, raw).unwrap();
            actual.push(
                mutate(
                    &path,
                    &Request {
                        expected_generation: generation,
                        ..request()
                    },
                    now,
                )
                .unwrap_err()
                .to_string(),
            );
        }
        assert_eq!(actual, expected);
    }
    #[test]
    fn retained_state_and_handoff_reads_are_bounded() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("lease.json");
        fs::write(&path, vec![b' '; MAX_RECORD_BYTES as usize + 1]).unwrap();
        assert!(matches!(
            mutate(&path, &request(), now),
            Err(Error::InputTooLarge { .. })
        ));
        assert!(matches!(
            contained_evidence(&LocalFs, root.path(), path.to_str().unwrap()),
            Err(Error::InputTooLarge { .. })
        ));
    }
}
