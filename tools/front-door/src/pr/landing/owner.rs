//! The one landing owner (contract C4).
//!
//! Every publisher submits a [`LandRequest`] here: `provenant land`, a graph
//! land node and, through it, graph delivery. The owner pins each member's
//! head and base, keys the request by them, and drives the candidate driver
//! (CI wait, rebase onto the target, gate, merge, local clean-up). Its record
//! under `landing/requests/<id>.json` holds one outcome per member, so a
//! resubmission is idempotent (a landed member is never landed again) and a
//! restart resumes from the record and the driver's publication markers.
//!
//! The request already carries what later slices need: a queue (#3127) reads
//! the records, a stack (#3133) submits its members in order, and a batch on a
//! base other than the default (#3191) pins that base. This slice lands one
//! target per request and refuses a member pinned to another base.
use super::{
    LandingError,
    approval::Approvals,
    driver,
    model::{GateDefinition, Outcome, Pin},
    policy::LandPolicy,
    ports::{AlarmSink, Forge, GateRunner, PullRequestState, Repo},
    receipt,
};
use provenant_cli::domains::LandSettings;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What lets a member land beyond the landing policy's own approval.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReviewProof {
    /// The policy's approval source decides.
    #[default]
    Policy,
    /// The chair accepts a delta no review covers.
    Accepted { reason: String },
}

/// One pull request of a landing request. A head or base left unset is
/// pinned to the live value when the request is submitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandMember {
    pub pr: u64,
    pub head: Option<String>,
    pub base: Option<String>,
    #[serde(default)]
    pub review_proof: ReviewProof,
}

/// Who decided the landing. `Observe` only records whether someone else
/// merged the pinned head; it never publishes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    Observe,
    #[default]
    Policy,
    Chair,
}

/// The policy selection the caller resolved, recorded so a resume is read
/// against the same choice.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyChoice {
    pub tier: Option<String>,
    pub poll_seconds: Option<u64>,
    /// How long this call waits on CI before it records a member pending.
    pub patience_seconds: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandRequest {
    pub members: Vec<LandMember>,
    pub authority: Authority,
    pub policy: PolicyChoice,
}

/// A landing's durable identity: the repository, target and pinned members.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LandingId(String);
impl LandingId {
    fn of(repository: &str, target: &str, members: &[LandMember]) -> Self {
        use sha2::{Digest, Sha256};
        let mut pinned: Vec<_> = members
            .iter()
            .map(|member| (member.pr, member.head.as_deref(), member.base.as_deref()))
            .collect();
        pinned.sort_unstable();
        let mut digest = Sha256::new();
        digest.update(repository.as_bytes());
        digest.update([0]);
        digest.update(target.as_bytes());
        for (pr, head, base) in pinned {
            digest.update(format!(
                "\0{pr}\0{}\0{}",
                head.unwrap_or(""),
                base.unwrap_or("")
            ));
        }
        Self(
            digest.finalize()[..16]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        )
    }
}
impl std::fmt::Display for LandingId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One member's outcome. Only `Landed` is settled: a pending, unknown or
/// refused member is driven again by a resubmission, and the driver's
/// publication markers keep that from publishing anything twice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MemberOutcome {
    Pending {
        reason: String,
    },
    Landed {
        commit: Option<String>,
    },
    /// Publication may have happened; the next submission reconciles it.
    Unknown {
        reason: String,
    },
    Refused {
        reason: String,
    },
}
impl MemberOutcome {
    pub fn landed(&self) -> bool {
        matches!(self, Self::Landed { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingRecord {
    pub schema_version: u32,
    pub id: LandingId,
    pub repository: String,
    pub target: String,
    pub request: LandRequest,
    pub outcomes: BTreeMap<u64, MemberOutcome>,
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Submissions that advanced this landing.
    pub passes: u64,
}

/// The landing port every publisher uses.
pub trait LandingOwner {
    /// Pin, key and advance a landing as far as it goes in one bounded call.
    fn submit(&mut self, request: LandRequest) -> Result<LandingId, LandingError>;
    /// The durable record of a submitted landing.
    fn record(&self, id: &LandingId) -> Result<Option<LandingRecord>, LandingError>;
}

/// The driver-backed owner. Policy, gate and approval sources are resolved
/// by the adapter that builds it; the owner holds no configuration of its own.
pub struct Owner<'a> {
    pub forge: &'a mut dyn Forge,
    pub repo: &'a mut dyn Repo,
    pub runner: &'a mut dyn GateRunner,
    pub alarms: &'a mut dyn AlarmSink,
    pub state: &'a Path,
    pub settings: &'a LandSettings,
    pub policy: &'a LandPolicy,
    pub gate: &'a GateDefinition,
    pub approvals: Approvals<'a>,
    /// Shadow runs never publish and keep no landing record.
    pub shadow: bool,
    /// The last driver pass, for an adapter that renders it.
    pub last: Option<(driver::Report, Option<LandingError>)>,
}

fn record_path(state: &Path, id: &LandingId) -> PathBuf {
    state
        .join("landing")
        .join("requests")
        .join(format!("{id}.json"))
}

pub fn read_record(state: &Path, id: &LandingId) -> Result<Option<LandingRecord>, LandingError> {
    match std::fs::read(record_path(state, id)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| LandingError::Failed(format!("read landing record {id}: {error}"))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(LandingError::Failed(format!(
            "read landing record {id}: {error}"
        ))),
    }
}

fn write_record(state: &Path, record: &LandingRecord) -> Result<(), LandingError> {
    let bytes = serde_json::to_vec_pretty(record)
        .map_err(|error| LandingError::Failed(format!("encode landing record: {error}")))?;
    receipt::atomic_write(&record_path(state, &record.id), &bytes)
}

/// A driver failure that a later submission can get past without a change.
fn transient(error: &LandingError) -> bool {
    matches!(
        error,
        LandingError::Pending(_)
            | LandingError::Busy
            | LandingError::RateLimited(_)
            | LandingError::CiUnavailable(_)
            | LandingError::Contention
            | LandingError::TargetMoved
    )
}

/// Each member's outcome from one driver pass.
pub(super) fn member_outcomes(
    members: &[LandMember],
    report: &driver::Report,
    failure: Option<&LandingError>,
) -> BTreeMap<u64, MemberOutcome> {
    let cause = failure.map(ToString::to_string);
    let cause = || cause.clone().unwrap_or_else(|| "not reached".into());
    members
        .iter()
        .map(|member| {
            let reason = report.exclusion_reasons.get(&member.pr).cloned();
            let cleanup_unsettled = report.local_steps.get(&member.pr).is_some_and(|local| {
                local["cleanup_pending"] == true || local["error"].is_string()
            });
            let outcome = match report.outcomes.get(&member.pr) {
                Some(Outcome::Landed) if cleanup_unsettled => MemberOutcome::Pending {
                    reason: "local cleanup is pending; resubmit to retry it".into(),
                },
                Some(Outcome::Landed) => MemberOutcome::Landed {
                    commit: member.head.as_ref().and_then(|head| {
                        report
                            .receipts
                            .iter()
                            .find_map(|receipt| receipt.approved_to_landed.get(head).cloned())
                    }),
                },
                Some(Outcome::PendingUnknown) => MemberOutcome::Unknown { reason: cause() },
                Some(Outcome::Pending) => MemberOutcome::Pending { reason: cause() },
                Some(Outcome::Red) => MemberOutcome::Refused {
                    reason: report
                        .warnings
                        .iter()
                        .find(|warning| warning.contains(&format!("#{}:", member.pr)))
                        .cloned()
                        .unwrap_or_else(|| "failed gate checks".into()),
                },
                Some(Outcome::Conflicted) => MemberOutcome::Refused {
                    reason: format!("pull request #{} conflicts with the target", member.pr),
                },
                Some(Outcome::Excluded) | None => match (reason, failure) {
                    (Some(reason), _) => MemberOutcome::Refused { reason },
                    (None, Some(error)) if transient(error) => {
                        MemberOutcome::Pending { reason: cause() }
                    }
                    (None, Some(_)) => MemberOutcome::Refused { reason: cause() },
                    (None, None) => MemberOutcome::Pending { reason: cause() },
                },
            };
            (member.pr, outcome)
        })
        .collect()
}

impl Owner<'_> {
    /// Fill each member's unset head and base from the forge.
    fn pin(&mut self, members: &mut [LandMember]) -> Result<(), LandingError> {
        for member in members.iter_mut() {
            if member.head.is_none() || member.base.is_none() {
                let info = self.forge.pull_request(member.pr)?;
                member.head.get_or_insert(info.head);
                member.base.get_or_insert(info.base_branch);
            }
        }
        Ok(())
    }

    /// Record whether the pinned heads were merged by someone else.
    fn observe(
        &mut self,
        members: &[LandMember],
    ) -> Result<BTreeMap<u64, MemberOutcome>, LandingError> {
        let mut outcomes = BTreeMap::new();
        for member in members {
            let info = self.forge.pull_request(member.pr)?;
            let head = member.head.as_deref().unwrap_or_default();
            let outcome = if info.head != head {
                MemberOutcome::Refused {
                    reason: format!(
                        "HEAD_MOVED: pull request #{} head moved to {}",
                        member.pr, info.head
                    ),
                }
            } else {
                match info.state {
                    PullRequestState::Merged => MemberOutcome::Landed {
                        commit: info.merged_commit,
                    },
                    PullRequestState::Open => MemberOutcome::Pending {
                        reason: format!(
                            "pull request #{} awaits the chair; next: provenant land {} --head {head}",
                            member.pr, member.pr
                        ),
                    },
                    PullRequestState::Closed => MemberOutcome::Refused {
                        reason: format!(
                            "PR_CLOSED: pull request #{} is closed without merging",
                            member.pr
                        ),
                    },
                }
            };
            outcomes.insert(member.pr, outcome);
        }
        Ok(outcomes)
    }

    /// One driver pass over the members not yet landed.
    fn drive(&mut self, members: &[LandMember], target: &str) -> BTreeMap<u64, MemberOutcome> {
        let numbers: Vec<u64> = members.iter().map(|member| member.pr).collect();
        let pins: BTreeMap<u64, Pin> = members
            .iter()
            .map(|member| {
                (
                    member.pr,
                    Pin {
                        head: member.head.clone(),
                        base: member.base.clone(),
                    },
                )
            })
            .collect();
        let acceptances: BTreeMap<u64, String> = members
            .iter()
            .filter_map(|member| match &member.review_proof {
                ReviewProof::Policy => None,
                ReviewProof::Accepted { reason } => Some((member.pr, reason.clone())),
            })
            .collect();
        let result = driver::run(
            &mut *self.forge,
            &mut *self.repo,
            &mut *self.runner,
            &mut *self.alarms,
            driver::Request {
                numbers: &numbers,
                target,
                settings: self.settings,
                policy: self.policy,
                gate: self.gate,
                shadow: self.shadow,
                state: self.state,
                approvals: self.approvals.with_acceptances(&acceptances),
                pins: &pins,
            },
        );
        let (report, failure) = match result {
            Ok(report) => (report, None),
            Err(LandingError::RunFailed { cause, report }) => (*report, Some(*cause)),
            Err(error) => (driver::Report::default(), Some(error)),
        };
        let outcomes = member_outcomes(members, &report, failure.as_ref());
        self.last = Some((report, failure));
        outcomes
    }
}

impl LandingOwner for Owner<'_> {
    fn submit(&mut self, mut request: LandRequest) -> Result<LandingId, LandingError> {
        if request.members.is_empty() {
            return Err(LandingError::Failed("no pull requests selected".into()));
        }
        let repository = self.forge.repository_identity()?;
        let target = self.forge.target_branch()?;
        self.pin(&mut request.members)?;
        let id = LandingId::of(&repository, &target, &request.members);
        let mut record = if self.shadow {
            None
        } else {
            read_record(self.state, &id)?
        }
        .unwrap_or_else(|| LandingRecord {
            schema_version: 1,
            id: id.clone(),
            repository,
            target: target.clone(),
            request: request.clone(),
            outcomes: BTreeMap::new(),
            warnings: Vec::new(),
            passes: 0,
        });
        // Landed members replay from the record; the rest advance again.
        let mut todo: Vec<LandMember> = record
            .request
            .members
            .iter()
            .filter(|member| {
                !record
                    .outcomes
                    .get(&member.pr)
                    .is_some_and(MemberOutcome::landed)
            })
            .cloned()
            .collect();
        if todo.is_empty() {
            self.last = None;
            return Ok(id);
        }
        // A request names one target; another base waits for #3191.
        todo.retain(|member| {
            let other = member.base.as_deref().filter(|base| *base != target);
            if let Some(base) = other {
                record.outcomes.insert(
                    member.pr,
                    MemberOutcome::Refused {
                        reason: format!(
                            "pull request #{} targets {base}, not the landing target {target}",
                            member.pr
                        ),
                    },
                );
            }
            other.is_none()
        });
        // The latest submission's authority and owner acceptances decide this pass.
        record.request.authority = request.authority;
        for member in &mut todo {
            if let Some(fresh) = request.members.iter().find(|fresh| fresh.pr == member.pr) {
                member.review_proof = fresh.review_proof.clone();
            }
        }
        self.last = None;
        let outcomes = match request.authority {
            Authority::Observe => self.observe(&todo)?,
            Authority::Policy | Authority::Chair => self.drive(&todo, &target),
        };
        record.outcomes.extend(outcomes);
        if let Some((report, _)) = &self.last {
            record.warnings = report.warnings.clone();
        }
        record.passes += 1;
        if !self.shadow {
            write_record(self.state, &record)?;
            if let Err(error) =
                crate::custody::settle_landing(self.repo.project_path(), self.state, &record)
            {
                record
                    .warnings
                    .push(format!("custody settlement pending: {error}"));
                write_record(self.state, &record)?;
            }
        }
        Ok(id)
    }

    fn record(&self, id: &LandingId) -> Result<Option<LandingRecord>, LandingError> {
        read_record(self.state, id)
    }
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "custom worktree harness includes tests without the test runner"
)]
mod tests {
    //! Invariants at the port: one record per landing with per-member
    //! outcomes, idempotent resubmission, and a restart that resumes.
    use super::*;
    use crate::pr::landing::fakes::*;
    use crate::pr::landing::model::GateResult;
    use crate::pr::landing::policy::LandPolicy;
    use provenant_cli::domains::LandOverride;

    struct World {
        forge: FakeForge,
        repo: FakeRepo,
        gate: FakeGate,
        alarms: FakeAlarms,
        settings: LandSettings,
        policy: LandPolicy,
        definition: GateDefinition,
        state: tempfile::TempDir,
    }

    fn world(count: u64) -> Result<World, Box<dyn std::error::Error>> {
        let (forge, repo, gate, settings, definition) =
            crate::pr::landing::driver::tests::fixture(count);
        let policy = LandPolicy::resolve(
            &settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )?;
        Ok(World {
            forge,
            repo,
            gate,
            alarms: FakeAlarms::default(),
            settings,
            policy,
            definition,
            state: tempfile::tempdir()?,
        })
    }

    fn submit(
        world: &mut World,
        request: LandRequest,
    ) -> Result<(LandingId, LandingRecord), Box<dyn std::error::Error>> {
        let mut owner = Owner {
            forge: &mut world.forge,
            repo: &mut world.repo,
            runner: &mut world.gate,
            alarms: &mut world.alarms,
            state: world.state.path(),
            settings: &world.settings,
            policy: &world.policy,
            gate: &world.definition,
            approvals: Approvals::forge(),
            shadow: false,
            last: None,
        };
        let id = owner.submit(request)?;
        let record = owner.record(&id)?.ok_or("no landing record")?;
        Ok((id, record))
    }

    fn members(numbers: &[u64]) -> Vec<LandMember> {
        numbers
            .iter()
            .map(|pr| LandMember {
                pr: *pr,
                head: None,
                base: None,
                review_proof: ReviewProof::Policy,
            })
            .collect()
    }

    #[test]
    fn a_single_pull_request_lands_into_one_record() -> Result<(), Box<dyn std::error::Error>> {
        let mut world = world(1)?;
        let (id, record) = submit(
            &mut world,
            LandRequest {
                members: members(&[1]),
                ..Default::default()
            },
        )?;
        assert_eq!(record.id, id);
        assert_eq!(record.outcomes.len(), 1);
        assert!(record.outcomes[&1].landed(), "{record:?}");
        // The pinned head is part of the record.
        assert!(record.request.members[0].head.is_some());
        Ok(())
    }

    #[test]
    fn explicit_owner_acceptance_lands_members_into_one_record()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut world = world(2)?;
        world.forge.approvals_at_head.clear();
        let mut members = members(&[1, 2]);
        for member in &mut members {
            member.review_proof = ReviewProof::Accepted {
                reason: "owner accepts uncovered delta".into(),
            };
        }
        let (_, record) = submit(
            &mut world,
            LandRequest {
                members,
                authority: Authority::Chair,
                policy: PolicyChoice::default(),
            },
        )?;
        assert_eq!(record.outcomes.len(), 2);
        assert!(
            record.outcomes.values().all(MemberOutcome::landed),
            "{record:?}"
        );
        Ok(())
    }

    #[test]
    fn resubmission_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
        let mut world = world(1)?;
        let request = LandRequest {
            members: members(&[1]),
            ..Default::default()
        };
        let (first, _) = submit(&mut world, request.clone())?;
        let merges = world.forge.merges.len();
        let pushes = world.repo.push_attempts.len();
        let (again, record) = submit(&mut world, request)?;
        assert_eq!(first, again);
        assert_eq!(world.forge.merges.len(), merges);
        assert_eq!(world.repo.push_attempts.len(), pushes);
        assert_eq!(record.passes, 1);
        Ok(())
    }

    #[test]
    fn a_restart_resumes_a_pending_member() -> Result<(), Box<dyn std::error::Error>> {
        let mut world = world(1)?;
        world.definition.timeout_seconds = 0;
        world.gate.check_result = Some(GateResult::Pending);
        let request = LandRequest {
            members: members(&[1]),
            ..Default::default()
        };
        let (id, record) = submit(&mut world, request.clone())?;
        assert!(
            matches!(record.outcomes[&1], MemberOutcome::Pending { .. }),
            "{record:?}"
        );
        // A new process reads the same record and finishes the landing.
        world.gate.check_result = None;
        let (resumed, record) = submit(&mut world, request)?;
        assert_eq!(resumed, id);
        assert!(record.outcomes[&1].landed(), "{record:?}");
        assert_eq!(record.passes, 2);
        Ok(())
    }

    #[test]
    fn a_moved_head_is_refused_not_landed() -> Result<(), Box<dyn std::error::Error>> {
        let mut world = world(1)?;
        let mut members = members(&[1]);
        members[0].head = Some(format!("{:040x}", 99));
        let (_, record) = submit(
            &mut world,
            LandRequest {
                members,
                ..Default::default()
            },
        )?;
        assert!(
            matches!(&record.outcomes[&1], MemberOutcome::Refused { reason } if reason.contains("HEAD_MOVED")),
            "{record:?}"
        );
        assert!(world.forge.merges.is_empty());
        Ok(())
    }
}
