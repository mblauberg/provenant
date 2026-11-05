//! Durable admission of authenticated GitHub event hints.
//!
//! This is the admission boundary only; a consumer must reconcile markers with
//! the forge before using them as current state.
pub mod gh;
pub mod http;
pub mod reconcile;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

/// Work to reconcile after a delivery. A sweep subsumes individual PR hints.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    PullRequest(u64),
    Repository,
}

/// The minimal durable record for a delivery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub received_at: u64,
    pub target: Target,
    pub pending: bool,
    #[serde(default)]
    pub event: String,
    #[serde(default)]
    pub completed_at: Option<u64>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct State {
    version: u32,
    #[serde(default)]
    repository: String,
    deliveries: BTreeMap<String, Marker>,
}

/// The result of admitting a delivery.
#[derive(Debug, PartialEq, Eq)]
pub enum Admission {
    Accepted,
    Duplicate,
    Rejected,
}

/// Result of interpreting a webhook request.
#[derive(Debug, PartialEq, Eq)]
pub enum WebhookResponse {
    /// A new marker is durable or the delivery was already admitted.
    Accepted,
    /// Headers or authenticated payload were invalid.
    Rejected,
}

/// Interpret webhook headers and admit the body. Header names are case
/// insensitive; duplicate security-sensitive headers are rejected. The HTTP
/// transport must bound body size before calling this function.
pub fn receive(
    intake: &mut Intake,
    headers: &[(&str, &str)],
    body: &[u8],
    now: u64,
) -> io::Result<WebhookResponse> {
    fn unique_header<'a>(headers: &'a [(&str, &'a str)], name: &str) -> Option<&'a str> {
        let mut values = headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name));
        let value = values.next()?.1;
        if values.next().is_some() {
            None
        } else {
            Some(value)
        }
    }
    let Some(delivery) = unique_header(headers, "X-GitHub-Delivery") else {
        return Ok(WebhookResponse::Rejected);
    };
    let Some(event) = unique_header(headers, "X-GitHub-Event") else {
        return Ok(WebhookResponse::Rejected);
    };
    let Some(signature) = unique_header(headers, "X-Hub-Signature-256") else {
        return Ok(WebhookResponse::Rejected);
    };
    match intake.admit(delivery, event, Some(signature), body, now)? {
        Admission::Accepted | Admission::Duplicate => Ok(WebhookResponse::Accepted),
        Admission::Rejected => Ok(WebhookResponse::Rejected),
    }
}

/// Single-owner persistent event admission queue.
pub struct Intake {
    path: PathBuf,
    repository: String,
    secret: Vec<u8>,
    state: State,
}

impl Intake {
    /// Open a queue for one repository. The caller serialises access to the file.
    pub fn open(path: impl AsRef<Path>, repository: String, secret: Vec<u8>) -> io::Result<Self> {
        let path = path.as_ref().to_owned();
        let mut state = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<State>(&bytes).map_err(io::Error::other)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => State {
                version: 1,
                ..State::default()
            },
            Err(error) => return Err(error),
        };
        if state.version != 1 {
            return Err(io::Error::other("unsupported intake state version"));
        }
        if !state.repository.is_empty() && state.repository != repository {
            return Err(io::Error::other(
                "intake state belongs to another repository",
            ));
        }
        state.repository = repository.clone();
        Ok(Self {
            path,
            repository,
            secret,
            state,
        })
    }

    /// Verify and durably record a hint before returning `Accepted`.
    pub fn admit(
        &mut self,
        delivery: &str,
        event: &str,
        signature: Option<&str>,
        body: &[u8],
        now: u64,
    ) -> io::Result<Admission> {
        if self.secret.is_empty()
            || !valid_signature(&self.secret, signature, body)
            || delivery.is_empty()
        {
            return Ok(Admission::Rejected);
        }
        let supported = [
            "pull_request",
            "push",
            "check_run",
            "check_suite",
            "status",
            "issue_comment",
            "pull_request_review",
            "pull_request_review_comment",
        ];
        if !supported.contains(&event) {
            return Ok(Admission::Rejected);
        }
        let payload: serde_json::Value = match serde_json::from_slice(body) {
            Ok(value) => value,
            Err(_) => return Ok(Admission::Rejected),
        };
        if payload
            .pointer("/repository/full_name")
            .and_then(|v| v.as_str())
            != Some(self.repository.as_str())
        {
            return Ok(Admission::Rejected);
        }
        if self.state.deliveries.contains_key(delivery) {
            // A previous write may have failed after rename; sync again before acknowledging.
            self.persist()?;
            return Ok(Admission::Duplicate);
        }
        let number = match event {
            "pull_request" | "pull_request_review" | "pull_request_review_comment" => payload
                .pointer("/pull_request/number")
                .and_then(|v| v.as_u64()),
            "issue_comment" => payload
                .pointer("/issue/pull_request")
                .and_then(|_| payload.pointer("/issue/number"))
                .and_then(|v| v.as_u64()),
            "check_run" | "check_suite" => {
                let key = if event == "check_run" {
                    "check_run"
                } else {
                    "check_suite"
                };
                let prs = payload
                    .get(key)
                    .and_then(|v| v.get("pull_requests"))
                    .and_then(|v| v.as_array());
                match prs {
                    Some(prs) if prs.len() == 1 => prs[0].get("number").and_then(|v| v.as_u64()),
                    _ => None,
                }
            }
            _ => None,
        };
        let marker = Marker {
            received_at: now,
            target: number.map_or(Target::Repository, Target::PullRequest),
            pending: true,
            event: event.to_owned(),
            completed_at: None,
        };
        self.state.deliveries.insert(delivery.to_owned(), marker);
        self.persist()?;
        Ok(Admission::Accepted)
    }

    /// Read accepted hints; multiple hints for the same target can be coalesced by the consumer.
    pub fn markers(&self) -> &BTreeMap<String, Marker> {
        &self.state.deliveries
    }

    /// Coalesce the pending work into a repository sweep or distinct PRs.
    pub fn pending_targets(&self) -> Vec<Target> {
        let mut prs = std::collections::BTreeSet::new();
        for marker in self
            .state
            .deliveries
            .values()
            .filter(|marker| marker.pending)
        {
            match marker.target {
                Target::Repository => return vec![Target::Repository],
                Target::PullRequest(number) => {
                    prs.insert(number);
                }
            }
        }
        prs.into_iter().map(Target::PullRequest).collect()
    }

    /// Mark only the deliveries observed before a successful reconciliation as complete.
    pub fn complete(&mut self, deliveries: &[String]) -> io::Result<()> {
        self.complete_at(deliveries, None)
    }

    /// Record the completion time for measurement while preserving older callers.
    pub fn complete_at(
        &mut self,
        deliveries: &[String],
        completed_at: Option<u64>,
    ) -> io::Result<()> {
        let before = self.state.clone();
        for id in deliveries {
            if let Some(marker) = self.state.deliveries.get_mut(id) {
                marker.pending = false;
                marker.completed_at = completed_at;
            }
        }
        if let Err(error) = self.persist() {
            self.state = before;
            return Err(error);
        }
        Ok(())
    }

    fn persist(&self) -> io::Result<()> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let temp = self
            .path
            .with_extension(format!("{}.tmp", std::process::id()));
        let result = (|| {
            use std::io::Write;
            let mut file = fs::File::create(&temp)?;
            serde_json::to_writer(&mut file, &self.state).map_err(io::Error::other)?;
            file.flush()?;
            file.sync_all()?;
            fs::rename(&temp, &self.path)?;
            fs::File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }
}

fn valid_signature(secret: &[u8], signature: Option<&str>, body: &[u8]) -> bool {
    let mut key = [0u8; 64];
    if secret.len() > key.len() {
        key[..32].copy_from_slice(&Sha256::digest(secret));
    } else {
        key[..secret.len()].copy_from_slice(secret);
    }
    let mut inner_pad = [0x36u8; 64];
    let mut outer_pad = [0x5cu8; 64];
    for i in 0..64 {
        inner_pad[i] ^= key[i];
        outer_pad[i] ^= key[i];
    }
    let inner = Sha256::new()
        .chain_update(inner_pad)
        .chain_update(body)
        .finalize();
    let digest = Sha256::new()
        .chain_update(outer_pad)
        .chain_update(inner)
        .finalize();
    let mut expected = String::from("sha256=");
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(expected, "{byte:02x}");
    }
    let actual = signature.unwrap_or("").as_bytes();
    // Compare every byte of the fixed-size representation, including length.
    let mut difference = actual.len() ^ expected.len();
    for (i, byte) in expected.bytes().enumerate() {
        difference |= usize::from(byte ^ actual.get(i).copied().unwrap_or(0));
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn webhook_headers_require_one_valid_signature_and_deduplicate() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let mut intake = Intake::open(
            dir.path().join("state.json"),
            "o/r".into(),
            b"secret".to_vec(),
        )?;
        let body = br#"{"repository":{"full_name":"o/r"},"pull_request":{"number":12}}"#;
        let signature = "sha256=db4061419529a8addfd7db7f148ff8677579ec4723d46ba890f9c7c80f8629ab";
        let missing = [
            ("X-GitHub-Delivery", "id"),
            ("X-GitHub-Event", "pull_request"),
        ];
        assert_eq!(
            receive(&mut intake, &missing, body, 1)?,
            WebhookResponse::Rejected
        );
        let bad = [
            ("X-GitHub-Delivery", "id"),
            ("X-GitHub-Event", "pull_request"),
            ("X-Hub-Signature-256", "sha256=bad"),
        ];
        assert_eq!(
            receive(&mut intake, &bad, body, 1)?,
            WebhookResponse::Rejected
        );
        let ambiguous = [
            ("X-GitHub-Delivery", "id"),
            ("X-GitHub-Event", "pull_request"),
            ("X-Hub-Signature-256", signature),
            ("x-hub-signature-256", signature),
        ];
        assert_eq!(
            receive(&mut intake, &ambiguous, body, 1)?,
            WebhookResponse::Rejected
        );
        let valid = [
            ("x-github-delivery", "id"),
            ("x-github-event", "pull_request"),
            ("x-hub-signature-256", signature),
        ];
        assert_eq!(
            receive(&mut intake, &valid, body, 2)?,
            WebhookResponse::Accepted
        );
        assert_eq!(
            receive(&mut intake, &valid, body, 3)?,
            WebhookResponse::Accepted
        );
        assert_eq!(intake.markers().len(), 1);
        assert_eq!(intake.markers()["id"].received_at, 2);
        assert_eq!(intake.pending_targets(), vec![Target::PullRequest(12)]);
        Ok(())
    }
    #[test]
    fn authenticated_delivery_survives_reopen_and_deduplicates() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.json");
        let body = br#"{"repository":{"full_name":"o/r"},"pull_request":{"number":12}}"#;
        // Signature produced independently using Python's hmac module.
        let signature = "sha256=db4061419529a8addfd7db7f148ff8677579ec4723d46ba890f9c7c80f8629ab";
        let mut intake = Intake::open(&path, "o/r".into(), b"secret".to_vec())?;
        assert_eq!(
            intake.admit("id", "pull_request", None, body, 5)?,
            Admission::Rejected
        );
        assert_eq!(
            intake.admit("id", "pull_request", Some("sha256=bad"), body, 5)?,
            Admission::Rejected
        );
        assert_eq!(
            intake.admit("id", "pull_request", Some(signature), body, 5)?,
            Admission::Accepted
        );
        drop(intake);
        let mut intake = Intake::open(&path, "o/r".into(), b"secret".to_vec())?;
        assert_eq!(
            intake.admit("id", "pull_request", Some(signature), body, 6)?,
            Admission::Duplicate
        );
        assert_eq!(
            intake.markers().get("id").map(|m| &m.target),
            Some(&Target::PullRequest(12))
        );
        Ok(())
    }
    #[test]
    fn unmappable_check_is_a_repository_hint_and_other_repositories_are_rejected() -> io::Result<()>
    {
        let dir = tempfile::tempdir()?;
        let mut intake = Intake::open(
            dir.path().join("state.json"),
            "o/r".into(),
            b"secret".to_vec(),
        )?;
        let body = br#"{"repository":{"full_name":"o/r"},"check_run":{"pull_requests":[]}}"#;
        let signature = "sha256=ca86aaa421aac4e350cee5222e2817b7cf0aa5e3f62d1c9fc0de49e6ea22daf6";
        assert_eq!(
            intake.admit("check", "check_run", Some(signature), body, 9)?,
            Admission::Accepted
        );
        assert_eq!(
            intake.markers().get("check").map(|m| &m.target),
            Some(&Target::Repository)
        );
        let other = br#"{"repository":{"full_name":"elsewhere/r"}}"#;
        let other_signature =
            "sha256=89322cd4e2f370433dc0a9213fee90457c0919a81a33c36faffc155bce0b70eb";
        assert_eq!(
            intake.admit("other", "push", Some(other_signature), other, 9)?,
            Admission::Rejected
        );
        assert_eq!(
            intake.admit("ignored", "deployment", Some(signature), body, 9)?,
            Admission::Rejected
        );
        assert_eq!(intake.markers().len(), 1);
        assert_eq!(intake.pending_targets(), vec![Target::Repository]);
        intake.complete(&["check".into()])?;
        assert!(intake.pending_targets().is_empty());
        drop(intake);
        let intake = Intake::open(
            dir.path().join("state.json"),
            "o/r".into(),
            b"secret".to_vec(),
        )?;
        assert!(intake.pending_targets().is_empty());
        assert!(intake.markers().contains_key("check"));
        Ok(())
    }
}
