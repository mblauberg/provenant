//! The format-2 custody contract. Advisory ownership is never a writer lease.
use crate::ExtraFields;
use crate::command_result::{CommandError, ErrorClass, SettingRef};
use crate::policy::{LEASE_HOLDER_UNREACHABLE, WORKTREE_SHARED, WORKTREE_SHARING_REFUSED};
use crate::provenance::{Layer, Source, Sourced};
use crate::warning::{Warning, WarningCode, detected_source};
use provenant_core::{ActorId, AttemptId, GitOid, HostId, ProjectId, WallTime};
use serde::{Deserialize, Serialize};

/// The durable attempt generation bound to supervisor custody and diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub attempt: AttemptId,
    pub generation: u64,
}

/// Identity of the submitting chair, shared by dispatch and custody.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chair {
    #[serde(flatten)]
    pub extra: ExtraFields,
    pub host: HostId,
    pub actor: ActorId,
    pub session: Option<String>,
    pub process: Option<HolderProcess>,
}

impl PartialEq for Chair {
    fn eq(&self, other: &Self) -> bool {
        (self.host, self.actor, &self.session) == (other.host, other.actor, &other.session)
    }
}
impl Eq for Chair {}
/// Native process identity used only for advisory presence observations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolderProcess {
    pub boot: String,
    pub pid: i64,
    pub native_start: u64,
}

macro_rules! subject_kinds {
    ($($variant:ident => ($name:literal, $path:literal)),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub enum SubjectKind { $(#[serde(rename = $name)] $variant),+ }
        impl SubjectKind {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub const fn is_path(self) -> bool { match self { $(Self::$variant => $path),+ } }
            pub const fn as_str(self) -> &'static str { match self { $(Self::$variant => $name),+ } }
        }
        impl std::str::FromStr for SubjectKind {
            type Err = String;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value { $($name => Ok(Self::$variant),)+ _ => Err(format!("unknown custody subject kind: {value}")) }
            }
        }
    }
}
subject_kinds! {
    Graph => ("graph", false), Attempt => ("attempt", false), Branch => ("branch", false), Ref => ("ref", false), Worktree => ("worktree", false),
    Pr => ("pr", false), Issue => ("issue", false), Project => ("project", false), Path => ("path", true), CloudKey => ("cloud_key", false),
}
impl std::fmt::Display for SubjectKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subject {
    pub kind: SubjectKind,
    pub id: String,
}
/// One checkout incarnation. The canonical path is an association, never its
/// identity by itself; Git's private directory owns the immutable generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeIdentity {
    pub path: String,
    pub generation: String,
}

impl Subject {
    pub fn worktree(path: impl Into<String>, generation: impl Into<String>) -> Self {
        let identity = WorktreeIdentity {
            path: path.into(),
            generation: generation.into(),
        };
        Self::new(
            SubjectKind::Worktree,
            serde_json::json!(identity).to_string(),
        )
    }

    pub fn worktree_identity(&self) -> Option<WorktreeIdentity> {
        if self.kind != SubjectKind::Worktree {
            return None;
        }
        let identity: WorktreeIdentity = serde_json::from_str(&self.id).ok()?;
        if !valid_worktree_path(&identity.path)
            || identity.generation.is_empty()
            || identity.generation.len() > 256
            || !identity
                .generation
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
            || Self::worktree(&identity.path, &identity.generation) != *self
        {
            return None;
        }
        Some(identity)
    }

    pub fn worktree_path(&self) -> Option<std::path::PathBuf> {
        Some(self.worktree_identity()?.path.into())
    }

    pub fn new(kind: SubjectKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }
}
impl std::fmt::Display for Subject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.kind, self.id)
    }
}
impl std::str::FromStr for Subject {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (kind, id) = value.split_once(':').ok_or("expected KIND:ID")?;
        let subject = Self::new(kind.parse()?, id);
        let valid = if subject.kind == SubjectKind::Worktree {
            subject.worktree_identity().is_some()
        } else {
            !id.is_empty() && id.len() <= 4096 && !id.chars().any(char::is_control)
        };
        if !valid {
            return Err("invalid custody subject id".into());
        }
        Ok(subject)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Holder {
    Chair {
        chair: Chair,
    },
    Attempt {
        attempt: AttemptId,
        chair: Option<Chair>,
    },
}
impl Holder {
    pub fn chair(&self) -> Option<&Chair> {
        match self {
            Self::Chair { chair } => Some(chair),
            Self::Attempt { chair, .. } => chair.as_ref(),
        }
    }
    pub fn actor(&self) -> Option<ActorId> {
        self.chair().map(|chair| chair.actor)
    }
    pub fn belongs_to(&self, chair: &Chair) -> bool {
        self.chair().is_some_and(|owner| {
            owner.host == chair.host && owner.actor == chair.actor && owner.session == chair.session
        })
    }
}

/// How a second writer is admitted when a worktree lease is live.
/// `Inherit` means resolve the ordinary layered setting for a new lease.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sharing {
    #[default]
    Inherit,
    Allow,
    Warn,
    Refuse,
}

impl Sharing {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Allow => "allow",
            Self::Warn => "warn",
            Self::Refuse => "refuse",
        }
    }
}

impl std::str::FromStr for Sharing {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "inherit" => Ok(Self::Inherit),
            "allow" => Ok(Self::Allow),
            "warn" => Ok(Self::Warn),
            "refuse" => Ok(Self::Refuse),
            _ => Err(format!("unknown worktree sharing mode: {value}")),
        }
    }
}

pub const WORKTREE_SHARING_KEY: &str = "lanes.worktree.sharing";

/// The compiled sharing default, with provenance for typed admission output.
pub fn default_sharing() -> Sourced<Sharing> {
    Sourced {
        value: Sharing::Warn,
        source: Source::default(),
    }
}

/// Preserve the resolved sharing source in a command or warning record.
pub fn sharing_setting_ref(sharing: &Sourced<Sharing>) -> SettingRef {
    SettingRef {
        key: WORKTREE_SHARING_KEY.to_owned(),
        value: sharing.value.as_str().to_owned(),
        layer: sharing.source.layer,
        origin: sharing.source.origin.clone().unwrap_or_default(),
    }
}

/// Durable worktree or branch custody. Whether this lease is live is derived
/// from the holder's attempt state at read time and is never stored here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeLease {
    pub holder: AttemptId,
    pub since: WallTime,
    #[serde(default)]
    pub sharing: Sharing,
}

/// Current derived state of a lease. An unreachable holder remains live until
/// the configured grace period expires; callers then classify it as lapsed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LeaseState {
    Live {
        attempt_state: String,
    },
    Lapsed {
        attempt_state: String,
    },
    Unreachable {
        retry_at: WallTime,
        setting: SettingRef,
    },
    /// The holder's host stayed unobservable through the configured grace.
    /// The lease may be adopted, but callers must report the lapse.
    LapsedUnreachable {
        retry_at: WallTime,
        setting: SettingRef,
    },
}

/// Durable fact that a new attempt continued a lapsed lease.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handover {
    pub from: AttemptId,
    pub to: AttemptId,
    pub at: WallTime,
    pub head: Option<GitOid>,
    /// Paths left uncommitted by the previous holder; they must be preserved.
    pub uncommitted: Vec<String>,
}

/// A lease record paired with its current holder classification. Confinement
/// derives presence at read time and persists only the first-unreachable time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaseObservation {
    pub lease: WorktreeLease,
    pub state: LeaseState,
}

/// Outcome of the shared writer-admission policy.
#[derive(Clone, Debug)]
pub enum Admit {
    Proceed { warning: Option<Box<Warning>> },
    Refuse { error: CommandError },
}

/// Resolve the effect of an already-classified lease and sharing setting.
/// Callers pass `Lapsed` for a terminal holder and `Live` while its attempt is
/// nonterminal. An unreachable holder that outlasted its grace proceeds with a
/// lapse warning. This function creates the typed warning or refusal once.
pub fn admit_writer(
    tree: &Subject,
    lease: Option<&LeaseObservation>,
    sharing: &Sourced<Sharing>,
) -> Admit {
    let effective = lease
        .map(|observation| observation.lease.sharing)
        .filter(|owner| *owner != Sharing::Inherit)
        .map(|owner| Sourced {
            value: owner,
            source: Source::at(Layer::Parent, "lease.json#sharing"),
        })
        .unwrap_or_else(|| sharing.clone());
    let effective = if effective.value == Sharing::Inherit {
        Sourced {
            value: Sharing::Warn,
            ..effective
        }
    } else {
        effective
    };
    let setting = sharing_setting_ref(&effective);
    let Some(observation) = lease else {
        return Admit::Proceed { warning: None };
    };
    let warning = |code: WarningCode, message: String| {
        Box::new(
            Warning::new(code, message, detected_source(code))
                .subject(tree.to_string())
                .setting(setting.clone()),
        )
    };
    let warning_with_setting = |code: WarningCode, message: String, setting: SettingRef| {
        Box::new(
            Warning::new(code, message, detected_source(code))
                .subject(tree.to_string())
                .setting(setting),
        )
    };
    let refusal = || {
        let holder = observation.lease.holder;
        let holder_text = holder.to_string();
        CommandError::new(
            WORKTREE_SHARING_REFUSED,
            format!(
                "{WORKTREE_SHARING_REFUSED}: {tree} is held by live attempt {holder}; wait for the holder or launch with --destination new"
            ),
            Some(&holder_text),
            format!("provenant wait {holder}"),
        )
        .with_class(ErrorClass::Configured)
        .with_setting(setting.clone())
    };
    match &observation.state {
        LeaseState::Lapsed { .. } => Admit::Proceed { warning: None },
        LeaseState::Live { attempt_state } => match effective.value {
            Sharing::Inherit | Sharing::Warn => Admit::Proceed {
                warning: Some(warning(
                    WarningCode(WORKTREE_SHARED),
                    format!(
                        "WORKTREE_SHARED: {tree} is held by attempt {} ({attempt_state})",
                        observation.lease.holder
                    ),
                )),
            },
            Sharing::Allow => Admit::Proceed { warning: None },
            Sharing::Refuse => Admit::Refuse { error: refusal() },
        },
        LeaseState::Unreachable { retry_at, setting } => match effective.value {
            Sharing::Inherit | Sharing::Warn => Admit::Proceed {
                warning: Some(warning_with_setting(
                    WarningCode(LEASE_HOLDER_UNREACHABLE),
                    format!(
                        "LEASE_HOLDER_UNREACHABLE: {tree} holder {} is unreachable until {retry_at}",
                        observation.lease.holder
                    ),
                    setting.clone(),
                )),
            },
            Sharing::Allow => Admit::Proceed { warning: None },
            Sharing::Refuse => Admit::Refuse { error: refusal() },
        },
        LeaseState::LapsedUnreachable { retry_at, setting } => Admit::Proceed {
            warning: Some(warning_with_setting(
                WarningCode(LEASE_HOLDER_UNREACHABLE),
                format!(
                    "LEASE_HOLDER_UNREACHABLE: {tree} holder {} stayed unreachable through the grace ending at {retry_at}; its lease has lapsed",
                    observation.lease.holder
                ),
                setting.clone(),
            )),
        },
    }
}

/// Boundary guard signature for operations that could destroy another live
/// lane's work. The caller supplies the already-resolved fact; no filesystem
/// or Git mutation belongs in this records crate.
pub fn guard_destructive(
    op: &str,
    subject: &Subject,
    destroys_live_work: bool,
    holder: Option<AttemptId>,
) -> Result<(), crate::command_result::CommandError> {
    if destroys_live_work {
        let holder = holder
            .map(|attempt| attempt.to_string())
            .unwrap_or_default();
        Err(crate::command_result::CommandError::new(
            "DESTROYS_LIVE_WORK",
            format!("{op} on {subject} would destroy work held by a live lane"),
            Some(&holder),
            if holder.is_empty() {
                "identify and wait for the holder".to_owned()
            } else {
                format!("provenant wait {holder}")
            },
        )
        .with_class(crate::command_result::ErrorClass::Boundary(
            crate::policy::Boundary::OtherWork,
        )))
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Provenant,
    Adopted,
    External,
}

/// What was proved, independently of how it was proved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingPredicate {
    Subsumed,
    Landed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LandingSource {
    RetainedRef {
        reference: String,
        commit: provenant_core::GitOid,
    },
    TreeEquivalent {
        reference: String,
        head: provenant_core::GitOid,
        target: provenant_core::GitOid,
        tree: provenant_core::GitOid,
    },
    IntegrationTarget {
        reference: String,
        commit: provenant_core::GitOid,
    },
    ForgeMerged {
        pr: String,
        head: provenant_core::GitOid,
        merge_commit: Option<provenant_core::GitOid>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandedEvidence {
    pub predicate: LandingPredicate,
    pub source: LandingSource,
}

/// A durable pointer to the archive captured before an explicit discard.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveProof {
    /// Archive directory id under the project's declared discard archive root.
    pub id: String,
    pub tip: provenant_core::GitOid,
}
impl ArchiveProof {
    pub fn valid_for(&self, expected_tip: provenant_core::GitOid) -> bool {
        !self.id.is_empty()
            && self.id.len() <= 255
            && self.id != "."
            && self.id != ".."
            && !self.id.chars().any(|ch| ch == '/' || ch == '\\')
            && !self.id.chars().any(char::is_control)
            && self.tip == expected_tip
    }
}

/// Evidence accepted for a destructive removal. Explicit discard is an
/// authority choice, not a claim that the subject landed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemovalEvidence {
    Landing {
        evidence: LandedEvidence,
    },
    /// The reference generation is discarded by authority. Any associated
    /// worktree must have an archive proof before removal begins.
    ExplicitDiscard {
        archive: Option<ArchiveProof>,
    },
    /// A clean idle worktree is removed while its reference remains available.
    IdleRetirement {
        tip: provenant_core::GitOid,
    },
}

/// Credential-free identity for a URL endpoint. Transport keeps the original URL private.
///
/// The SSH username is a destination principal: different users at the same
/// host and path can reach different repositories, so it must survive the
/// credential-free projection to keep delivery and cleanup bound to the
/// destination that was actually selected. Any password or token after the
/// username is a secret and is dropped. For other transports (http(s), file,
/// ...) a username can itself be a token, so all userinfo is removed.
///
/// scp-like `user@host:path` URLs carry no `://`, have no password form, and
/// retain their username as written.
pub fn endpoint_identity(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let (principal, host) = match authority.rsplit_once('@') {
        Some((userinfo, host)) if scheme == "ssh" || scheme == "git+ssh" => {
            let user = userinfo.split_once(':').map_or(userinfo, |(user, _)| user);
            (format!("{user}@"), host)
        }
        Some((_, host)) => (String::new(), host),
        None => (String::new(), authority),
    };
    format!("{scheme}://{principal}{host}{}", &rest[end..])
}

/// Where a reference lives. Remote delivery is pinned to the push URL bound by
/// the host; callers never infer it from the remote name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RefEndpoint {
    Local,
    Remote {
        remote: String,
        push_url: String,
    },
    /// A detached private checkout's actual HEAD, distinguished by its
    /// canonical worktree path and its RefLedger generation.
    Worktree {
        path: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefOrigin {
    Created,
    Adopted,
}

/// Endpoint-qualified identity for one logical reference in a project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RefEndpointIdentity {
    Local,
    Remote {
        remote: String,
        push_url_digest: provenant_core::Digest,
    },
    Worktree {
        path_digest: provenant_core::Digest,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefIdentity {
    pub repository: ProjectId,
    pub endpoint: RefEndpointIdentity,
    pub reference: String,
}

impl RefIdentity {
    fn custody_subject(&self, generation: u64) -> Result<Subject, crate::CanonicalError> {
        let id =
            crate::CanonicalV1::record_digest("custody-ref-generation", 2, &(self, generation))?
                .to_string();
        Ok(Subject::new(SubjectKind::Ref, id))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefOwnership {
    pub repository: ProjectId,
    pub endpoint: RefEndpoint,
    pub reference: String,
    pub generation: u64,
    pub origin: RefOrigin,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefGeneration {
    pub identity: RefIdentity,
    pub generation: u64,
    pub origin: RefOrigin,
}

impl RefGeneration {
    pub fn custody_subject(&self) -> Result<Subject, crate::CanonicalError> {
        self.identity.custody_subject(self.generation)
    }

    pub fn matches_ownership(
        &self,
        ownership: &RefOwnership,
    ) -> Result<bool, crate::CanonicalError> {
        let observed = ownership.generation_identity()?;
        Ok(self == &observed)
    }
}

impl RefOwnership {
    /// Logical branch custody is an advisory owner source only; endpoint and
    /// generation identity for retirement comes from `custody_subject`.
    pub fn logical_branch_subject(&self) -> Option<Subject> {
        if matches!(self.endpoint, RefEndpoint::Worktree { .. }) {
            return None;
        }
        let id = self.reference.strip_prefix("refs/heads/")?;
        if id.is_empty() || id.len() > 4096 || id.chars().any(char::is_control) {
            return None;
        }
        Some(Subject::new(SubjectKind::Branch, id))
    }

    pub fn identity(&self) -> Result<RefIdentity, crate::CanonicalError> {
        let endpoint = match &self.endpoint {
            RefEndpoint::Local => RefEndpointIdentity::Local,
            RefEndpoint::Remote { remote, push_url } => RefEndpointIdentity::Remote {
                remote: remote.clone(),
                push_url_digest: crate::CanonicalV1::record_digest(
                    "custody-ref-push-url",
                    2,
                    &(self.repository, remote, push_url),
                )?,
            },
            RefEndpoint::Worktree { path } => {
                if self.reference != "HEAD" || !valid_worktree_path(path) {
                    return Err(crate::CanonicalError::InvalidKind);
                }
                RefEndpointIdentity::Worktree {
                    path_digest: crate::CanonicalV1::record_digest(
                        "custody-worktree-head",
                        2,
                        &(self.repository, path),
                    )?,
                }
            }
        };
        Ok(RefIdentity {
            repository: self.repository,
            endpoint,
            reference: self.reference.clone(),
        })
    }

    pub fn generation_identity(&self) -> Result<RefGeneration, crate::CanonicalError> {
        Ok(RefGeneration {
            identity: self.identity()?,
            generation: self.generation,
            origin: self.origin,
        })
    }

    /// Stable custody identity for one observed generation. Remote URL text
    /// stays in the live ref ledger and never enters the custody record or key.
    pub fn custody_subject(&self) -> Result<Subject, crate::CanonicalError> {
        self.generation_identity()?.custody_subject()
    }
}

fn valid_worktree_path(path: &str) -> bool {
    use std::path::Component;
    path.len() <= 4096
        && !path.is_empty()
        && !path.chars().any(char::is_control)
        && !path.contains("//")
        && path
            .split('/')
            .skip(1)
            .all(|component| !component.is_empty() && component != "." && component != "..")
        && std::path::Path::new(path).is_absolute()
        && std::path::Path::new(path)
            .components()
            .any(|component| matches!(component, Component::Normal(_)))
        && std::path::Path::new(path)
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

/// The actor's durable decision. Explicit discard is distinct from a landing
/// claim and remains fenced by the pinned reference generation and tip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RetirementDecision {
    /// Remove the reference and its associated worktrees after current landing
    /// evidence or a previously captured explicit-discard archive.
    Discard {
        authority: ActorId,
        evidence: RemovalEvidence,
    },
    /// Remove only clean, idle worktrees while preserving the local reference.
    RetireWorktree {
        authority: ActorId,
        evidence: RemovalEvidence,
    },
}
impl RetirementDecision {
    pub fn authority(&self) -> ActorId {
        match self {
            Self::Discard { authority, .. } | Self::RetireWorktree { authority, .. } => *authority,
        }
    }

    pub fn evidence(&self) -> &RemovalEvidence {
        match self {
            Self::Discard { evidence, .. } | Self::RetireWorktree { evidence, .. } => evidence,
        }
    }

    pub fn removes_reference(&self) -> bool {
        matches!(self, Self::Discard { .. })
    }

    /// Whether a request can resume this recorded action for the same pinned
    /// generation. Callers must validate both decisions against the record's
    /// related subjects and expected tip; this comparison deliberately ignores
    /// a refreshed authority and refreshed Landing evidence, but keeps the
    /// action and evidence family fixed.
    pub fn permits_replay(&self, requested: &Self) -> bool {
        match (self, requested) {
            (
                Self::Discard {
                    evidence: RemovalEvidence::Landing { .. },
                    ..
                },
                Self::Discard {
                    evidence: RemovalEvidence::Landing { .. },
                    ..
                },
            )
            | (
                Self::RetireWorktree {
                    evidence: RemovalEvidence::Landing { .. },
                    ..
                },
                Self::RetireWorktree {
                    evidence: RemovalEvidence::Landing { .. },
                    ..
                },
            ) => true,
            (
                Self::Discard {
                    evidence: RemovalEvidence::ExplicitDiscard { archive: left },
                    ..
                },
                Self::Discard {
                    evidence: RemovalEvidence::ExplicitDiscard { archive: right },
                    ..
                },
            )
            | (
                Self::RetireWorktree {
                    evidence: RemovalEvidence::ExplicitDiscard { archive: left },
                    ..
                },
                Self::RetireWorktree {
                    evidence: RemovalEvidence::ExplicitDiscard { archive: right },
                    ..
                },
            ) => matches!((left, right), (None, None) | (Some(_), Some(_))),
            (
                Self::RetireWorktree {
                    evidence: RemovalEvidence::IdleRetirement { tip: left },
                    ..
                },
                Self::RetireWorktree {
                    evidence: RemovalEvidence::IdleRetirement { tip: right },
                    ..
                },
            ) => left == right,
            _ => false,
        }
    }

    pub fn valid_for(
        &self,
        related_subjects: &[Subject],
        expected_tip: provenant_core::GitOid,
    ) -> bool {
        match self {
            Self::Discard {
                evidence: RemovalEvidence::Landing { evidence },
                ..
            } => valid_landing_evidence(evidence, expected_tip, true),
            Self::Discard {
                evidence: RemovalEvidence::ExplicitDiscard { archive },
                ..
            } => match archive {
                Some(archive) => archive.valid_for(expected_tip),
                None => related_subjects.is_empty(),
            },
            Self::Discard {
                evidence: RemovalEvidence::IdleRetirement { .. },
                ..
            } => false,
            Self::RetireWorktree {
                evidence: RemovalEvidence::Landing { evidence },
                ..
            } => {
                !related_subjects.is_empty()
                    && valid_landing_evidence(evidence, expected_tip, false)
            }
            Self::RetireWorktree {
                evidence: RemovalEvidence::IdleRetirement { tip },
                ..
            } => !related_subjects.is_empty() && *tip == expected_tip,
            Self::RetireWorktree {
                evidence:
                    RemovalEvidence::ExplicitDiscard {
                        archive: Some(archive),
                    },
                ..
            } => !related_subjects.is_empty() && archive.valid_for(expected_tip),
            Self::RetireWorktree { .. } => false,
        }
    }
}

fn valid_landing_evidence(
    evidence: &LandedEvidence,
    expected_tip: provenant_core::GitOid,
    reference_must_be_landed: bool,
) -> bool {
    match (&evidence.predicate, &evidence.source) {
        (LandingPredicate::Subsumed, LandingSource::RetainedRef { reference, .. })
            if !reference_must_be_landed =>
        {
            valid_ref_name(reference)
        }
        (
            LandingPredicate::Subsumed | LandingPredicate::Landed,
            LandingSource::TreeEquivalent {
                reference, head, ..
            },
        ) if *head == expected_tip && !reference_must_be_landed => valid_ref_name(reference),
        (LandingPredicate::Landed, LandingSource::IntegrationTarget { reference, .. }) => {
            valid_ref_name(reference)
        }
        (LandingPredicate::Landed, LandingSource::ForgeMerged { pr, head, .. }) => {
            !pr.is_empty() && *head == expected_tip
        }
        _ => false,
    }
}

fn valid_ref_name(reference: &str) -> bool {
    reference.starts_with("refs/")
        && reference.len() <= 4096
        && !reference.chars().any(char::is_control)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum RetirementStep {
    Worktree { subject: Subject },
    Reference,
}
impl RetirementStep {
    pub fn for_decision(subjects: &[Subject], decision: &RetirementDecision) -> Vec<Self> {
        let worktrees = subjects
            .iter()
            .cloned()
            .map(|subject| Self::Worktree { subject });
        if decision.removes_reference() {
            worktrees.chain(std::iter::once(Self::Reference)).collect()
        } else {
            worktrees.collect()
        }
    }

    pub fn prerequisites_complete(&self, outcomes: &[RetirementStepOutcome]) -> bool {
        match self {
            Self::Worktree { .. } => true,
            Self::Reference => outcomes
                .iter()
                .filter(|outcome| matches!(&outcome.step, Self::Worktree { .. }))
                .all(|outcome| matches!(&outcome.state, RetirementStepState::Completed { .. })),
        }
    }

    /// Worktrees can be discarded once their exact tip is subsumed; deleting
    /// the reference itself requires a current landed proof.
    fn accepts_completion_evidence(
        &self,
        intent: &RemovalEvidence,
        evidence: &RemovalEvidence,
    ) -> bool {
        match (intent, evidence, self) {
            (
                RemovalEvidence::ExplicitDiscard {
                    archive: Some(expected),
                },
                RemovalEvidence::ExplicitDiscard {
                    archive: Some(actual),
                },
                Self::Worktree { .. } | Self::Reference,
            ) => expected.valid_for(expected.tip) && actual.valid_for(expected.tip),
            (
                RemovalEvidence::ExplicitDiscard { archive: None },
                RemovalEvidence::ExplicitDiscard { archive: None },
                Self::Reference,
            ) => true,
            (
                RemovalEvidence::IdleRetirement { tip: expected },
                RemovalEvidence::IdleRetirement { tip: actual },
                Self::Worktree { .. },
            ) => expected == actual,
            (
                RemovalEvidence::Landing { .. },
                RemovalEvidence::Landing { evidence },
                Self::Worktree { .. },
            ) => matches!(
                evidence.predicate,
                LandingPredicate::Subsumed | LandingPredicate::Landed
            ),
            (
                RemovalEvidence::Landing { .. },
                RemovalEvidence::Landing { evidence },
                Self::Reference,
            ) => evidence.predicate == LandingPredicate::Landed,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RetirementStepState {
    Pending,
    Completed { evidence: RemovalEvidence },
    Refused { reason: String },
    Unknown { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetirementStepOutcome {
    pub step: RetirementStep,
    pub state: RetirementStepState,
}

/// Custody-owned intent and resumable outcomes for one reference generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetirementRecord {
    pub schema_version: u32,
    pub id: String,
    pub project: ProjectId,
    /// The endpoint-qualified reference generation being retired.
    pub subject: Subject,
    /// Additional custody subjects retired by this same worktree operation.
    pub related_subjects: Vec<Subject>,
    pub reference: RefGeneration,
    pub expected_tip: provenant_core::GitOid,
    pub decision: RetirementDecision,
    pub revision: u64,
    pub outcomes: Vec<RetirementStepOutcome>,
}

impl RetirementRecord {
    fn normalise_subjects(subjects: &[Subject]) -> Vec<Subject> {
        let mut subjects = subjects.to_vec();
        subjects.sort_by(|left, right| {
            (left.kind.as_str(), &left.id).cmp(&(right.kind.as_str(), &right.id))
        });
        subjects.dedup();
        subjects
    }

    pub fn identity(
        project: ProjectId,
        related_subjects: &[Subject],
        reference: &RefGeneration,
        expected_tip: provenant_core::GitOid,
        decision: &RetirementDecision,
    ) -> Result<String, crate::CanonicalError> {
        let subject = reference.custody_subject()?;
        let related_subjects = Self::normalise_subjects(related_subjects);
        Ok(crate::CanonicalV1::record_digest(
            "custody-retirement",
            2,
            &(
                project,
                subject,
                related_subjects,
                reference,
                expected_tip,
                decision,
            ),
        )?
        .to_string())
    }

    pub fn plan(
        project: ProjectId,
        related_subjects: Vec<Subject>,
        reference: RefOwnership,
        expected_tip: provenant_core::GitOid,
        decision: RetirementDecision,
    ) -> Result<Self, crate::CanonicalError> {
        let reference = reference.generation_identity()?;
        let subject = reference.custody_subject()?;
        let related_subjects = Self::normalise_subjects(&related_subjects);
        let id = Self::identity(
            project,
            &related_subjects,
            &reference,
            expected_tip,
            &decision,
        )?;
        let outcomes = RetirementStep::for_decision(&related_subjects, &decision)
            .into_iter()
            .map(|step| RetirementStepOutcome {
                step,
                state: RetirementStepState::Pending,
            })
            .collect();
        Ok(Self {
            schema_version: 2,
            id,
            project,
            subject,
            related_subjects,
            reference,
            expected_tip,
            decision,
            revision: 1,
            outcomes,
        })
    }

    fn projection(&self, subject: &Subject) -> RemovalRecord {
        let projected_step = if subject == &self.subject && self.decision.removes_reference() {
            RetirementStep::Reference
        } else {
            RetirementStep::Worktree {
                subject: subject.clone(),
            }
        };
        let state = self
            .outcomes
            .iter()
            .find(|outcome| outcome.step == projected_step)
            .map(|outcome| &outcome.state);
        let outcome = match state {
            Some(RetirementStepState::Completed { evidence }) => RemovalOutcome::Removed {
                evidence: evidence.clone(),
                retirement_id: self.id.clone(),
            },
            _ => RemovalOutcome::Kept {
                cause: RemovalUncertainty::RetirementIncomplete,
            },
        };
        RemovalRecord {
            project: self.project,
            subject: subject.clone(),
            tip: self.expected_tip,
            outcome,
        }
    }

    /// Project each affected custody subject into the existing GC report.
    pub fn removals(&self) -> Vec<RemovalRecord> {
        self.subjects()
            .map(|subject| self.projection(subject))
            .collect()
    }

    /// Validate a step proof against this immutable decision and pinned tip.
    /// Refreshed archive IDs are valid when they remain safe and name the same
    /// expected tip; confinement verifies the captured archive before mutation.
    pub fn accepts_completion_evidence(
        &self,
        step: &RetirementStep,
        evidence: &RemovalEvidence,
    ) -> bool {
        if !self.outcomes.iter().any(|outcome| &outcome.step == step)
            || !step.accepts_completion_evidence(self.decision.evidence(), evidence)
        {
            return false;
        }
        let completion = match step {
            RetirementStep::Worktree { .. } => RetirementDecision::RetireWorktree {
                authority: self.decision.authority(),
                evidence: evidence.clone(),
            },
            RetirementStep::Reference => RetirementDecision::Discard {
                authority: self.decision.authority(),
                evidence: evidence.clone(),
            },
        };
        completion.valid_for(&self.related_subjects, self.expected_tip)
    }

    pub fn removal(&self) -> RemovalRecord {
        self.subjects()
            .next()
            .map(|subject| self.projection(subject))
            .unwrap_or_else(|| self.projection(&self.subject))
    }

    pub fn subjects(&self) -> impl Iterator<Item = &Subject> {
        std::iter::once(&self.subject)
            .filter(move |_| self.decision.removes_reference())
            .chain(self.related_subjects.iter())
    }

    pub fn valid_evidence(&self) -> bool {
        if self
            .related_subjects
            .iter()
            .any(|subject| subject.worktree_identity().is_none())
        {
            return false;
        }
        if !self
            .decision
            .valid_for(&self.related_subjects, self.expected_tip)
        {
            return false;
        }
        if self.outcomes.iter().any(|outcome| {
            matches!(
                &outcome.state,
                RetirementStepState::Completed { evidence }
                    if !self.accepts_completion_evidence(&outcome.step, evidence)
            )
        }) {
            return false;
        }
        match &self.reference.identity.endpoint {
            RefEndpointIdentity::Worktree { path_digest } => {
                let [worktree] = self.related_subjects.as_slice() else {
                    return false;
                };
                let Some(identity) = worktree.worktree_identity() else {
                    return false;
                };
                let expected = crate::CanonicalV1::record_digest(
                    "custody-worktree-head",
                    2,
                    &(self.reference.identity.repository, &identity.path),
                );
                self.reference.identity.reference == "HEAD"
                    && self.reference.generation > 0
                    && worktree.kind == SubjectKind::Worktree
                    && worktree.worktree_identity().is_some()
                    && matches!(&self.decision, RetirementDecision::RetireWorktree { .. })
                    && expected.is_ok_and(|expected| &expected == path_digest)
            }
            RefEndpointIdentity::Local => {
                let refs_valid = self.reference.identity.reference.starts_with("refs/")
                    && self.reference.identity.reference.len() <= 4096
                    && !self
                        .reference
                        .identity
                        .reference
                        .chars()
                        .any(char::is_control);
                let related_worktrees = !self.related_subjects.is_empty()
                    && self
                        .related_subjects
                        .iter()
                        .all(|subject| subject.kind == SubjectKind::Worktree);
                self.reference.generation > 0
                    && refs_valid
                    && match &self.decision {
                        RetirementDecision::Discard { .. } => true,
                        RetirementDecision::RetireWorktree { .. } => related_worktrees,
                    }
            }
            RefEndpointIdentity::Remote { .. } => {
                self.reference.generation > 0
                    && self.reference.identity.reference.starts_with("refs/")
                    && self.reference.identity.reference.len() <= 4096
                    && !self
                        .reference
                        .identity
                        .reference
                        .chars()
                        .any(char::is_control)
                    && matches!(&self.decision, RetirementDecision::Discard { .. })
            }
        }
    }

    pub fn owns_subject(&self, subject: &Subject) -> bool {
        self.subjects().any(|owned| owned == subject)
    }
}

macro_rules! removal_uncertainties {
    ($($variant:ident => $wire:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub enum RemovalUncertainty {
            $(#[serde(rename = $wire)] $variant),+
        }
        impl RemovalUncertainty {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
        }
    };
}
removal_uncertainties! {
    ShallowClone => "shallow_clone",
    ConflictedMergeTree => "conflicted_merge_tree",
    OfflineForge => "offline_forge",
    RewrittenDefaultBranch => "rewritten_default_branch",
    UnprovenLanding => "unproven_landing",
    MissingCommit => "missing_commit",
    RetirementIncomplete => "retirement_incomplete",
    CustodyUnknown => "custody_unknown",
    CensusUnknown => "census_unknown",
    RunRecordsUnknown => "run_records_unknown",
    Busy => "busy",
    DirtyWorktree => "dirty_worktree",
    LockedOrUnavailable => "locked_or_unavailable",
    ObservationFailed => "observation_failed",
}

/// The outcome of considering one subject and tip for removal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RemovalOutcome {
    Removed {
        evidence: RemovalEvidence,
        retirement_id: String,
    },
    Kept {
        cause: RemovalUncertainty,
    },
}

/// Evidence emitted by collection for a removed or uncertainty-retained subject.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalRecord {
    pub project: ProjectId,
    pub subject: Subject,
    pub tip: provenant_core::GitOid,
    pub outcome: RemovalOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CustodyState {
    Active,
    Landed {
        evidence: LandedEvidence,
    },
    /// Points at this module's durable custody-owned retirement record.
    Retired {
        retirement_id: String,
    },
    Released,
}
impl CustodyState {
    pub const fn kind(&self) -> StateKind {
        match self {
            Self::Active => StateKind::Active,
            Self::Landed { .. } => StateKind::Landed,
            Self::Retired { .. } => StateKind::Retired,
            Self::Released => StateKind::Released,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateKind {
    Active,
    Landed,
    Retired,
    Released,
}
impl StateKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Landed => "landed",
            Self::Retired => "retired",
            Self::Released => "released",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyRecord {
    #[serde(flatten)]
    pub extra: ExtraFields,
    pub schema_version: u64,
    pub subject: Subject,
    pub holder: Holder,
    pub project: ProjectId,
    pub origin: Origin,
    pub state: CustodyState,
    pub last_activity: WallTime,
    /// Related artefacts acquired together; landing follows branch/worktree links.
    pub links: Vec<Subject>,
    /// Host-sealed dispatch request digest for a graph association.
    pub provenance: Option<provenant_core::Digest>,
}
/// One query contract for status, session digests and retention.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustodyQuery {
    pub after: u64,
    pub limit: u64,
    pub project: Option<ProjectId>,
    pub state: Option<StateKind>,
    pub holder: Option<Holder>,
    /// Includes lane items nested under this chair, across projects.
    pub chair: Option<Chair>,
}

impl Default for CustodyQuery {
    fn default() -> Self {
        Self {
            after: 0,
            limit: 100,
            project: None,
            state: None,
            holder: None,
            chair: None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyPage {
    pub items: Vec<CustodyRecord>,
    pub next: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyChange {
    pub record: CustodyRecord,
    /// An active prior holder is retained in the event and exposed as a warning.
    pub previous_holder: Option<Holder>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HolderPresence {
    Live,
    Unattended,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyView {
    pub record: CustodyRecord,
    pub presence: HolderPresence,
    pub mine: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyStatus {
    pub items: Vec<CustodyView>,
    pub next: Option<u64>,
}

#[cfg(test)]
#[path = "custody_tests.rs"]
mod tests;
