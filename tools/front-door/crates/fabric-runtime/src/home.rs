//! One coordination home. Remote calls never mutate a local mirror.
use crate::{coordination::*, hosts::*};
use provenant_core::{ActorId, CancelToken, Clock, OperationId, ProjectId, StateRoot, WaitBudget};
use provenant_fabric_store::{PeerPhase, PeerPhaseChange, Request, RetryWait, Store};

pub enum CoordinationService<'a> {
    Local(Box<LocalCoordination<'a>>),
    Remote(Box<RemoteCoordination<'a>>),
}
pub struct RemoteCoordination<'a> {
    pub transport: Box<dyn PeerTransport + 'a>,
    pub peer: crate::config::Peer,
    pub request: PeerRequest,
    pub journal: Store,
    pub project: ProjectId,
    pub actor: ActorId,
    pub clock: &'a dyn Clock,
    pub backoff: &'a dyn RetryWait,
}
impl<'a> CoordinationService<'a> {
    pub fn open(
        root: &StateRoot,
        project: ProjectId,
        actor: ActorId,
        clock: &'a dyn Clock,
        backoff: &'a dyn RetryWait,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Self, CoordinationError> {
        Ok(Self::Local(Box::new(LocalCoordination::open(
            root, project, actor, clock, backoff, budget, cancel,
        )?)))
    }
    pub fn new(
        store: Store,
        project: ProjectId,
        actor: ActorId,
        clock: &'a dyn Clock,
        backoff: &'a dyn RetryWait,
    ) -> Self {
        Self::Local(Box::new(LocalCoordination::new(
            store, project, actor, clock, backoff,
        )))
    }

    /// Pause a read observation without holding a store transaction.
    pub fn pause_observation(&self, budget: WaitBudget, cancel: &CancelToken) {
        match self {
            Self::Local(local) => local.pause_observation(budget, cancel),
            Self::Remote(remote) => remote.backoff.wait(
                provenant_core::DurationMicros::from_micros(
                    500_000.min(budget.remaining(remote.clock.mono_now()).as_micros()),
                ),
                cancel,
            ),
        }
    }

    pub fn mutate(
        &self,
        input: Mutation,
        id: &OperationId,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<MutationResult, CoordinationError> {
        match self {
            Self::Local(service) => service.mutate(input, id, budget, cancel),
            Self::Remote(remote) => {
                let input = match input {
                    Mutation::Mail(v) => CoordinationInput::Mail(v),
                    Mutation::Task(v) => CoordinationInput::Task(v),
                    Mutation::Events(v) => CoordinationInput::Events(v),
                    Mutation::Claim(v) => CoordinationInput::Claim(v),
                    Mutation::Landing(v) => CoordinationInput::Landing(v),
                };
                Ok(
                    match remote.call(input, Some(id.clone()), budget, cancel)? {
                        CoordinationResult::Mail(v) => MutationResult::Mail(v),
                        CoordinationResult::Task(v) => MutationResult::Task(v),
                        CoordinationResult::Events(v) => MutationResult::Events(v),
                        CoordinationResult::Claim(v) => MutationResult::Claim(v),
                        CoordinationResult::Landing(v) => MutationResult::Landing(v),
                    },
                )
            }
        }
    }
}
impl RemoteCoordination<'_> {
    fn call(
        &self,
        input: CoordinationInput,
        id: Option<OperationId>,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<CoordinationResult, CoordinationError> {
        let operation_id = if input.mutation().is_some() {
            Some(match id {
                Some(id) => id,
                None => OperationId::parse(&provenant_core::hex(
                    &provenant_supervisor::fs::entropy_128().map_err(CoordinationError::Entropy)?,
                ))
                .map_err(|_| CoordinationError::EntropyEncoding)?,
            })
        } else {
            None
        };
        let mut request = self.request.clone();
        request.action = PeerAction::Coordination {
            input,
            operation_id,
        };
        let store_request = Request {
            clock: self.clock,
            budget,
            cancel,
            backoff: self.backoff,
        };
        let response = if let Some(id) = request.operation() {
            let bytes =
                serde_json::to_vec(&request).map_err(CoordinationError::ResponseEncoding)?;
            let operation = self.journal.prepare_peer(
                self.project,
                self.actor,
                &id,
                request.digest()?,
                &bytes,
                &store_request,
            )?;
            match operation.phase {
                PeerPhase::Committed => decode_recorded_response(
                    operation.response.as_deref().ok_or(PeerError::Frame)?,
                )?,
                PeerPhase::Sent | PeerPhase::Unknown => {
                    // Recover the owning home's receipt; never resend mutation.
                    let mut recovery = request.clone();
                    recovery.action = PeerAction::Receipt {
                        operation_id: id.clone(),
                    };
                    let response = self
                        .transport
                        .exchange(&self.peer, &recovery, budget, cancel)
                        .map_err(|error| match error {
                            PeerError::ProtocolMismatch { .. } => error,
                            _ => PeerError::Unknown {
                                operation_id: id.clone(),
                            },
                        })?;
                    if !matches!(response.result, PeerResult::Coordination(_)) {
                        return Err(PeerError::Unknown { operation_id: id }.into());
                    }
                    self.journal.peer_phase(
                        self.project,
                        self.actor,
                        &id,
                        PeerPhaseChange::Committed {
                            response: serde_json::to_vec(&response)
                                .map_err(CoordinationError::ResponseEncoding)?,
                        },
                        &store_request,
                    )?;
                    response
                }
                PeerPhase::Prepared => {
                    self.journal.peer_phase(
                        self.project,
                        self.actor,
                        &id,
                        PeerPhaseChange::Sent,
                        &store_request,
                    )?;
                    match self
                        .transport
                        .exchange(&self.peer, &request, budget, cancel)
                    {
                        Ok(response) => {
                            if let PeerResult::Error {
                                operation_id: Some(response_id),
                                ..
                            } = &response.result
                                && response_id != &id
                            {
                                self.journal.peer_phase(
                                    self.project,
                                    self.actor,
                                    &id,
                                    PeerPhaseChange::Unknown,
                                    &store_request,
                                )?;
                                return Err(PeerError::Frame.into());
                            }
                            let commit = match &response.result {
                                PeerResult::Coordination(_) => true,
                                PeerResult::Error { error, .. } if is_retryable_refusal(error) => {
                                    // Resending is safe here: the owner records a receipt per
                                    // operation id, so a late-applied mutation replays, not repeats.
                                    self.journal
                                        .retry_peer(self.project, self.actor, &id, &store_request)
                                        .map_err(|_| PeerError::Unknown {
                                            operation_id: id.clone(),
                                        })?;
                                    false
                                }
                                PeerResult::Error { error, .. } if is_definitive_refusal(error) => {
                                    true
                                }
                                _ => {
                                    let _ = self.journal.peer_phase(
                                        self.project,
                                        self.actor,
                                        &id,
                                        PeerPhaseChange::Unknown,
                                        &store_request,
                                    );
                                    return Err(PeerError::Unknown { operation_id: id }.into());
                                }
                            };
                            if commit {
                                self.journal
                                    .peer_phase(
                                        self.project,
                                        self.actor,
                                        &id,
                                        PeerPhaseChange::Committed {
                                            response: serde_json::to_vec(&response)
                                                .map_err(CoordinationError::ResponseEncoding)?,
                                        },
                                        &store_request,
                                    )
                                    .map_err(|_| PeerError::Unknown {
                                        operation_id: id.clone(),
                                    })?;
                            }
                            response
                        }
                        Err(error @ PeerError::ProtocolMismatch { .. }) => {
                            self.journal.retry_peer(
                                self.project,
                                self.actor,
                                &id,
                                &store_request,
                            )?;
                            return Err(error.into());
                        }
                        Err(_) => {
                            // Sent already recovers from the owner's receipt if this write fails.
                            let _ = self.journal.peer_phase(
                                self.project,
                                self.actor,
                                &id,
                                PeerPhaseChange::Unknown,
                                &store_request,
                            );
                            return Err(PeerError::Unknown { operation_id: id }.into());
                        }
                    }
                }
            }
        } else {
            self.transport
                .exchange(&self.peer, &request, budget, cancel)?
        };
        match response.result {
            PeerResult::Coordination(value) => Ok(value),
            PeerResult::Error {
                error,
                operation_id,
            } if matches!(error, facade::FacadeError::LaunchUnknown { .. })
                || operation_id
                    .as_ref()
                    .is_some_and(|id| Some(id) != request.operation().as_ref()) =>
            {
                // Uncertain mutations returned above with their locally frozen
                // ID. A read cannot have an uncertain launch or any operation ID.
                Err(PeerError::Frame.into())
            }
            PeerResult::Error { error, .. } => Err(PeerError::Refused { error }.into()),
            _ => Err(PeerError::Frame.into()),
        }
    }
}

fn is_retryable_refusal(error: &facade::FacadeError) -> bool {
    matches!(
        error,
        facade::FacadeError::Busy | facade::FacadeError::Timeout | facade::FacadeError::Cancelled
    )
}

fn is_definitive_refusal(error: &facade::FacadeError) -> bool {
    use facade::FacadeError;
    matches!(
        error,
        FacadeError::InvalidInput
            | FacadeError::NotFound
            | FacadeError::Conflict
            | FacadeError::NotTerminal
            | FacadeError::OperationConflict
            | FacadeError::StaleFence
            | FacadeError::PeerVersion
            | FacadeError::PeerProtocolMismatch { .. }
            | FacadeError::UpgradeRefused
            | FacadeError::HomeUnavailable
    )
}

macro_rules! route {
    ($method:ident,$input:ty,$result:ty,$variant:ident) => {
        fn $method(
            &self,
            input: $input,
            budget: WaitBudget,
            cancel: &CancelToken,
        ) -> Result<$result, CoordinationError> {
            if CoordinationInput::$variant(input.clone())
                .mutation()
                .is_some()
            {
                return Err(CoordinationError::InvalidInput(
                    "mutation requires an operation id",
                ));
            }
            match self {
                Self::Local(service) => service.$method(input, budget, cancel),
                Self::Remote(remote) => {
                    match remote.call(CoordinationInput::$variant(input), None, budget, cancel)? {
                        CoordinationResult::$variant(value) => Ok(value),
                        _ => Err(PeerError::Frame.into()),
                    }
                }
            }
        }
    };
}
impl CoordinationApi for CoordinationService<'_> {
    fn mutate(
        &self,
        input: Mutation,
        operation_id: Option<OperationId>,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<MutationResult, CoordinationError> {
        let operation_id = match operation_id {
            Some(operation_id) => operation_id,
            None => OperationId::parse(&provenant_core::hex(
                &provenant_supervisor::fs::entropy_128().map_err(CoordinationError::Entropy)?,
            ))
            .map_err(|_| CoordinationError::EntropyEncoding)?,
        };
        CoordinationService::mutate(self, input, &operation_id, budget, cancel)
    }

    route!(mail, MailInput, MailResult, Mail);
    route!(task, TaskInput, TaskResult, Task);
    route!(events, EventsInput, EventsResult, Events);
    route!(claim, ClaimInput, ClaimResult, Claim);
    route!(landing, LandingInput, LandingResult, Landing);
}

impl<'a> CoordinationService<'a> {
    /// Composition supplies the already resolved identity and OS owners. A
    /// configured remote home is the only write target, with no local fallback.
    #[allow(clippy::too_many_arguments)]
    pub fn for_identity(
        root: &StateRoot,
        identity: &crate::identity::Identity,
        config: &crate::config::FabricConfig,
        home: &'a std::path::Path,
        cwd: &'a std::path::Path,
        executor: &'a dyn provenant_supervisor::exec::Executor,
        clock: &'a dyn Clock,
        backoff: &'a dyn RetryWait,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Self, CoordinationError> {
        if let Some(project) = &identity.federation
            && let Some(host) = config
                .home_hosts
                .get(project.key.as_str())
                .filter(|h| **h != identity.host)
        {
            let peer = config
                .peers
                .values()
                .find(|p| p.host == *host)
                .cloned()
                .ok_or(PeerError::Unavailable)?;
            let request = PeerRequest {
                version: 1,
                protocol: PROTOCOL,
                target_host: *host,
                source_host: identity.host,
                agent: identity.inbox.agent.clone(),
                project: ProjectLocator::new(identity, home)?,
                action: PeerAction::Health,
            };
            let executable = discover_ssh_executable(config.ssh_executable.as_deref())?;
            let transport = SshTransport {
                executor,
                clock,
                home,
                cwd,
                executable,
                seconds: config.limits.peer_seconds,
            };
            let store_request = Request {
                clock,
                budget,
                cancel,
                backoff,
            };
            let journal = Store::open(root, &store_request)?;
            return Ok(Self::Remote(Box::new(RemoteCoordination {
                transport: Box::new(transport),
                peer,
                request,
                journal,
                project: identity.project,
                actor: identity.inbox.actor,
                clock,
                backoff,
            })));
        }
        Self::open(
            root,
            identity.project,
            identity.inbox.actor,
            clock,
            backoff,
            budget,
            cancel,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::RemoteCoordination;
    use crate::coordination::facade;
    use crate::{
        coordination::{CoordinationError, EventsInput, EventsResult},
        hosts::{
            CoordinationInput, CoordinationResult, PROTOCOL, PeerAction, PeerError, PeerRequest,
            PeerResponse, PeerResult, PeerTransport, ProjectLocator,
        },
        identity::CrossHostProjectKey,
    };
    use provenant_core::{
        ActorId, CancelToken, Clock, HostId, OperationId, ProjectId, RootInputs, StateRoot,
        WaitBudget,
    };
    use provenant_fabric_store::{PeerPhase, PeerPhaseChange, Request, Store, ThreadWait};
    use provenant_supervisor::{clock::SystemClock, fs::LocalFs};
    use std::{
        os::unix::fs::PermissionsExt,
        path::Path,
        sync::{Arc, Mutex},
    };

    #[derive(Default)]
    struct ReceiptPeer {
        actions: Arc<Mutex<Vec<PeerAction>>>,
    }
    impl PeerTransport for ReceiptPeer {
        fn exchange(
            &self,
            _peer: &crate::config::Peer,
            request: &PeerRequest,
            _budget: WaitBudget,
            _cancel: &CancelToken,
        ) -> std::result::Result<PeerResponse, PeerError> {
            let mut actions = self.actions.lock().unwrap();
            let first = actions.is_empty();
            actions.push(request.action.clone());
            if first {
                assert!(matches!(
                    &request.action,
                    PeerAction::Coordination {
                        input: CoordinationInput::Events(EventsInput::Note { .. }),
                        operation_id: Some(_),
                    }
                ));
                // The owner may have committed the mutation, but this caller
                // receives no reply and must recover only via Receipt.
                return Err(PeerError::Unavailable);
            }
            assert!(matches!(&request.action, PeerAction::Receipt { .. }));
            Ok(PeerResponse {
                version: request.version,
                protocol: PROTOCOL,
                host: request.target_host,
                project: request.project.key.clone(),
                result: PeerResult::Coordination(CoordinationResult::Events(EventsResult::Noted {
                    seq: 73,
                })),
            })
        }
    }

    struct TransientThenSuccessPeer {
        actions: Arc<Mutex<Vec<PeerAction>>>,
        error: facade::FacadeError,
    }
    impl PeerTransport for TransientThenSuccessPeer {
        fn exchange(
            &self,
            _peer: &crate::config::Peer,
            request: &PeerRequest,
            _budget: WaitBudget,
            _cancel: &CancelToken,
        ) -> std::result::Result<PeerResponse, PeerError> {
            let mut actions = self.actions.lock().unwrap();
            let first = actions.is_empty();
            actions.push(request.action.clone());
            if first {
                assert!(matches!(
                    &request.action,
                    PeerAction::Coordination {
                        input: CoordinationInput::Events(EventsInput::Note { .. }),
                        operation_id: Some(_),
                    }
                ));
                return Ok(PeerResponse {
                    version: request.version,
                    protocol: PROTOCOL,
                    host: request.target_host,
                    project: request.project.key.clone(),
                    result: PeerResult::Error {
                        error: self.error.clone(),
                        operation_id: request.operation(),
                    },
                });
            }

            assert!(matches!(
                &request.action,
                PeerAction::Coordination {
                    input: CoordinationInput::Events(EventsInput::Note { .. }),
                    operation_id: Some(_),
                }
            ));
            Ok(PeerResponse {
                version: request.version,
                protocol: PROTOCOL,
                host: request.target_host,
                project: request.project.key.clone(),
                result: PeerResult::Coordination(CoordinationResult::Events(EventsResult::Noted {
                    seq: 73,
                })),
            })
        }
    }

    struct RemoteFixture {
        _temporary: tempfile::TempDir,
        root: StateRoot,
        clock: SystemClock,
        cancel: CancelToken,
        backoff: ThreadWait,
        budget: WaitBudget,
        local_host: HostId,
        remote_host: HostId,
        project: ProjectId,
        actor: ActorId,
        project_key: CrossHostProjectKey,
    }
    impl RemoteFixture {
        fn new() -> Self {
            let temporary = crate::test_support::tempdir().unwrap();
            std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
            let base = temporary.path().canonicalize().unwrap();
            let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
            inputs.state_dir = Some(base.join("state"));
            let root: StateRoot = inputs.resolve(&LocalFs).unwrap().state;
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let backoff = ThreadWait;
            let budget = WaitBudget::cli(clock.mono_now(), 30).unwrap();
            let local_host = HostId::from_bytes([81; 16]);
            let remote_host = HostId::from_bytes([82; 16]);
            let project_key = CrossHostProjectKey::from_home(
                Path::new("/home/local"),
                Path::new("/home/local/project/.git"),
            )
            .unwrap();
            Self {
                _temporary: temporary,
                root,
                clock,
                cancel,
                backoff,
                budget,
                local_host,
                remote_host,
                project: ProjectId::from_bytes([83; 16]),
                actor: ActorId::from_bytes([84; 16]),
                project_key,
            }
        }

        fn remote<'a>(&'a self, transport: Box<dyn PeerTransport + 'a>) -> RemoteCoordination<'a> {
            let request = PeerRequest {
                version: 1,
                protocol: PROTOCOL,
                target_host: self.remote_host,
                source_host: self.local_host,
                agent: "chair".into(),
                project: ProjectLocator {
                    key: self.project_key.clone(),
                    common_dir: "project/.git".into(),
                },
                action: PeerAction::Health,
            };
            let store_request = Request {
                clock: &self.clock,
                budget: self.budget,
                cancel: &self.cancel,
                backoff: &self.backoff,
            };
            let journal = Store::open(&self.root, &store_request).unwrap();
            RemoteCoordination {
                transport,
                peer: crate::config::Peer {
                    host: self.remote_host,
                    destination: "worker".into(),
                    remote_executable: None,
                    connect_timeout_seconds: 10,
                },
                request,
                journal,
                project: self.project,
                actor: self.actor,
                clock: &self.clock,
                backoff: &self.backoff,
            }
        }
    }

    fn note(text: &str) -> CoordinationInput {
        CoordinationInput::Events(EventsInput::Note {
            note: text.to_owned(),
            dedup_key: Some("handoff-note-1".into()),
        })
    }

    fn noted(result: CoordinationResult) -> i64 {
        match result {
            CoordinationResult::Events(EventsResult::Noted { seq }) => seq,
            _ => panic!("expected a noted event receipt"),
        }
    }

    #[test]
    fn remote_coordination_preserves_protocol_mismatch_on_mutation_and_recovery() {
        struct WrongProtocol;
        impl PeerTransport for WrongProtocol {
            fn exchange(
                &self,
                peer: &crate::config::Peer,
                _: &PeerRequest,
                _: WaitBudget,
                _: &CancelToken,
            ) -> Result<PeerResponse, PeerError> {
                Err(PeerError::ProtocolMismatch {
                    peer: peer.destination.clone(),
                    remote: 1,
                })
            }
        }
        let fixture = RemoteFixture::new();
        let remote = fixture.remote(Box::new(WrongProtocol));
        let id = OperationId::parse("wrong-protocol").unwrap();
        for recovery in [false, false, true] {
            if recovery {
                remote
                    .journal
                    .peer_phase(
                        fixture.project,
                        fixture.actor,
                        &id,
                        PeerPhaseChange::Sent,
                        &Request {
                            clock: &fixture.clock,
                            budget: fixture.budget,
                            cancel: &fixture.cancel,
                            backoff: &fixture.backoff,
                        },
                    )
                    .unwrap();
            }
            let error = remote
                .call(
                    note("hello"),
                    Some(id.clone()),
                    fixture.budget,
                    &fixture.cancel,
                )
                .unwrap_err();
            assert_eq!(error.code(), "peer_version");
            assert_eq!(
                error.to_string(),
                format!("peer worker speaks protocol 1, this host speaks {PROTOCOL}; upgrade both")
            );
            let operation = remote
                .journal
                .lookup_peer(
                    fixture.project,
                    fixture.actor,
                    &id,
                    &Request {
                        clock: &fixture.clock,
                        budget: fixture.budget,
                        cancel: &fixture.cancel,
                        backoff: &fixture.backoff,
                    },
                )
                .unwrap()
                .unwrap();
            assert_eq!(
                operation.phase,
                if recovery {
                    PeerPhase::Sent
                } else {
                    PeerPhase::Prepared
                }
            );
        }
    }

    #[test]
    fn remote_coordination_read_rejects_forged_operation_errors() {
        struct ErrorPeer {
            error: facade::FacadeError,
            operation_id: Option<OperationId>,
        }
        impl PeerTransport for ErrorPeer {
            fn exchange(
                &self,
                _peer: &crate::config::Peer,
                request: &PeerRequest,
                _budget: WaitBudget,
                _cancel: &CancelToken,
            ) -> Result<PeerResponse, PeerError> {
                assert!(request.operation().is_none());
                Ok(PeerResponse {
                    version: request.version,
                    protocol: PROTOCOL,
                    host: request.target_host,
                    project: request.project.key.clone(),
                    result: PeerResult::Error {
                        error: self.error.clone(),
                        operation_id: self.operation_id.clone(),
                    },
                })
            }
        }
        let launch_unknown = facade::FacadeError::LaunchUnknown {
            operation_id: OperationId::parse("forged").unwrap(),
        };
        for (error, id, corrupt) in [
            (launch_unknown.clone(), Some("forged"), true),
            (launch_unknown, None, true),
            (facade::FacadeError::Busy, Some("forged"), true),
            (facade::FacadeError::Busy, None, false),
        ] {
            let fixture = RemoteFixture::new();
            let remote = fixture.remote(Box::new(ErrorPeer {
                error,
                operation_id: id.map(|id| OperationId::parse(id).unwrap()),
            }));
            let error = remote
                .call(
                    CoordinationInput::Events(EventsInput::Read {
                        after: None,
                        limit: 1,
                        wait_seconds: None,
                    }),
                    None,
                    fixture.budget,
                    &fixture.cancel,
                )
                .unwrap_err();
            if corrupt {
                assert!(matches!(error, CoordinationError::Peer(PeerError::Frame)));
                assert_eq!(error.code(), "state_corrupt");
            } else {
                assert!(matches!(
                    error,
                    CoordinationError::Peer(PeerError::Refused { .. })
                ));
                assert_eq!(error.code(), "busy");
            }
        }
    }

    #[test]
    fn remote_coordination_forged_refusal_stays_unknown() {
        struct ForgedPeer(facade::FacadeError);
        impl PeerTransport for ForgedPeer {
            fn exchange(
                &self,
                _peer: &crate::config::Peer,
                request: &PeerRequest,
                _budget: WaitBudget,
                _cancel: &CancelToken,
            ) -> Result<PeerResponse, PeerError> {
                Ok(PeerResponse {
                    version: request.version,
                    protocol: PROTOCOL,
                    host: request.target_host,
                    project: request.project.key.clone(),
                    result: PeerResult::Error {
                        error: self.0.clone(),
                        operation_id: Some(OperationId::parse("forged").unwrap()),
                    },
                })
            }
        }
        for error in [
            facade::FacadeError::Conflict,
            facade::FacadeError::Busy,
            facade::FacadeError::LaunchUnknown {
                operation_id: OperationId::parse("forged").unwrap(),
            },
        ] {
            let fixture = RemoteFixture::new();
            let remote = fixture.remote(Box::new(ForgedPeer(error)));
            let operation = OperationId::parse("real-operation").unwrap();
            let error = remote
                .call(
                    note("forged refusal"),
                    Some(operation.clone()),
                    fixture.budget,
                    &fixture.cancel,
                )
                .unwrap_err();
            assert!(matches!(error, CoordinationError::Peer(PeerError::Frame)));
            assert_eq!(error.code(), "state_corrupt");
            let request = Request {
                clock: &fixture.clock,
                budget: fixture.budget,
                cancel: &fixture.cancel,
                backoff: &fixture.backoff,
            };
            let pending = remote
                .journal
                .lookup_peer(fixture.project, fixture.actor, &operation, &request)
                .unwrap()
                .unwrap();
            assert_eq!(pending.phase, PeerPhase::Unknown);
            assert!(pending.response.is_none());
        }
    }

    #[test]
    fn remote_coordination_recovers_lost_reply_by_receipt_then_replays_locally() {
        let fixture = RemoteFixture::new();
        let operation = OperationId::parse("coordination-lost-reply-1").unwrap();
        let peer_actions = Arc::new(Mutex::new(Vec::new()));
        let remote = fixture.remote(Box::new(ReceiptPeer {
            actions: Arc::clone(&peer_actions),
        }));

        assert!(matches!(
            remote.call(
                note("first body"),
                Some(operation.clone()),
                fixture.budget,
                &fixture.cancel,
            ),
            Err(CoordinationError::Peer(PeerError::Unknown { operation_id }))
                if operation_id == operation
        ));
        let actions_after_loss = peer_actions.lock().unwrap().clone();
        assert_eq!(actions_after_loss.len(), 1);
        assert!(matches!(
            &actions_after_loss[0],
            PeerAction::Coordination { operation_id: Some(id), .. } if id == &operation
        ));
        let journal_request = Request {
            clock: &fixture.clock,
            budget: fixture.budget,
            cancel: &fixture.cancel,
            backoff: &fixture.backoff,
        };
        let pending = remote
            .journal
            .lookup_peer(fixture.project, fixture.actor, &operation, &journal_request)
            .unwrap()
            .unwrap();
        assert_eq!(pending.phase, PeerPhase::Unknown);

        assert_eq!(
            noted(
                remote
                    .call(
                        note("first body"),
                        Some(operation.clone()),
                        fixture.budget,
                        &fixture.cancel,
                    )
                    .unwrap()
            ),
            73
        );
        let actions_after_receipt = peer_actions.lock().unwrap().clone();
        assert_eq!(actions_after_receipt.len(), 2);
        assert!(matches!(
            &actions_after_receipt[1],
            PeerAction::Receipt { operation_id } if operation_id == &operation
        ));
        let committed = remote
            .journal
            .lookup_peer(fixture.project, fixture.actor, &operation, &journal_request)
            .unwrap()
            .unwrap();
        assert_eq!(committed.phase, PeerPhase::Committed);

        // A committed same-body replay is answered from the journal.
        assert_eq!(
            noted(
                remote
                    .call(
                        note("first body"),
                        Some(operation.clone()),
                        fixture.budget,
                        &fixture.cancel,
                    )
                    .unwrap()
            ),
            73
        );
        assert_eq!(peer_actions.lock().unwrap().len(), 2);

        // Same key with a changed mutation conflicts before contacting the peer.
        assert!(matches!(
            remote.call(
                note("changed body"),
                Some(operation),
                fixture.budget,
                &fixture.cancel,
            ),
            Err(CoordinationError::Store(
                provenant_fabric_store::Error::Conflict
            ))
        ));
        assert_eq!(peer_actions.lock().unwrap().len(), 2);
    }

    #[test]
    fn remote_coordination_transient_error_remains_retryable() {
        for error in [
            facade::FacadeError::Busy,
            facade::FacadeError::Timeout,
            facade::FacadeError::Cancelled,
        ] {
            let fixture = RemoteFixture::new();
            let actions = Arc::new(Mutex::new(Vec::new()));
            let remote = fixture.remote(Box::new(TransientThenSuccessPeer {
                actions: Arc::clone(&actions),
                error: error.clone(),
            }));
            let operation = OperationId::parse("transient-peer-operation-1").unwrap();

            assert!(matches!(
                remote.call(
                    note("retry transient operation"),
                    Some(operation.clone()),
                    fixture.budget,
                    &fixture.cancel,
                ),
                Err(CoordinationError::Peer(PeerError::Refused { error: actual }))
                    if actual == error
            ));
            assert_eq!(
                noted(
                    remote
                        .call(
                            note("retry transient operation"),
                            Some(operation.clone()),
                            fixture.budget,
                            &fixture.cancel,
                        )
                        .unwrap()
                ),
                73
            );
            assert_eq!(actions.lock().unwrap().len(), 2);
            assert!(matches!(
                &actions.lock().unwrap()[0],
                PeerAction::Coordination { operation_id: Some(id), .. } if id == &operation
            ));
            assert!(matches!(
                &actions.lock().unwrap()[1],
                PeerAction::Coordination { operation_id: Some(id), .. } if id == &operation
            ));
        }
    }
}
