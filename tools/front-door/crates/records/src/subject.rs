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
}
