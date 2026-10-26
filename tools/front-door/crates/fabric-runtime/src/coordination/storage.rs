//! SQLite coordination receipt DTOs. Storage units and proof bytes remain
//! independent of the public request contract.
use super::*;

macro_rules! stored_proof {
    ($stored:ident, $public:ident, {$($field:ident: $ty:ty),* $(,)?}) => {
        #[derive(Serialize, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct $stored { $(pub $field: $ty),* }
        impl std::fmt::Debug for $stored {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($stored)).finish_non_exhaustive()
            }
        }
        impl From<$public> for $stored {
            fn from(value: $public) -> Self { Self { $($field: value.$field),* } }
        }
        impl From<$stored> for $public {
            fn from(value: $stored) -> Self { Self { $($field: value.$field),* } }
        }
    };
}
stored_proof!(StoredTaskProof, TaskProof, {
    task: TaskId, generation: u64, token: [u8; 16],
});
stored_proof!(StoredClaimProof, ClaimProof, {
    id: ClaimId, generation: u64, token: [u8; 16],
});
stored_proof!(StoredLandingProof, LandingProof, {
    reference: String, id: ClaimId, generation: u64, token: [u8; 16],
});

#[derive(Serialize)]
#[serde(tag = "domain", content = "input", rename_all = "snake_case")]
pub(super) enum StoredMutation<'a> {
    Mail(&'a MailInput),
    Task(StoredTaskInput<'a>),
    Events(&'a EventsInput),
    Claim(StoredClaimInput<'a>),
    Landing(StoredLandingInput<'a>),
}

#[derive(Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(super) enum StoredTaskInput<'a> {
    Create {
        objective: &'a str,
        note: &'a Option<String>,
    },
    List {
        state: &'a Option<TaskState>,
        after: &'a Option<TaskId>,
        limit: u64,
    },
    Claim {
        id: TaskId,
        lease_ms: u64,
    },
    Update {
        proof: StoredTaskProof,
        objective: &'a str,
        note: &'a Option<String>,
    },
    Finish {
        proof: StoredTaskProof,
        state: TaskState,
    },
    Release {
        proof: StoredTaskProof,
    },
}

#[derive(Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(super) enum StoredClaimInput<'a> {
    Acquire {
        scope: &'a WorkScope,
        lease_ms: u64,
    },
    Renew {
        proof: StoredClaimProof,
        lease_ms: u64,
    },
    Verify {
        proof: StoredClaimProof,
    },
    Release {
        proof: StoredClaimProof,
    },
    List {
        after: &'a Option<ClaimId>,
        limit: u64,
    },
}

#[derive(Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(super) enum StoredLandingInput<'a> {
    Acquire {
        reference: &'a str,
        expected: &'a GitOid,
        candidate: &'a GitOid,
        lease_ms: u64,
    },
    Renew {
        proof: StoredLandingProof,
        lease_ms: u64,
    },
    Verify {
        proof: StoredLandingProof,
    },
    Release {
        proof: StoredLandingProof,
    },
    Wait {
        reference: &'a str,
        generation: u64,
        wait_ms: u64,
    },
}

impl<'a> From<&'a Mutation> for StoredMutation<'a> {
    fn from(value: &'a Mutation) -> Self {
        match value {
            Mutation::Mail(value) => Self::Mail(value),
            Mutation::Events(value) => Self::Events(value),
            Mutation::Task(value) => Self::Task(match value {
                TaskInput::Create { objective, note } => {
                    StoredTaskInput::Create { objective, note }
                }
                TaskInput::List {
                    state,
                    after,
                    limit,
                } => StoredTaskInput::List {
                    state,
                    after,
                    limit: *limit,
                },
                TaskInput::Claim { id, lease_seconds } => StoredTaskInput::Claim {
                    id: *id,
                    lease_ms: lease_seconds.saturating_mul(1_000),
                },
                TaskInput::Update {
                    proof,
                    objective,
                    note,
                } => StoredTaskInput::Update {
                    proof: proof.clone().into(),
                    objective,
                    note,
                },
                TaskInput::Finish { proof, state } => StoredTaskInput::Finish {
                    proof: proof.clone().into(),
                    state: *state,
                },
                TaskInput::Release { proof } => StoredTaskInput::Release {
                    proof: proof.clone().into(),
                },
            }),
            Mutation::Claim(value) => Self::Claim(match value {
                ClaimInput::Acquire {
                    scope,
                    lease_seconds,
                } => StoredClaimInput::Acquire {
                    scope,
                    lease_ms: lease_seconds.saturating_mul(1_000),
                },
                ClaimInput::Renew {
                    proof,
                    lease_seconds,
                } => StoredClaimInput::Renew {
                    proof: proof.clone().into(),
                    lease_ms: lease_seconds.saturating_mul(1_000),
                },
                ClaimInput::Verify { proof } => StoredClaimInput::Verify {
                    proof: proof.clone().into(),
                },
                ClaimInput::Release { proof } => StoredClaimInput::Release {
                    proof: proof.clone().into(),
                },
                ClaimInput::List { after, limit } => StoredClaimInput::List {
                    after,
                    limit: *limit,
                },
            }),
            Mutation::Landing(value) => Self::Landing(match value {
                LandingInput::Acquire {
                    reference,
                    expected,
                    candidate,
                    lease_seconds,
                } => StoredLandingInput::Acquire {
                    reference,
                    expected,
                    candidate,
                    lease_ms: lease_seconds.saturating_mul(1_000),
                },
                LandingInput::Renew {
                    proof,
                    lease_seconds,
                } => StoredLandingInput::Renew {
                    proof: proof.clone().into(),
                    lease_ms: lease_seconds.saturating_mul(1_000),
                },
                LandingInput::Verify { proof } => StoredLandingInput::Verify {
                    proof: proof.clone().into(),
                },
                LandingInput::Release { proof } => StoredLandingInput::Release {
                    proof: proof.clone().into(),
                },
                LandingInput::Wait {
                    reference,
                    generation,
                    wait_seconds,
                } => StoredLandingInput::Wait {
                    reference,
                    generation: *generation,
                    wait_ms: wait_seconds.saturating_mul(1_000),
                },
            }),
        }
    }
}

/// Result codec used only by SQLite operation receipts.
#[derive(Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StoredMutationResult(StoredResult);

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum StoredResult {
    Task(StoredTaskResult),
    Claim(StoredClaimResult),
    Landing(StoredLandingResult),
    Other(MutationResult),
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
enum StoredTaskResult {
    Claimed { proof: StoredTaskProof },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
enum StoredClaimResult {
    Acquired {
        proof: StoredClaimProof,
        claim: WorkClaim,
    },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
enum StoredLandingResult {
    Acquired {
        proof: StoredLandingProof,
        landing: Landing,
    },
}

impl From<MutationResult> for StoredMutationResult {
    fn from(value: MutationResult) -> Self {
        Self(match value {
            MutationResult::Task(TaskResult::Claimed { proof }) => {
                StoredResult::Task(StoredTaskResult::Claimed {
                    proof: proof.into(),
                })
            }
            MutationResult::Claim(ClaimResult::Acquired { proof, claim }) => {
                StoredResult::Claim(StoredClaimResult::Acquired {
                    proof: proof.into(),
                    claim,
                })
            }
            MutationResult::Landing(LandingResult::Acquired { proof, landing }) => {
                StoredResult::Landing(StoredLandingResult::Acquired {
                    proof: proof.into(),
                    landing,
                })
            }
            other => StoredResult::Other(other),
        })
    }
}
impl From<StoredMutationResult> for MutationResult {
    fn from(value: StoredMutationResult) -> Self {
        match value.0 {
            StoredResult::Task(StoredTaskResult::Claimed { proof }) => {
                Self::Task(TaskResult::Claimed {
                    proof: proof.into(),
                })
            }
            StoredResult::Claim(StoredClaimResult::Acquired { proof, claim }) => {
                Self::Claim(ClaimResult::Acquired {
                    proof: proof.into(),
                    claim,
                })
            }
            StoredResult::Landing(StoredLandingResult::Acquired { proof, landing }) => {
                Self::Landing(LandingResult::Acquired {
                    proof: proof.into(),
                    landing,
                })
            }
            StoredResult::Other(other) => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sqlite_receipts_keep_byte_tokens_and_millisecond_requests() {
        let proof = TaskProof {
            task: TaskId::from_bytes([1; 16]),
            generation: 2,
            token: [0xab; 16],
        };
        let mutation = Mutation::Task(TaskInput::Claim {
            id: proof.task,
            lease_seconds: 3,
        });
        assert_eq!(
            serde_json::to_value(StoredMutation::from(&mutation)).unwrap(),
            json!({"domain":"task","input":{"op":"claim","id":proof.task,"lease_ms":3000}})
        );
        let mutation = Mutation::Task(TaskInput::Release {
            proof: proof.clone(),
        });
        let value = serde_json::to_value(StoredMutation::from(&mutation)).unwrap();
        assert_eq!(value["input"]["proof"]["token"], json!(vec![0xab; 16]));
        let stored = StoredMutationResult::from(MutationResult::Task(TaskResult::Claimed {
            proof: proof.clone(),
        }));
        let value = serde_json::to_value(stored).unwrap();
        assert_eq!(
            value,
            json!({"result":"claimed","proof":{"task":proof.task,"generation":2,"token":vec![0xab;16]}})
        );
        let stored: StoredMutationResult = serde_json::from_value(value).unwrap();
        let MutationResult::Task(TaskResult::Claimed { proof: actual }) =
            MutationResult::from(stored)
        else {
            panic!("task proof")
        };
        assert_eq!(actual, proof);
        assert_eq!(
            serde_json::to_value(actual).unwrap()["token"],
            "ab".repeat(16)
        );
    }
}
