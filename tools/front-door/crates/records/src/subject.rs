use provenant_core::{GitOid, WallTime};
use serde::{Deserialize, Serialize};

/// The commits one operation acts on, resolved once at admission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedSubject {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<GitOid>,
    pub head: GitOid,
    pub origin: SubjectOrigin,
    pub pinned_at: WallTime,
    #[serde(default)]
    pub policy: SubjectPolicy,
    /// Credential-free forge repository id (`owner/name`), when the route had one.
    /// It is provenance only, never a credential and never a lookup key here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// The ref name the base came from, when the route named one. The head's
    /// ref name already lives in `origin`; this names the comparison side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    /// `KEY from SOURCE` for the policy that fixed this head, so every policy
    /// reports its source at the point of use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_source: Option<String>,
    /// Earlier pinned heads in order, oldest first, recorded when a
    /// `follow_latest` policy re-pins; empty for a subject that never moved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub followed_from: Vec<GitOid>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubjectOrigin {
    /// A local branch, or HEAD when detached.
    LocalRef {
        name: String,
    },
    /// A fetched remote-tracking ref.
    RemoteTracking {
        name: String,
    },
    ForgePr {
        repository: String,
        number: u64,
    },
    /// A smoke fixture repository created for this run.
    Fixture {
        nonce: String,
    },
    /// A full OID supplied by the caller.
    Explicit,
    /// Reserved for continuation admission (#3972); no producer yet. The
    /// predecessor graph name travels as provenance for the derived head.
    Continuation {
        predecessor: String,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectPolicy {
    #[default]
    Pinned,
    FollowLatest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum HeadCheck {
    Unmoved,
    Moved { pinned: GitOid, current: GitOid },
    Unavailable { reason: String },
}

/// Compare the pinned head with the caller's current observation.
pub fn head_check(pinned: &GitOid, current: Result<GitOid, String>) -> HeadCheck {
    match current {
        Ok(current) if &current == pinned => HeadCheck::Unmoved,
        Ok(current) => HeadCheck::Moved {
            pinned: *pinned,
            current,
        },
        Err(reason) => HeadCheck::Unavailable { reason },
    }
}

/// Classify remote-tracking refs separately from local names.
pub fn ref_origin(name: &str) -> SubjectOrigin {
    if name.starts_with("refs/remotes/") {
        SubjectOrigin::RemoteTracking {
            name: name.to_owned(),
        }
    } else {
        SubjectOrigin::LocalRef {
            name: name.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_check_classifies_observations() {
        let pinned = GitOid::parse(&"1".repeat(40)).unwrap();
        let current = GitOid::parse(&"2".repeat(40)).unwrap();
        assert_eq!(head_check(&pinned, Ok(pinned)), HeadCheck::Unmoved);
        assert_eq!(
            head_check(&pinned, Ok(current)),
            HeadCheck::Moved { pinned, current }
        );
        assert_eq!(
            head_check(&pinned, Err("offline".into())),
            HeadCheck::Unavailable {
                reason: "offline".into()
            }
        );
    }

    #[test]
    fn ref_origin_distinguishes_remote_tracking() {
        assert_eq!(
            ref_origin("refs/remotes/origin/main"),
            SubjectOrigin::RemoteTracking {
                name: "refs/remotes/origin/main".into()
            }
        );
        for name in ["refs/heads/main", "HEAD"] {
            assert_eq!(
                ref_origin(name),
                SubjectOrigin::LocalRef { name: name.into() }
            );
        }
    }

    #[test]
    fn subject_round_trips_unknown_fields() {
        let value = serde_json::json!({"head": "1".repeat(40), "origin": {"kind": "explicit"}, "pinned_at": WallTime::from_micros(1).unwrap(), "policy": "pinned", "future": {"value": 42}});
        let subject: PinnedSubject = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(subject).unwrap(), value);
    }

    /// Every slice-8 capability field is absent when unset, so a record
    /// written before them keeps its exact wire shape and digest.
    #[test]
    fn empty_capability_fields_keep_the_slice_zero_wire_shape() {
        let subject = PinnedSubject {
            extra: Default::default(),
            base: None,
            head: GitOid::parse(&"1".repeat(40)).unwrap(),
            origin: SubjectOrigin::Explicit,
            pinned_at: WallTime::from_micros(1).unwrap(),
            policy: SubjectPolicy::Pinned,
            repository: None,
            base_ref: None,
            policy_source: None,
            followed_from: Vec::new(),
        };
        let expected = serde_json::json!({
            "head": "1".repeat(40),
            "origin": {"kind": "explicit"},
            "pinned_at": WallTime::from_micros(1).unwrap(),
            "policy": "pinned",
        });
        assert_eq!(
            serde_json::to_string(&subject).unwrap(),
            expected.to_string()
        );
    }

    /// The reserved continuation origin round-trips without a producer, so a
    /// future continuation reader can deserialise it without a schema bump.
    #[test]
    fn continuation_origin_round_trips() {
        let origin = SubjectOrigin::Continuation {
            predecessor: "smoke/G".into(),
        };
        let wire = serde_json::to_string(&origin).unwrap();
        assert_eq!(wire, r#"{"kind":"continuation","predecessor":"smoke/G"}"#);
        assert_eq!(
            serde_json::from_str::<SubjectOrigin>(&wire).unwrap(),
            origin
        );
    }
}
