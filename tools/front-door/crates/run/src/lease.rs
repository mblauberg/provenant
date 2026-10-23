//! Single-driver lease policy. Clocks are supplied by the owner.
use crate::pending;
use jiff::Timestamp;
use serde_json::Value;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Acquire,
    Renew,
    Transfer,
    Takeover,
    Release,
}

#[derive(Debug)]
pub struct Request<'a> {
    pub action: Action,
    pub holder: &'a str,
    pub ttl_seconds: i64,
    pub expected_generation: Option<i64>,
    pub target: &'a str,
    pub handoff: &'a str,
}

pub use crate::pending::Lease;

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
    #[error("lease generation is invalid")]
    Generation,
    #[error("generation mismatch: expected {expected}, found {found}")]
    Mismatch { expected: i64, found: i64 },
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
    Timestamp(#[from] jiff::Error),
    #[error("invalid lease record field: {0}")]
    Field(&'static str),
}

fn io_error(path: &Path, source: io::Error) -> Error {
    Error::Io {
        path: path.into(),
        source,
    }
}

fn string<'a>(state: &'a Value, name: &'static str) -> Result<&'a str, Error> {
    match state.get(name) {
        None => Ok(""),
        Some(Value::String(s)) => Ok(s),
        _ => Err(Error::Field(name)),
    }
}

fn stamp(instant: Timestamp) -> String {
    let mut value = instant.strftime("%Y-%m-%dT%H:%M:%S").to_string();
    let micros = instant.subsec_nanosecond() / 1000;
    if micros != 0 {
        value.push_str(&format!(".{micros:06}"));
    }
    value.push('Z');
    value
}

/// Apply policy to retained JSON and already-contained handoff evidence.
/// The two instants preserve the source's separate expiry check and update clock reads.
pub fn transition(
    state: &Value,
    request: &Request<'_>,
    evidence: Option<&Value>,
    checked_at: Timestamp,
    updated_at: Timestamp,
) -> Result<Lease, Error> {
    if !state.is_object() {
        return Err(Error::Field("object"));
    }
    let generation = match state.get("generation") {
        None => 0,
        Some(Value::Bool(b)) => i64::from(*b), // Python bool is an int here.
        Some(v) => v.as_i64().ok_or(Error::Generation)?,
    };
    if let Some(expected) = request.expected_generation {
        if expected != generation {
            return Err(Error::Mismatch {
                expected,
                found: generation,
            });
        }
    }
    let active = if state.get("status").and_then(Value::as_str) == Some("active") {
        match state.get("expires_at") {
            None | Some(Value::Null) => false,
            Some(Value::String(s)) if s.is_empty() => false,
            Some(Value::String(s)) => s.parse::<Timestamp>()? > checked_at,
            _ => return Err(Error::Field("expires_at")),
        }
    } else {
        false
    };
    let current = string(state, "holder")?;
    let holder = match request.action {
        Action::Acquire => {
            if active {
                return Err(Error::Active(current.into()));
            }
            if !current.is_empty() && current != request.holder {
                return Err(Error::Owned(current.into()));
            }
            request.holder
        }
        Action::Renew => {
            if !active || current != request.holder {
                return Err(Error::Renew);
            }
            request.holder
        }
        Action::Transfer => {
            if !active || current != request.holder || request.target.is_empty() {
                return Err(Error::Transfer);
            }
            request.target
        }
        Action::Takeover => {
            let valid = evidence.is_some_and(|v| {
                v.get("schema_version").is_some_and(|v| v == 1 || v == true)
                    && v.get("from_holder").and_then(Value::as_str) == Some(current)
                    && v.get("to_holder").and_then(Value::as_str) == Some(request.holder)
                    && v.get("generation").and_then(Value::as_i64) == request.expected_generation
                    && v.get("approved_by").is_some_and(pending::json_truthy)
            });
            if active
                || current.is_empty()
                || current == request.holder
                || request.expected_generation.is_none()
                || !valid
            {
                return Err(Error::Takeover);
            }
            request.holder
        }
        Action::Release => {
            if current != request.holder {
                return Err(Error::Release);
            }
            ""
        }
    };
    let expiry = if request.action == Action::Release {
        String::new()
    } else {
        stamp(updated_at.checked_add(jiff::SignedDuration::from_secs(request.ttl_seconds))?)
    };
    Ok(Lease {
        schema_version: 1,
        status: if request.action == Action::Release {
            "released"
        } else {
            "active"
        }
        .into(),
        holder: holder.into(),
        previous_holder: if current != holder {
            current
        } else {
            string(state, "previous_holder")?
        }
        .into(),
        generation: generation.checked_add(1).ok_or(Error::Generation)?,
        updated_at: stamp(updated_at),
        expires_at: expiry,
        handoff_evidence: if request.action == Action::Takeover {
            request.handoff
        } else {
            string(state, "handoff_evidence")?
        }
        .into(),
    })
}

/// Hold the persistent sibling lock through read, policy evaluation and publication.
pub fn mutate(
    path: &Path,
    request: &Request<'_>,
    mut clock: impl FnMut() -> Timestamp,
) -> Result<Lease, Error> {
    let mut lock_name = path.as_os_str().to_owned();
    lock_name.push(".lock");
    let lock_path = PathBuf::from(lock_name);
    let _lock = pending::lock(&lock_path).map_err(|e| io_error(&lock_path, e))?;
    let state = if path.exists() {
        let bytes = pending::read_bounded(path, 1024 * 1024).map_err(|e| io_error(path, e))?;
        serde_json::from_slice(&bytes).map_err(Error::Json)?
    } else {
        serde_json::json!({})
    };
    let evidence = if request.action == Action::Takeover {
        fs::canonicalize(request.handoff)
            .ok()
            .filter(|p| {
                fs::canonicalize(pending::parent(path)).is_ok_and(|root| p.starts_with(root))
            })
            .and_then(|p| pending::read_bounded(&p, 1024 * 1024).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
    } else {
        None
    };
    let lease = transition(&state, request, evidence.as_ref(), clock(), clock())?;
    let value = serde_json::to_value(&lease).map_err(Error::Json)?;
    let mut bytes =
        pending::ascii_json(&serde_json::to_string_pretty(&value).map_err(Error::Json)?)
            .into_bytes();
    bytes.push(b'\n');
    pending::publish_lease(path, &bytes).map_err(|e| io_error(path, e))?;
    Ok(lease)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::process::Command;
    fn instant() -> Timestamp {
        "2026-10-23T01:02:03.000004Z".parse().unwrap()
    }
    fn request(action: Action) -> Request<'static> {
        Request {
            action,
            holder: "claude",
            ttl_seconds: 60,
            expected_generation: None,
            target: "",
            handoff: "",
        }
    }
    fn first() -> Lease {
        transition(
            &json!({}),
            &request(Action::Acquire),
            None,
            instant(),
            instant(),
        )
        .unwrap()
    }
    fn state() -> Value {
        serde_json::to_value(first()).unwrap()
    }

    #[test]
    fn active_lease_has_one_holder_and_cannot_be_reacquired() {
        let state = state();
        for holder in ["claude", "codex"] {
            let req = Request {
                holder,
                ..request(Action::Acquire)
            };
            assert_eq!(
                transition(&state, &req, None, instant(), instant())
                    .unwrap_err()
                    .to_string(),
                "active lease belongs to claude; use renew"
            );
        }
        assert_eq!(first().generation, 1);
    }
    #[test]
    fn transfer_checks_generation_and_revokes_previous_holder() {
        let req = Request {
            target: "codex",
            expected_generation: Some(1),
            ..request(Action::Transfer)
        };
        let moved = transition(&state(), &req, None, instant(), instant()).unwrap();
        assert_eq!(
            (
                moved.holder.as_str(),
                moved.previous_holder.as_str(),
                moved.generation
            ),
            ("codex", "claude", 2)
        );
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
        assert!(matches!(
            transition(
                &state(),
                &Request {
                    expected_generation: Some(0),
                    ..req
                },
                None,
                instant(),
                instant()
            ),
            Err(Error::Mismatch { .. })
        ));
    }
    #[test]
    fn expiry_does_not_allow_another_holder_to_acquire() {
        let mut old = state();
        old["expires_at"] = json!("2026-10-23T01:02:03Z");
        let req = Request {
            holder: "codex",
            ..request(Action::Acquire)
        };
        assert!(matches!(
            transition(&old, &req, None, instant(), instant()),
            Err(Error::Owned(_))
        ));
        assert!(matches!(
            transition(&old, &request(Action::Renew), None, instant(), instant()),
            Err(Error::Renew)
        ));
        assert!(transition(&old, &request(Action::Acquire), None, instant(), instant()).is_ok());
    }
    #[test]
    fn takeover_requires_expiry_new_holder_generation_and_approval() {
        let mut old = state();
        old["expires_at"] = json!("2026-01-01T00:00:00Z");
        let evidence = json!({"schema_version":1,"from_holder":"claude","to_holder":"codex","generation":1,"approved_by":"human"});
        let req = Request {
            holder: "codex",
            expected_generation: Some(1),
            handoff: "handoff.json",
            ..request(Action::Takeover)
        };
        let moved = transition(&old, &req, Some(&evidence), instant(), instant()).unwrap();
        assert_eq!(
            (
                moved.holder.as_str(),
                moved.generation,
                moved.handoff_evidence.as_str()
            ),
            ("codex", 2, "handoff.json")
        );
        for field in [
            "schema_version",
            "from_holder",
            "to_holder",
            "generation",
            "approved_by",
        ] {
            let mut bad = evidence.clone();
            bad.as_object_mut().unwrap().remove(field);
            assert!(
                matches!(
                    transition(&old, &req, Some(&bad), instant(), instant()),
                    Err(Error::Takeover)
                ),
                "{field}"
            );
        }
        assert!(matches!(
            transition(&state(), &req, Some(&evidence), instant(), instant()),
            Err(Error::Takeover)
        ));
        assert!(matches!(
            transition(
                &old,
                &Request {
                    expected_generation: None,
                    ..req
                },
                Some(&evidence),
                instant(),
                instant()
            ),
            Err(Error::Takeover)
        ));
    }
    #[test]
    fn release_clears_holder_and_expiry_but_retains_handoff() {
        let mut old = state();
        old["handoff_evidence"] = json!("handoff.json");
        let released =
            transition(&old, &request(Action::Release), None, instant(), instant()).unwrap();
        assert_eq!(
            (
                released.status.as_str(),
                released.holder.as_str(),
                released.expires_at.as_str(),
                released.previous_holder.as_str(),
                released.handoff_evidence.as_str()
            ),
            ("released", "", "", "claude", "handoff.json")
        );
        assert!(matches!(
            transition(
                &old,
                &Request {
                    holder: "codex",
                    ..request(Action::Release)
                },
                None,
                instant(),
                instant()
            ),
            Err(Error::Release)
        ));
    }
    #[test]
    fn renew_preserves_previous_holder_and_formats_microseconds() {
        let mut old = state();
        old["previous_holder"] = json!("other");
        let renewed =
            transition(&old, &request(Action::Renew), None, instant(), instant()).unwrap();
        assert_eq!(renewed.previous_holder, "other");
        assert_eq!(renewed.updated_at, "2026-10-23T01:02:03.000004Z");
        assert_eq!(renewed.expires_at, "2026-10-23T01:03:03.000004Z");
    }
    #[test]
    fn invalid_generation_and_timestamp_are_typed_errors() {
        for bad in [json!(null), json!(1.5), json!("1"), json!([])] {
            let mut old = state();
            old["generation"] = bad;
            assert!(matches!(
                transition(&old, &request(Action::Acquire), None, instant(), instant()),
                Err(Error::Generation)
            ));
        }
        let mut old = state();
        old["expires_at"] = json!("invalid");
        assert!(matches!(
            transition(&old, &request(Action::Acquire), None, instant(), instant()),
            Err(Error::Timestamp(_))
        ));
    }
    #[test]
    fn expired_takeover_requires_contained_evidence() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let path = root.path().join("LEASE.json");
        let mut old = state();
        old["expires_at"] = json!("2026-01-01T00:00:00Z");
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        let evidence = json!({"schema_version":1,"from_holder":"claude","to_holder":"codex","generation":1,"approved_by":"human"});
        fs::write(outside.path(), evidence.to_string()).unwrap();
        let req = Request {
            holder: "codex",
            expected_generation: Some(1),
            handoff: outside.path().to_str().unwrap(),
            ..request(Action::Takeover)
        };
        assert!(matches!(mutate(&path, &req, instant), Err(Error::Takeover)));
        let inside = root.path().join("handoff.json");
        fs::write(&inside, evidence.to_string()).unwrap();
        let moved = mutate(
            &path,
            &Request {
                handoff: inside.to_str().unwrap(),
                ..req
            },
            instant,
        )
        .unwrap();
        assert_eq!(moved.holder, "codex");
    }
    #[test]
    fn simultaneous_acquires_publish_one_holder() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("LEASE.json");
        let barrier = std::sync::Barrier::new(2);
        let outcomes = std::thread::scope(|scope| {
            let handles: Vec<_> = ["claude", "codex"]
                .into_iter()
                .map(|holder| {
                    let path = &path;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        mutate(
                            path,
                            &Request {
                                holder,
                                ..request(Action::Acquire)
                            },
                            instant,
                        )
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(outcomes.iter().filter(|r| r.is_ok()).count(), 1);
        let persisted: Lease = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(persisted.generation, 1);
    }
    #[test]
    fn failed_read_does_not_replace_invalid_state() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("LEASE.json");
        fs::write(&path, "{").unwrap();
        assert!(matches!(
            mutate(&path, &request(Action::Acquire), instant),
            Err(Error::Json(_))
        ));
        assert_eq!(fs::read_to_string(path).unwrap(), "{");
    }
    #[test]
    fn differential_python_policy_and_publication_bytes() {
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../skills/orchestrate/scripts/lease.py");
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("python.json");
        let result = Command::new("python3").args(["-c", r#"
import importlib.util,json,sys
from datetime import datetime,timezone
from pathlib import Path
spec=importlib.util.spec_from_file_location('lease',sys.argv[1]); m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
m.now=lambda:datetime(2026,10,23,1,2,3,4,tzinfo=timezone.utc)
p=Path(sys.argv[2]);rows=[]
for action,holder,expected,target in [('acquire','cläu😀de',None,''),('acquire','cläu😀de',None,''),('renew','wrong',1,''),('transfer','cläu😀de',1,'codex'),('release','codex',2,'')]:
 try: rows.append(m.mutate(p,action,holder,60,expected,target))
 except ValueError as e: rows.append(str(e))
print(json.dumps(rows))
"#]).arg(script).arg(&path).output();
        let result = match result {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                eprintln!("SKIP: python3 absent");
                return;
            }
            other => other.unwrap(),
        };
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let mut rows = Vec::new();
        let mut old = json!({});
        for (action, holder, expected, target) in [
            (Action::Acquire, "cläu😀de", None, ""),
            (Action::Acquire, "cläu😀de", None, ""),
            (Action::Renew, "wrong", Some(1), ""),
            (Action::Transfer, "cläu😀de", Some(1), "codex"),
            (Action::Release, "codex", Some(2), ""),
        ] {
            let req = Request {
                action,
                holder,
                expected_generation: expected,
                target,
                ..request(action)
            };
            match transition(&old, &req, None, instant(), instant()) {
                Ok(lease) => {
                    old = serde_json::to_value(lease).unwrap();
                    rows.push(old.clone());
                }
                Err(e) => rows.push(json!(e.to_string())),
            }
        }
        assert_eq!(
            json!(rows),
            serde_json::from_slice::<Value>(&result.stdout).unwrap()
        );
        let expected = pending::ascii_json(&serde_json::to_string_pretty(&old).unwrap()) + "\n";
        assert_eq!(fs::read_to_string(&path).unwrap(), expected);
        let rust_path = root.path().join("rust.json");
        mutate(&rust_path, &request(Action::Acquire), instant).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(rust_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
