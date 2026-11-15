//! Shared, declaration-shaped fixtures for landing tests.
use super::{
    LandingError,
    approval::Approvals,
    driver::{self, Report, Request},
    fakes::{FakeAlarms, FakeForge, FakeGate, FakeRepo},
    model::*,
    policy::LandPolicy,
    ports::{Approval, PullRequestInfo, PullRequestState, Repo},
};
use provenant_cli::domains::{GateKind, LandOverride, LandSettings};
use std::collections::BTreeMap;

/// The common builder for landing boundary tests.
pub(crate) struct LandingFixture;

impl LandingFixture {
    pub(crate) fn pr(number: u64, head: &str) -> EligiblePr {
        EligiblePr {
            number,
            head: head.into(),
            approval: format!("approval-{number}"),
            approval_evidence: Vec::new(),
            base_branch: "main".into(),
            accepted: None,
            extra: Default::default(),
        }
    }

    pub(crate) fn step(pr: EligiblePr, commit: &str, tree: &str) -> CandidateStep {
        CandidateStep {
            pr,
            commit: commit.into(),
            tree: tree.into(),
            extra: Default::default(),
        }
    }

    pub(crate) fn candidate(commit: &str) -> Candidate {
        Self::candidate_named(commit, "t", &format!("land/{commit}"))
    }

    pub(crate) fn candidate_named(commit: &str, tree: &str, ref_name: &str) -> Candidate {
        Candidate {
            base: "base".into(),
            steps: Vec::new(),
            commit: commit.into(),
            tree: tree.into(),
            ref_name: ref_name.into(),
            extra: Default::default(),
        }
    }

    pub(crate) fn ordered_candidate() -> Candidate {
        Candidate {
            base: "base".into(),
            steps: vec![
                Self::step(Self::pr(1, "head-1"), "step-1", "tree-1"),
                Self::step(Self::pr(2, "head-2"), "candidate", "tree-2"),
            ],
            commit: "candidate".into(),
            tree: "tree-2".into(),
            ref_name: "land/test".into(),
            extra: Default::default(),
        }
    }

    pub(crate) fn candidate_with_steps(count: usize) -> Candidate {
        let steps = (0..count)
            .map(|index| {
                let number = index as u64;
                Self::step(Self::pr(number, &format!("h{number}")), "c", "t")
            })
            .collect();
        Candidate {
            base: "base".into(),
            steps,
            commit: "c".into(),
            tree: "t".into(),
            ref_name: "land/1".into(),
            extra: Default::default(),
        }
    }

    pub(crate) fn definition() -> GateDefinition {
        Self::definition_for(&["ci-status"], "d", None, 60)
    }

    pub(crate) fn definition_for(
        checks: &[&str],
        digest: &str,
        issuer: Option<&str>,
        timeout_seconds: u64,
    ) -> GateDefinition {
        GateDefinition {
            kind: GateKind::Checks,
            digest: digest.into(),
            checks: checks.iter().map(|check| (*check).into()).collect(),
            commands: Vec::new(),
            issuer: issuer.map(str::to_owned),
            executor: None,
            coverage: Vec::new(),
            timeout_seconds,
            pool: None,
            env_passthrough: Vec::new(),
        }
    }

    pub(crate) fn evidence(commit: &str, result: GateResult, issuer: Option<&str>) -> GateEvidence {
        GateEvidence {
            commit: commit.into(),
            tree: "t".into(),
            definition_digest: "d".into(),
            checks: vec![CheckEvidence {
                name: "ci-status".into(),
                issuer: issuer.map(str::to_owned),
                result,
            }],
            issuer: None,
            coverage: Vec::new(),
            result,
            pool: None,
            duration_seconds: None,
            extra: Default::default(),
        }
    }

    pub(crate) fn policy() -> LandPolicy {
        LandPolicy::resolve(
            &LandSettings::default(),
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )
        .expect("default policy resolves")
    }

    pub(crate) fn receipt(commit: &str, shadow: bool) -> LandingReceipt {
        LandingReceipt {
            policy: Self::policy(),
            base: "base".into(),
            candidate: commit.into(),
            tree: "t".into(),
            candidate_ref: format!("land/{commit}"),
            candidate_steps: Vec::new(),
            pull_requests: Vec::new(),
            gate_definition: Self::definition(),
            gate_evidence: Self::evidence(commit, GateResult::Passed, None),
            publish: PublishDecision {
                mode: PublishMode::FastForward,
                reason: "test".into(),
                extra: Default::default(),
            },
            outcomes: BTreeMap::new(),
            split: Vec::new(),
            approved_to_landed: BTreeMap::from([("head".into(), "landed".into())]),
            cost: CostEstimate::Unknown {
                reason: "x".into(),
                extra: Default::default(),
            },
            alarms: Vec::new(),
            shadow,
            extra: Default::default(),
        }
    }

    pub(crate) fn driver_case(
        count: u64,
    ) -> (FakeForge, FakeRepo, FakeGate, LandSettings, GateDefinition) {
        let mut forge = FakeForge {
            direct_push: true,
            target_branch: "main".into(),
            ..FakeForge::default()
        };
        for number in 1..=count {
            let head = format!("{number:040x}");
            forge.pull_requests.insert(
                number,
                PullRequestInfo {
                    number,
                    head: head.clone(),
                    base_branch: "main".into(),
                    author: "author".into(),
                    state: PullRequestState::Open,
                    merged_commit: None,
                    stack: None,
                },
            );
            forge.heads.insert(number, head.clone());
            forge.approvals_at_head.insert(
                (number, head),
                vec![Approval {
                    reference: format!("approval-{number}"),
                    reviewer: "reviewer".into(),
                }],
            );
        }
        let repo = FakeRepo {
            target: "a".repeat(40),
            valid_commit_ids: true,
            ..FakeRepo::default()
        };
        let gate = FakeGate {
            auto: true,
            ..FakeGate::default()
        };
        let settings = LandSettings {
            batch_wait_seconds: 0,
            publish_settle_seconds: 0,
            pr_ci: provenant_cli::domains::PrCiSettings {
                mode: provenant_cli::domains::PrCiMode::None,
                ..Default::default()
            },
            ..LandSettings::default()
        };
        let definition = Self::definition();
        (forge, repo, gate, settings, definition)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "test fixture supplies independent ports"
    )]
    pub(crate) fn exercise_with(
        forge: &mut FakeForge,
        repo: &mut FakeRepo,
        runner: &mut FakeGate,
        settings: &LandSettings,
        definition: &GateDefinition,
        numbers: &[u64],
        shadow: bool,
        state: &std::path::Path,
        approvals: Approvals<'_>,
    ) -> Result<Report, LandingError> {
        let mut policy = LandPolicy::resolve(
            settings,
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )
        .map_err(|error| LandingError::Failed(error.to_string()))?;
        policy.publication_port = super::resolve_land_port(repo.project_path(), state)?;
        driver::run(
            forge,
            repo,
            runner,
            &mut FakeAlarms::default(),
            Request {
                pins: &Default::default(),
                numbers,
                target: "main",
                settings,
                policy: &policy,
                gate: definition,
                shadow,
                state,
                approvals,
            },
        )
        .map_err(|error| match error {
            LandingError::RunFailed { cause, .. } => *cause,
            error => error,
        })
    }
}
