//! What stands as a pull request's landing approval, decided once for single
//! and batch landing: configured independent review sources on the exact head.
use super::{
    LandingError,
    model::{Accepted, ApprovalEvidence},
    ports::{Forge, MergeTree, Repo, ReviewRecords},
};
use provenant_cli::domains::{ApprovalSource, ApprovalTier};
use provenant_confinement::review_exempt::ReviewExempt;
use provenant_core::review_cover::{self, ReviewGit, Reviewed};
use std::collections::BTreeMap;

/// The approval source and tier of one landing run.
#[derive(Clone, Copy)]
pub struct Approvals<'a> {
    warnings: Option<&'a std::cell::RefCell<Vec<String>>>,
    sources: &'a [ApprovalSource],
    records: &'a dyn ReviewRecords,
    tier: ApprovalTier,
    exempt: Option<&'a ReviewExempt>,
    /// The reason the chair accepts a delta no review covers.
    accept_delta: Option<&'a str>,
    /// Explicit per-member owner acceptances carried by a landing request.
    acceptances: Option<&'a BTreeMap<u64, String>>,
    carry_state: Option<&'a std::path::Path>,
    carry_kinds: &'a [provenant_cli::domains::ApprovalCarry],
}

/// What lets a pull request land at its head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Approved(Approved),
    Refused(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approved {
    pub reference: String,
    pub evidence: Vec<ApprovalEvidence>,
    pub accepted: Option<Accepted>,
}

impl Approved {
    fn by(reference: impl Into<String>) -> Self {
        Self {
            reference: reference.into(),
            evidence: Vec::new(),
            accepted: None,
        }
    }
}

#[cfg(test)]
struct NoRecords;
#[cfg(test)]
impl ReviewRecords for NoRecords {
    fn reviews(&self, _number: u64, _head: &str) -> Result<super::ports::Reviews, LandingError> {
        Ok(super::ports::Reviews::default())
    }
}

impl<'a> Approvals<'a> {
    pub fn new(sources: &'a [ApprovalSource], records: &'a dyn ReviewRecords) -> Self {
        Self {
            warnings: None,
            sources,
            records,
            tier: ApprovalTier::Auto,
            exempt: None,
            accept_delta: None,
            acceptances: None,
            carry_state: None,
            carry_kinds: &[],
        }
    }

    /// Keep source diagnostics in the caller's structured landing report,
    /// including successful approval by another usable source.
    pub fn with_warnings<'b>(self, warnings: &'b std::cell::RefCell<Vec<String>>) -> Approvals<'b>
    where
        'a: 'b,
    {
        Approvals {
            warnings: Some(warnings),
            ..self
        }
    }

    /// Carry each member's explicit owner acceptance.
    pub fn with_acceptances<'b>(self, acceptances: &'b BTreeMap<u64, String>) -> Approvals<'b>
    where
        'a: 'b,
    {
        Approvals {
            warnings: self.warnings,
            sources: self.sources,
            records: self.records,
            tier: self.tier,
            exempt: self.exempt,
            accept_delta: self.accept_delta,
            acceptances: Some(acceptances),
            carry_state: self.carry_state,
            carry_kinds: self.carry_kinds,
        }
    }

    pub fn with_carry(
        self,
        state: &'a std::path::Path,
        kinds: &'a [provenant_cli::domains::ApprovalCarry],
    ) -> Self {
        Approvals {
            carry_state: Some(state),
            carry_kinds: kinds,
            ..self
        }
    }

    /// Apply the landing tier, the review-exempt paths and the chair's override.
    pub fn governed(
        self,
        tier: ApprovalTier,
        exempt: &'a ReviewExempt,
        accept_delta: Option<&'a str>,
    ) -> Self {
        Self {
            tier,
            exempt: Some(exempt),
            accept_delta,
            ..self
        }
    }

    /// Approving forge reviews only.
    #[cfg(test)]
    pub fn forge() -> Approvals<'static> {
        Approvals {
            warnings: None,
            sources: &[ApprovalSource::Forge],
            records: &NoRecords,
            tier: ApprovalTier::Carry,
            exempt: None,
            accept_delta: None,
            acceptances: None,
            carry_state: None,
            carry_kinds: &[],
        }
    }

    /// Whether the run must read the target to decide an approval.
    pub fn needs_target(&self) -> bool {
        self.tier == ApprovalTier::Auto
            || (self.tier != ApprovalTier::None && !self.carry_kinds.is_empty())
    }

    fn production(&self, path: &str) -> bool {
        !(self
            .exempt
            .is_some_and(|exempt| exempt.covers(path.as_bytes()))
            || test_or_doc(path))
    }

    /// Whether `paths` is a change to no production code.
    fn non_production(&self, paths: &[String]) -> bool {
        !paths.is_empty() && paths.iter().all(|path| !self.production(path))
    }
}

impl<'a> Approvals<'a> {
    /// Bind to the repository and fetched target the proof reads.
    pub fn at<'r>(self, repo: &'r mut dyn Repo, target: &'r str) -> Approver<'a, 'r> {
        Approver {
            approvals: self,
            repo,
            target,
        }
    }
}

/// [`Approvals`] bound to a repository and the fetched target commit.
pub struct Approver<'a, 'r> {
    approvals: Approvals<'a>,
    repo: &'r mut dyn Repo,
    target: &'r str,
}

impl Approver<'_, '_> {
    /// Decide whether `head` of pull request `number`, by `author`, may land.
    pub fn decide(
        &mut self,
        forge: &mut dyn Forge,
        number: u64,
        head: &str,
        author: &str,
    ) -> Result<Decision, LandingError> {
        let approvals = self.approvals;
        let tier = match approvals.tier {
            ApprovalTier::Auto => {
                let paths = match self.repo.merge_base(head, self.target)? {
                    Some(base) => self.repo.changed_paths(&base, head)?,
                    None => Vec::new(),
                };
                if approvals.non_production(&paths) {
                    ApprovalTier::None
                } else {
                    ApprovalTier::Carry
                }
            }
            tier => tier,
        };
        if tier == ApprovalTier::None {
            return Ok(Decision::Approved(Approved::by("ci-only")));
        }
        let mut unusable = Vec::new();
        let mut evidence_set = Vec::new();
        for source in approvals.sources {
            let candidates = match source {
                ApprovalSource::ProvenantReview => match approvals.records.reviews(number, head) {
                    Ok(reviews) => {
                        unusable.extend(reviews.unusable);
                        reviews
                            .records
                            .into_iter()
                            .filter(|record| {
                                record.head == head
                                    && record.independent
                                    && matches!(record.verdict,
                                    provenant_records::workflow::ReviewVerdict::Approve
                                    | provenant_records::workflow::ReviewVerdict::ApproveWithNits)
                            })
                            .map(|record| ApprovalEvidence {
                                source: *source,
                                reference: record.reference,
                                reviewer: record.reviewer,
                                head: record.head,
                                extra: Default::default(),
                            })
                            .collect::<Vec<_>>()
                    }
                    Err(error) => {
                        unusable.push(format!("{source:?}: {error}"));
                        Vec::new()
                    }
                },
                ApprovalSource::Forge => match forge.approvals(number, head) {
                    Ok(records) => records
                        .into_iter()
                        .filter(|record| {
                            !record.reviewer.is_empty()
                                && !record.reviewer.eq_ignore_ascii_case(author)
                        })
                        .map(|record| ApprovalEvidence {
                            source: *source,
                            reference: record.reference,
                            reviewer: record.reviewer,
                            head: head.to_owned(),
                            extra: Default::default(),
                        })
                        .collect(),
                    Err(error) => {
                        unusable.push(format!("{source:?}: {error}"));
                        Vec::new()
                    }
                },
            };
            for evidence in candidates
                .into_iter()
                .filter(|record| !record.reference.is_empty() && !record.reviewer.is_empty())
            {
                if !evidence_set.contains(&evidence) {
                    evidence_set.push(evidence);
                }
            }
        }
        if let Some(warnings) = approvals.warnings {
            let mut warnings = warnings.borrow_mut();
            for source in &unusable {
                let warning = format!("pull request {number}: review source skipped: {source}");
                if !warnings.contains(&warning) {
                    warnings.push(warning);
                }
            }
        }
        if let Some(first) = evidence_set.first() {
            return Ok(Decision::Approved(Approved {
                reference: first.reference.clone(),
                evidence: evidence_set,
                accepted: None,
            }));
        }
        let accepted = approvals
            .acceptances
            .and_then(|accepted| accepted.get(&number))
            .map(String::as_str)
            .or(approvals.accept_delta);
        if let Some(reason) = accepted {
            return Ok(Decision::Approved(Approved {
                accepted: Some(Accepted {
                    reason: reason.to_owned(),
                    actor: forge.actor()?,
                    extra: Default::default(),
                }),
                ..Approved::by("accepted-delta")
            }));
        }
        let repository = if approvals.carry_state.is_some() {
            match forge.repository_identity() {
                Ok(repository) => Some(repository),
                Err(error) => {
                    unusable.push(format!("repository identity unavailable: {error}"));
                    None
                }
            }
        } else {
            None
        };
        let mut carry_context = None;
        if let (Some(state), Some(repository)) = (approvals.carry_state, repository.as_deref()) {
            let receipt =
                match super::receipt::read_approval_carry(state, repository, number, head)? {
                    Some(receipt) => Some(receipt),
                    None => super::receipt::read_latest_approval_carry(state, repository, number)?,
                };
            if let Some(receipt) = receipt {
                carry_context = Some(receipt.clone());
                let enabled = match receipt.kind.as_str() {
                    "clean_merge" => approvals
                        .carry_kinds
                        .contains(&provenant_cli::domains::ApprovalCarry::CleanMerge),
                    "format" => approvals
                        .carry_kinds
                        .contains(&provenant_cli::domains::ApprovalCarry::Format),
                    _ => false,
                };
                let clean_merge = receipt.kind == "clean_merge"
                    && receipt.current_head == head
                    && clean_merge_preserves_diff(
                        self.repo,
                        &receipt.approved_head,
                        &receipt.base_head,
                        head,
                        self.target,
                    )?;
                let format_only = receipt.kind == "format"
                    && receipt.current_head == head
                    && format_delta_only(
                        state,
                        self.repo.project_path(),
                        &receipt.approved_head,
                        head,
                    );
                if receipt.kind == "format"
                    && !format_only
                    && let Some(warnings) = approvals.warnings
                {
                    warnings.borrow_mut().push(format!("pull request {number}: format approval carry unavailable because the configured formatter is missing, failed, or does not reproduce the current tree; treating the delta as content"));
                }
                let mut carried = Vec::new();
                if enabled && (clean_merge || format_only) {
                    for source in approvals.sources {
                        match source {
                            ApprovalSource::Forge => {
                                match forge.approvals(number, &receipt.approved_head) {
                                    Ok(records) => carried.extend(
                                        records
                                            .into_iter()
                                            .filter(|record| {
                                                !record.reviewer.is_empty()
                                                    && !record.reviewer.eq_ignore_ascii_case(author)
                                            })
                                            .map(|record| ApprovalEvidence {
                                                source: *source,
                                                reference: record.reference,
                                                reviewer: record.reviewer,
                                                head: receipt.approved_head.clone(),
                                                extra: Default::default(),
                                            }),
                                    ),
                                    Err(error) => unusable.push(format!("{source:?}: {error}")),
                                }
                            }
                            ApprovalSource::ProvenantReview => {
                                match approvals.records.reviews(number, &receipt.approved_head) {
                                    Ok(reviews) => {
                                        unusable.extend(reviews.unusable);
                                        carried.extend(reviews.records.into_iter().filter(|record|
                                            record.independent && record.head == receipt.approved_head
                                            && matches!(record.verdict,
                                                provenant_records::workflow::ReviewVerdict::Approve
                                                | provenant_records::workflow::ReviewVerdict::ApproveWithNits))
                                            .map(|record| ApprovalEvidence { source: *source, reference: record.reference, reviewer: record.reviewer, head: record.head, extra: Default::default() }));
                                    }
                                    Err(error) => unusable.push(format!("{source:?}: {error}")),
                                }
                            }
                        }
                    }
                }
                if let Some(first) = carried.first() {
                    if let Some(warnings) = approvals.warnings {
                        warnings.borrow_mut().push(format!("pull request {number}: carried independent approval across {} delta ({})", receipt.kind, receipt.approved_head));
                    }
                    return Ok(Decision::Approved(Approved {
                        reference: first.reference.clone(),
                        evidence: carried,
                        accepted: None,
                    }));
                }
            }
        }
        let parents = self.repo.parents(head)?;
        if let Some(parent) = parents.first().cloned() {
            let prior_receipt = match (approvals.carry_state, repository.as_deref()) {
                (Some(state), Some(repository)) => {
                    super::receipt::read_latest_approval_carry(state, repository, number)?
                }
                _ => None,
            };
            let prior_approved_head = prior_receipt
                .as_ref()
                .map(|receipt| receipt.approved_head.clone());
            let mut approved_head = None;
            let mut carried = Vec::new();
            let mut candidates = Vec::new();
            if let Some(approved) = prior_approved_head.as_ref() {
                candidates.push(approved.clone());
            }
            let mut candidate = Some(parent.clone());
            let mut seen = std::collections::BTreeSet::new();
            while let Some(commit) = candidate {
                if !seen.insert(commit.clone()) {
                    break;
                }
                if !candidates.contains(&commit) {
                    candidates.push(commit.clone());
                }
                candidate = self.repo.parents(&commit)?.first().cloned();
            }
            let mut unavailable_sources = Vec::new();
            for commit in candidates {
                let evidence = approval_evidence_at(
                    approvals.sources,
                    approvals.records,
                    forge,
                    number,
                    &commit,
                    author,
                    &mut unusable,
                    &mut unavailable_sources,
                );
                if !evidence.is_empty() {
                    approved_head = Some(commit);
                    carried = evidence;
                    break;
                }
            }
            let classification_head = approved_head
                .clone()
                .or_else(|| prior_approved_head.clone())
                .unwrap_or_else(|| parent.clone());
            let mut base_head = String::new();
            let mut kind = "content";
            if !carried.is_empty() && parents.len() > 1 {
                base_head = parents[1].clone();
                if approvals
                    .carry_kinds
                    .contains(&provenant_cli::domains::ApprovalCarry::CleanMerge)
                {
                    if clean_merge_preserves_diff(
                        self.repo,
                        &classification_head,
                        &base_head,
                        head,
                        self.target,
                    )? {
                        kind = "clean_merge";
                    }
                }
            } else if !carried.is_empty()
                && approvals
                    .carry_kinds
                    .contains(&provenant_cli::domains::ApprovalCarry::Format)
            {
                if let Some(state) = approvals.carry_state
                    && format_delta_only(
                        state,
                        self.repo.project_path(),
                        &classification_head,
                        head,
                    )
                {
                    kind = "format";
                } else if let Some(warnings) = approvals.warnings {
                    warnings.borrow_mut().push(format!("pull request {number}: format approval carry unavailable because the configured formatter is missing, failed, or does not reproduce the current tree; treating the delta as content"));
                }
            }
            if kind != "content" && !carried.is_empty() && repository.is_some() {
                if let (Some(state), Some(repository)) =
                    (approvals.carry_state, repository.as_deref())
                {
                    super::receipt::write_approval_carry(
                        state,
                        &super::receipt::ApprovalCarryReceipt {
                            repository: repository.to_owned(),
                            number,
                            approved_head: classification_head.clone(),
                            current_head: head.into(),
                            base_head: base_head.clone(),
                            kind: kind.into(),
                            extra: Default::default(),
                        },
                    )?;
                }
                if let Some(warnings) = approvals.warnings {
                    warnings.borrow_mut().push(format!("pull request {number}: carried independent approval across {kind} delta ({classification_head})"));
                }
                let first = &carried[0];
                return Ok(Decision::Approved(Approved {
                    reference: first.reference.clone(),
                    evidence: carried,
                    accepted: None,
                }));
            }
            carry_context = Some(super::receipt::ApprovalCarryReceipt {
                number,
                repository: repository.clone().unwrap_or_default(),
                approved_head: approved_head.or(prior_approved_head).unwrap_or_default(),
                current_head: head.to_owned(),
                base_head,
                kind: kind.into(),
                extra: Default::default(),
            });
        }
        let context = carry_context.as_ref();
        let approved = context
            .map(|receipt| receipt.approved_head.as_str())
            .filter(|head| !head.is_empty())
            .unwrap_or("unknown");
        let paths = match context {
            Some(receipt) if !receipt.approved_head.is_empty() => self
                .repo
                .changed_paths(&receipt.approved_head, head)?
                .join(", "),
            Some(_) => "unavailable".to_owned(),
            None if !parents.is_empty() => self.repo.changed_paths(&parents[0], head)?.join(", "),
            None => "unavailable".to_owned(),
        };
        let reason = format!(
            "no independent approval for approved head {approved} and current head {head}; changed paths: {}{}",
            if paths.is_empty() { "none" } else { &paths },
            if unusable.is_empty() {
                String::new()
            } else {
                format!("; approval sources unavailable: {}", unusable.join("; "))
            },
        );
        Ok(Decision::Refused(reason))
    }
}

fn clean_merge_preserves_diff(
    repo: &mut dyn Repo,
    approved: &str,
    base: &str,
    current: &str,
    target: &str,
) -> Result<bool, LandingError> {
    if !repo.is_ancestor(approved, current)?
        || !repo.is_ancestor(base, current)?
        || !repo.is_ancestor(base, target)?
    {
        return Ok(false);
    }
    let expected_tree = match repo.merge_tree(approved, base)? {
        MergeTree::Tree(tree) => tree,
        MergeTree::Conflict => return Ok(false),
    };
    if repo.tree_of(current)? != expected_tree {
        return Ok(false);
    }
    let Some(pr_base) = repo.merge_base(approved, base)? else {
        return Ok(false);
    };
    let approved_diff = repo.diff_content(&pr_base, approved)?;
    let merged_diff = repo.diff_content(base, current)?;
    Ok(approved_diff == merged_diff)
}

fn approval_evidence_at(
    sources: &[ApprovalSource],
    records: &dyn ReviewRecords,
    forge: &mut dyn Forge,
    number: u64,
    head: &str,
    author: &str,
    unusable: &mut Vec<String>,
    unavailable_sources: &mut Vec<ApprovalSource>,
) -> Vec<ApprovalEvidence> {
    let mut evidence = Vec::new();
    for source in sources {
        if unavailable_sources.contains(source) {
            continue;
        }
        match source {
            ApprovalSource::Forge => match forge.approvals(number, head) {
                Ok(approvals) => evidence.extend(
                    approvals
                        .into_iter()
                        .filter(|approval| {
                            !approval.reviewer.is_empty()
                                && !approval.reviewer.eq_ignore_ascii_case(author)
                        })
                        .map(|approval| ApprovalEvidence {
                            source: *source,
                            reference: approval.reference,
                            reviewer: approval.reviewer,
                            head: head.to_owned(),
                            extra: Default::default(),
                        }),
                ),
                Err(error) => {
                    unusable.push(format!("{source:?}: {error}"));
                    unavailable_sources.push(*source);
                }
            },
            ApprovalSource::ProvenantReview => match records.reviews(number, head) {
                Ok(reviews) => {
                    unusable.extend(reviews.unusable);
                    evidence.extend(
                        reviews
                            .records
                            .into_iter()
                            .filter(|record| {
                                record.independent
                                    && record.head == head
                                    && matches!(
                                        record.verdict,
                                        provenant_records::workflow::ReviewVerdict::Approve
                                            | provenant_records::workflow::ReviewVerdict::ApproveWithNits
                                    )
                            })
                            .map(|record| ApprovalEvidence {
                                source: *source,
                                reference: record.reference,
                                reviewer: record.reviewer,
                                head: record.head,
                                extra: Default::default(),
                            }),
                    );
                }
                Err(error) => {
                    unusable.push(format!("{source:?}: {error}"));
                    unavailable_sources.push(*source);
                }
            },
        }
    }
    evidence
}

fn format_delta_only(
    state: &std::path::Path,
    project: Option<&std::path::Path>,
    approved: &str,
    current: &str,
) -> bool {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let Some(project) = project else { return false };
    let Ok(ports) = provenant_config::PortTable::read_for(Some(project), state) else {
        return false;
    };
    let Some(command) = ports
        .verify
        .tiers
        .values()
        .flat_map(|tier| tier.commands.iter())
        .find(|command| formatter_kind(command).is_some())
    else {
        return false;
    };
    let kind = formatter_kind(command).unwrap_or_default();
    let mut argv: Vec<String> = command
        .split_whitespace()
        .filter(|part| {
            !matches!(
                *part,
                "{packages}" | "{integration_args}" | "--check" | "--"
            )
        })
        .map(str::to_owned)
        .collect();
    match kind {
        "cargo-fmt" => {}
        "prettier" => {
            if !argv
                .iter()
                .any(|arg| matches!(arg.as_str(), "--write" | "-w"))
            {
                argv.push("--write".into());
            }
        }
        "biome" => {
            if !argv
                .iter()
                .any(|arg| matches!(arg.as_str(), "--write" | "-w"))
            {
                argv.push("--write".into());
            }
        }
        "black" | "ruff-format" => {}
        "gofmt" => {
            if !argv.iter().any(|arg| arg == "-w") {
                argv.insert(1, "-w".into());
            }
        }
        _ => return false,
    }
    let workspace = |root: &std::path::Path| root.join(&ports.verify.workspace);
    let root = std::env::temp_dir().join(format!(
        "provenant-approval-format-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let left = root.join("approved");
    let right = root.join("current");
    if std::fs::create_dir_all(&root).is_err() {
        return false;
    }
    let git = provenant_os::git::git_executable().and_then(|path| path.to_str().map(str::to_owned));
    let Some(git) = git else {
        let _ = std::fs::remove_dir_all(&root);
        return false;
    };
    let add = |path: &std::path::Path, commit: &str| {
        let args = [
            git.as_str(),
            "worktree",
            "add",
            "--detach",
            path.to_str().unwrap_or_default(),
            commit,
        ];
        crate::pr::execute_for(project, &args, std::time::Duration::from_secs(300), false)
            .is_ok_and(|result| result.termination == provenant_os::exec::Termination::Exit(0))
    };
    let mut success = add(&left, approved) && add(&right, current);
    for path in [&left, &right] {
        if success {
            let workdir = workspace(path);
            if !workdir.is_dir() {
                success = false;
                continue;
            }
            let args: Vec<&str> = argv.iter().map(String::as_str).collect();
            success =
                crate::pr::execute_for(&workdir, &args, std::time::Duration::from_secs(300), false)
                    .is_ok_and(|result| {
                        result.termination == provenant_os::exec::Termination::Exit(0)
                    });
        }
    }
    argv.clear();
    let tree = |path: &std::path::Path| -> Option<String> {
        let path = path.to_str()?;
        let args = [git.as_str(), "-C", path, "add", "-A"];
        let staged =
            crate::pr::execute_for(project, &args, std::time::Duration::from_secs(60), false)
                .ok()?;
        if staged.termination != provenant_os::exec::Termination::Exit(0) {
            return None;
        }
        let args = [git.as_str(), "-C", path, "write-tree"];
        let result =
            crate::pr::execute_for(project, &args, std::time::Duration::from_secs(60), false)
                .ok()?;
        (result.termination == provenant_os::exec::Termination::Exit(0)).then(|| {
            String::from_utf8_lossy(&result.stdout.bytes)
                .trim()
                .to_owned()
        })
    };
    let same = success && tree(&left).zip(tree(&right)).is_some_and(|(a, b)| a == b);
    for path in [&left, &right] {
        if path.exists() {
            let _ = crate::pr::execute_for(
                project,
                &[
                    git.as_str(),
                    "worktree",
                    "remove",
                    "--force",
                    path.to_str().unwrap_or_default(),
                ],
                std::time::Duration::from_secs(60),
                false,
            );
        }
    }
    let _ = std::fs::remove_dir_all(root);
    same
}

fn formatter_kind(command: &str) -> Option<&'static str> {
    let parts: Vec<_> = command.split_whitespace().collect();
    if parts.iter().any(|part| *part == "fmt")
        && parts
            .first()
            .is_some_and(|program| program.rsplit('/').next() == Some("cargo"))
    {
        Some("cargo-fmt")
    } else if parts.iter().any(|part| *part == "prettier") {
        Some("prettier")
    } else if parts.iter().any(|part| *part == "biome") {
        Some("biome")
    } else if parts.iter().any(|part| *part == "black") {
        Some("black")
    } else if parts.windows(2).any(|pair| pair == ["ruff", "format"]) {
        Some("ruff-format")
    } else if parts
        .first()
        .is_some_and(|program| program.rsplit('/').next() == Some("gofmt"))
    {
        Some("gofmt")
    } else {
        None
    }
}

/// A path that is a test or a doc. Hidden paths, such as CI and agent
/// configuration, are never either.
fn test_or_doc(path: &str) -> bool {
    let Some((name, directories)) = path
        .split('/')
        .collect::<Vec<_>>()
        .split_last()
        .map(|(n, d)| (*n, d.to_vec()))
    else {
        return false;
    };
    if path
        .split('/')
        .any(|part| part.starts_with('.') || part == "..")
    {
        return false;
    }
    directories
        .iter()
        .any(|part| matches!(*part, "tests" | "test" | "docs" | "__tests__"))
        || name.ends_with("_tests.rs")
        || name.ends_with("_test.rs")
        || name == "tests.rs"
        || name.ends_with(".md")
}

struct RepoGit<'a>(&'a mut dyn Repo);

impl ReviewGit for RepoGit<'_> {
    type Error = LandingError;
    fn merge_base(&mut self, left: &str, right: &str) -> Result<Option<String>, LandingError> {
        self.0.merge_base(left, right)
    }
    fn is_ancestor(&mut self, ancestor: &str, descendant: &str) -> Result<bool, LandingError> {
        self.0.is_ancestor(ancestor, descendant)
    }
    fn tree_of(&mut self, commit: &str) -> Result<String, LandingError> {
        self.0.tree_of(commit)
    }
    fn merge_tree(&mut self, left: &str, right: &str) -> Result<Option<String>, LandingError> {
        Ok(match self.0.merge_tree(left, right)? {
            MergeTree::Tree(tree) => Some(tree),
            MergeTree::Conflict => None,
        })
    }
}

/// Prove a graph land head against a fresh fetch of the branch its pull
/// request merges into, as the forge reports it, without moving any reference.
pub(crate) fn graph_coverage(
    project: &std::path::Path,
    host_state: &std::path::Path,
    branch: &str,
    reviewed: Reviewed<'_>,
    head: &str,
) -> Result<review_cover::Coverage, LandingError> {
    let oid = head
        .parse::<provenant_core::GitOid>()
        .map_err(|error| LandingError::Failed(error.to_string()))?;
    let base_branch = crate::pr::pull_for_branch(host_state, project, branch, oid)
        .map_err(|error| LandingError::Failed(error.to_string()))?
        .base_branch;
    let mut repo = super::adapters::GitRepo::new(project, host_state)?;
    let target = repo.fetch_target_untracked(&base_branch)?;
    review_cover::coverage(&mut RepoGit(&mut repo), reviewed, head, &target)
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "custom worktree harness includes tests without the test runner"
)]
mod tests {
    use super::*;
    use crate::pr::landing::fakes::{FakeForge, FakeRepo, FakeReviews};

    #[test]
    fn configured_source_order_selects_the_approving_evidence() -> Result<(), LandingError> {
        use crate::pr::landing::ports::{Approval, ReviewRecord};
        use provenant_records::workflow::ReviewVerdict;
        let mut forge = FakeForge::default();
        forge.approvals_at_head.insert(
            (1, "head".into()),
            vec![Approval {
                reference: "forge-review".into(),
                reviewer: "reviewer".into(),
            }],
        );
        let reviews = FakeReviews::of(BTreeMap::from([(
            1,
            vec![ReviewRecord {
                reference: "review-attempt".into(),
                reviewer: "review-execution".into(),
                head: "head".into(),
                verdict: ReviewVerdict::Approve,
                independent: true,
            }],
        )]));
        let mut repo = FakeRepo::default();
        for sources in [
            vec![ApprovalSource::ProvenantReview, ApprovalSource::Forge],
            vec![ApprovalSource::Forge, ApprovalSource::ProvenantReview],
            vec![ApprovalSource::Forge],
        ] {
            let Decision::Approved(approved) = Approvals::new(&sources, &reviews)
                .at(&mut repo, "base")
                .decide(&mut forge, 1, "head", "writer")?
            else {
                return Err(LandingError::Failed(
                    "configured source did not approve".into(),
                ));
            };
            assert!(
                sources
                    .iter()
                    .all(|source| approved.evidence.iter().any(|e| e.source == *source))
            );
        }
        let disabled = Approvals::new(&[], &reviews)
            .at(&mut repo, "base")
            .decide(&mut forge, 1, "head", "writer")?;
        assert!(matches!(disabled, Decision::Refused(_)));
        Ok(())
    }

    #[test]
    fn carry_receipts_preserve_approval_only_for_reproduced_clean_trees() -> Result<(), LandingError>
    {
        use crate::pr::landing::ports::Approval;
        use tempfile::tempdir;
        let sources = [ApprovalSource::Forge];
        let reviews = FakeReviews::default();
        for (
            kind,
            current_tree,
            expected,
            expected_path,
            receipt_exists,
            merge_base,
            diffs_match,
        ) in [
            (
                "clean_merge",
                "tree-approved-base",
                true,
                "",
                true,
                "base",
                true,
            ),
            (
                "clean_merge",
                "tree-approved-base",
                true,
                "",
                false,
                "base",
                true,
            ),
            (
                "clean_merge",
                "resolution-changed",
                false,
                "src/logic.rs",
                true,
                "base",
                true,
            ),
            (
                "format",
                "logic-changed",
                false,
                "src/logic.rs",
                true,
                "base",
                true,
            ),
            (
                "clean_merge",
                "tree-approved-unrelated",
                false,
                "src/logic.rs",
                true,
                "unrelated",
                true,
            ),
            (
                "clean_merge",
                "tree-approved-base",
                false,
                "src/logic.rs",
                true,
                "base",
                false,
            ),
        ] {
            let state = tempdir().map_err(|error| LandingError::Failed(error.to_string()))?;
            let current = format!("current-{kind}-{current_tree}");
            let receipt = super::super::receipt::ApprovalCarryReceipt {
                repository: "fake-repository".into(),
                number: 12,
                approved_head: "approved".into(),
                current_head: current.clone(),
                base_head: merge_base.into(),
                kind: kind.into(),
                extra: Default::default(),
            };
            if receipt_exists {
                super::super::receipt::write_approval_carry(state.path(), &receipt)?;
            }
            let mut forge = FakeForge::default();
            forge.approvals_at_head.insert(
                (12, "approved".into()),
                vec![Approval {
                    reference: "review-7".into(),
                    reviewer: "reviewer".into(),
                }],
            );
            let mut repo = FakeRepo::default();
            repo.parents
                .insert("approved".into(), vec!["fork-base".into()]);
            repo.parents.insert("base".into(), vec!["fork-base".into()]);
            repo.parents
                .insert(current.clone(), vec!["approved".into(), merge_base.into()]);
            repo.diffs.insert(
                ("fork-base".into(), "approved".into()),
                "approved patch".into(),
            );
            repo.diffs.insert(
                (merge_base.into(), current.clone()),
                if diffs_match {
                    "approved patch".into()
                } else {
                    "changed patch".into()
                },
            );
            repo.trees.insert(current.clone(), current_tree.into());
            repo.paths.insert(
                ("approved".into(), current.clone()),
                vec!["src/logic.rs".into()],
            );
            let warnings = std::cell::RefCell::new(Vec::new());
            let decision = Approvals::new(&sources, &reviews)
                .with_warnings(&warnings)
                .with_carry(
                    state.path(),
                    &[
                        provenant_cli::domains::ApprovalCarry::CleanMerge,
                        provenant_cli::domains::ApprovalCarry::Format,
                    ],
                )
                .at(&mut repo, "base")
                .decide(&mut forge, 12, &current, "writer")?;
            if expected {
                assert!(matches!(decision, Decision::Approved(_)));
                assert!(
                    super::super::receipt::read_approval_carry(
                        state.path(),
                        "fake-repository",
                        12,
                        &current,
                    )?
                    .is_some()
                );
                assert!(
                    warnings
                        .borrow()
                        .iter()
                        .any(|warning| warning.contains("carried independent approval"))
                );
            } else {
                let Decision::Refused(reason) = decision else {
                    return Err(LandingError::Failed(
                        "changed tree retained approval".into(),
                    ));
                };
                assert!(
                    reason.contains("approved head approved")
                        && reason.contains(&current)
                        && reason.contains(expected_path)
                );
                if kind == "format" {
                    assert!(
                        warnings
                            .borrow()
                            .iter()
                            .any(|warning| warning.contains("formatter"))
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn configured_formatter_carries_only_formatting_changes() -> Result<(), LandingError> {
        let root = tempfile::tempdir().map_err(|error| LandingError::Failed(error.to_string()))?;
        let project = root.path().join("project");
        let state = root.path().join("state");
        std::fs::create_dir_all(&project)
            .and_then(|()| std::fs::create_dir_all(&state))
            .map_err(|error| LandingError::Failed(error.to_string()))?;
        let run_git = |args: &[&str]| -> Result<String, LandingError> {
            let git = provenant_os::git::git_executable()
                .and_then(|path| path.to_str().map(str::to_owned))
                .ok_or_else(|| LandingError::Failed("Git executable is unavailable".into()))?;
            let argv = std::iter::once(git.as_str())
                .chain(args.iter().copied())
                .collect::<Vec<_>>();
            let output =
                crate::pr::execute_for(&project, &argv, std::time::Duration::from_secs(60), false)
                    .map_err(|error| LandingError::Failed(error.to_string()))?;
            if output.termination != provenant_os::exec::Termination::Exit(0) {
                return Err(LandingError::Failed(
                    String::from_utf8_lossy(&output.stderr.bytes).into_owned(),
                ));
            }
            Ok(String::from_utf8_lossy(&output.stdout.bytes)
                .trim()
                .to_owned())
        };
        run_git(&["init", "-q", "--initial-branch=main"])?;
        run_git(&["config", "user.name", "Fixture"])?;
        run_git(&["config", "user.email", "fixture@example.invalid"])?;
        std::fs::write(
            project.join("Cargo.toml"),
            "[package]\nname='format-fixture'\nversion='0.1.0'\nedition='2021'\n",
        )
        .map_err(|error| LandingError::Failed(error.to_string()))?;
        std::fs::create_dir_all(project.join("src"))
            .map_err(|error| LandingError::Failed(error.to_string()))?;
        std::fs::write(project.join("src/lib.rs"), "pub fn value( ){let n=1;n}\n")
            .map_err(|error| LandingError::Failed(error.to_string()))?;
        let mut ports = provenant_config::PortTable::builtin();
        ports.verify.workspace = std::path::PathBuf::from(".");
        ports
            .verify
            .tiers
            .get_mut("0")
            .ok_or_else(|| LandingError::Failed("built-in verify tier is missing".into()))?
            .commands = vec!["cargo fmt --all".into()];
        let ports_dir = project.join(".provenant");
        std::fs::create_dir_all(&ports_dir)
            .and_then(|()| {
                std::fs::write(
                    ports_dir.join("ports.json"),
                    serde_json::to_vec(&ports).map_err(std::io::Error::other)?,
                )
            })
            .map_err(|error| LandingError::Failed(error.to_string()))?;
        run_git(&["add", "Cargo.toml", "src/lib.rs", ".provenant/ports.json"])?;
        run_git(&["commit", "-qm", "approved"])?;
        let approved = run_git(&["rev-parse", "HEAD"])?;
        let cargo = std::env::var_os("CARGO")
            .and_then(|path| path.into_string().ok())
            .unwrap_or_else(|| "cargo".into());
        let formatted = crate::pr::execute_for(
            &project,
            &[&cargo, "fmt", "--all"],
            std::time::Duration::from_secs(300),
            false,
        )
        .map_err(|error| LandingError::Failed(error.to_string()))?;
        if formatted.termination != provenant_os::exec::Termination::Exit(0) {
            return Err(LandingError::Failed("fixture cargo fmt failed".into()));
        }
        run_git(&["add", "src/lib.rs"])?;
        run_git(&["commit", "-qm", "format"])?;
        let current = run_git(&["rev-parse", "HEAD"])?;
        let sources = [ApprovalSource::Forge];
        let reviews = FakeReviews::default();
        let mut forge = FakeForge::default();
        forge.approvals_at_head.insert(
            (77, approved.clone()),
            vec![crate::pr::landing::ports::Approval {
                reference: "format-review".into(),
                reviewer: "reviewer".into(),
            }],
        );
        let mut repo = FakeRepo {
            project_path: Some(project.clone()),
            ..Default::default()
        };
        repo.parents.insert(current.clone(), vec![approved.clone()]);
        let warnings = std::cell::RefCell::new(Vec::new());
        let decision = Approvals::new(&sources, &reviews)
            .with_warnings(&warnings)
            .with_carry(
                state.as_path(),
                &[provenant_cli::domains::ApprovalCarry::Format],
            )
            .at(&mut repo, "base")
            .decide(&mut forge, 77, &current, "author")?;
        assert!(matches!(decision, Decision::Approved(_)));
        assert!(
            warnings
                .borrow()
                .iter()
                .any(|warning| warning.contains("carried independent approval across format"))
        );
        assert!(
            super::super::receipt::read_approval_carry(
                state.as_path(),
                "fake-repository",
                77,
                &current,
            )?
            .is_some()
        );

        std::fs::write(
            project.join("src/lib.rs"),
            "pub fn value() { let n = 1; n }\npub fn logic() -> bool { true }\n",
        )
        .map_err(|error| LandingError::Failed(error.to_string()))?;
        run_git(&["add", "src/lib.rs"])?;
        run_git(&["commit", "-qm", "logic"])?;
        let logic = run_git(&["rev-parse", "HEAD"])?;
        repo.parents.insert(logic.clone(), vec![current.clone()]);
        repo.paths.insert(
            (approved.clone(), logic.clone()),
            vec!["src/logic.rs".into()],
        );
        let warnings = std::cell::RefCell::new(Vec::new());
        let decision = Approvals::new(&sources, &reviews)
            .with_warnings(&warnings)
            .with_carry(
                state.as_path(),
                &[provenant_cli::domains::ApprovalCarry::Format],
            )
            .at(&mut repo, "base")
            .decide(&mut forge, 77, &logic, "author")?;
        let Decision::Refused(reason) = decision else {
            return Err(LandingError::Failed(
                "formatter carry retained a logic change".into(),
            ));
        };
        assert!(reason.contains(&approved) && reason.contains(&logic));
        assert!(
            warnings
                .borrow()
                .iter()
                .any(|warning| warning.contains("format approval carry unavailable"))
        );
        std::fs::remove_file(ports_dir.join("ports.json"))
            .map_err(|error| LandingError::Failed(error.to_string()))?;
        let no_formatter_state =
            tempfile::tempdir().map_err(|error| LandingError::Failed(error.to_string()))?;
        let mut no_formatter_repo = FakeRepo {
            project_path: Some(project.clone()),
            ..Default::default()
        };
        no_formatter_repo
            .parents
            .insert(current.clone(), vec![approved.clone()]);
        let warnings = std::cell::RefCell::new(Vec::new());
        let decision = Approvals::new(&sources, &reviews)
            .with_warnings(&warnings)
            .with_carry(
                no_formatter_state.path(),
                &[provenant_cli::domains::ApprovalCarry::Format],
            )
            .at(&mut no_formatter_repo, "base")
            .decide(&mut forge, 77, &current, "author")?;
        assert!(matches!(decision, Decision::Refused(_)));
        assert!(
            warnings
                .borrow()
                .iter()
                .any(|warning| warning.contains("configured formatter is missing"))
        );
        Ok(())
    }

    #[test]
    fn content_delta_refusal_names_approved_head_current_head_and_path() -> Result<(), LandingError>
    {
        use crate::pr::landing::ports::Approval;
        let sources = [ApprovalSource::Forge];
        let reviews = FakeReviews::default();
        let mut forge = FakeForge::default();
        forge.approvals_at_head.insert(
            (4, "approved-head".into()),
            vec![Approval {
                reference: "review-4".into(),
                reviewer: "independent".into(),
            }],
        );
        let mut repo = FakeRepo::default();
        repo.parents
            .insert("current-head".into(), vec!["approved-head".into()]);
        repo.paths.insert(
            ("approved-head".into(), "current-head".into()),
            vec!["src/logic.rs".into()],
        );
        let decision = Approvals::new(&sources, &reviews)
            .at(&mut repo, "base")
            .decide(&mut forge, 4, "current-head", "writer")?;
        let Decision::Refused(message) = decision else {
            return Err(LandingError::Failed(
                "content delta carried approval".into(),
            ));
        };
        assert!(
            message.contains("approved-head")
                && message.contains("current-head")
                && message.contains("src/logic.rs")
        );
        Ok(())
    }

    // Spec: docs/specs/cli.md#landing-adds-no-merge-authority
    #[test]
    fn unusable_graph_diagnostic_keeps_decision_correct() -> Result<(), LandingError> {
        use crate::pr::landing::ports::ReviewRecord;
        use provenant_records::workflow::ReviewVerdict;

        let graph = "graph-broken-review";
        let sources = [ApprovalSource::ProvenantReview];
        let mut forge = FakeForge::default();
        let mut repo = FakeRepo::default();
        let reviews = FakeReviews {
            records: BTreeMap::from([(
                1,
                vec![ReviewRecord {
                    reference: "review-attempt".into(),
                    reviewer: "independent-reviewer".into(),
                    head: "head".into(),
                    verdict: ReviewVerdict::Approve,
                    independent: true,
                }],
            )]),
            unusable: BTreeMap::from([(1, vec![format!("graph {graph}: bad checkpoint digest")])]),
            ..FakeReviews::default()
        };

        let warnings = std::cell::RefCell::new(Vec::new());
        let decision = Approvals::new(&sources, &reviews)
            .with_warnings(&warnings)
            .at(&mut repo, "base")
            .decide(&mut forge, 1, "head", "writer")?;
        assert!(
            warnings
                .borrow()
                .iter()
                .any(|warning| warning.contains(graph))
        );
        let Decision::Approved(approved) = decision else {
            return Err(LandingError::Failed(
                "valid approval was hidden by an unusable graph".into(),
            ));
        };
        assert_eq!(approved.reference, "review-attempt");
        assert_eq!(approved.evidence.len(), 1);

        let no_approval = FakeReviews {
            unusable: BTreeMap::from([(1, vec![format!("graph {graph}: bad checkpoint digest")])]),
            ..FakeReviews::default()
        };
        let decision = Approvals::new(&sources, &no_approval)
            .at(&mut repo, "base")
            .decide(&mut forge, 1, "head", "writer")?;
        let Decision::Refused(reason) = decision else {
            return Err(LandingError::Failed(
                "unusable graph was treated as an approval".into(),
            ));
        };
        assert!(reason.contains(graph), "{reason}");

        let unreadable = FakeReviews {
            failure: Some("graph enumeration denied".into()),
            ..FakeReviews::default()
        };
        let sources = [ApprovalSource::ProvenantReview, ApprovalSource::Forge];
        for has_approval in [true, false] {
            let mut forge = FakeForge::default();
            if has_approval {
                forge.approvals_at_head.insert(
                    (1, "head".into()),
                    vec![super::super::ports::Approval {
                        reference: "forge-review".into(),
                        reviewer: "independent-reviewer".into(),
                    }],
                );
            }
            let warnings = std::cell::RefCell::new(Vec::new());
            let decision = Approvals::new(&sources, &unreadable)
                .with_warnings(&warnings)
                .at(&mut repo, "base")
                .decide(&mut forge, 1, "head", "writer")?;
            assert_eq!(matches!(decision, Decision::Approved(_)), has_approval);
            assert!(
                warnings
                    .borrow()
                    .iter()
                    .any(|warning| warning.contains("graph enumeration denied"))
            );
            if let Decision::Refused(reason) = decision {
                assert!(reason.contains("graph enumeration denied"));
            }
        }
        Ok(())
    }

    #[test]
    fn owner_override_is_explicit_and_records_actor_and_reason() -> Result<(), LandingError> {
        let mut forge = FakeForge::default();
        let mut repo = FakeRepo::default();
        let reviews = FakeReviews::default();
        let exempt = ReviewExempt::none();
        let sources = [ApprovalSource::ProvenantReview, ApprovalSource::Forge];
        let approvals = Approvals::new(&sources, &reviews);
        assert!(matches!(
            approvals
                .at(&mut repo, "base")
                .decide(&mut forge, 1, "head", "writer")?,
            Decision::Refused(_)
        ));
        let decision = approvals
            .governed(ApprovalTier::Carry, &exempt, Some("owner accepts delta"))
            .at(&mut repo, "base")
            .decide(&mut forge, 1, "head", "writer")?;
        let Decision::Approved(approved) = decision else {
            return Err(LandingError::Failed(
                "explicit override did not approve".into(),
            ));
        };
        let accepted = approved
            .accepted
            .ok_or_else(|| LandingError::Failed("missing owner evidence".into()))?;
        assert_eq!(accepted.actor, "chair");
        assert_eq!(accepted.reason, "owner accepts delta");
        assert!(approved.evidence.is_empty());
        Ok(())
    }
}
