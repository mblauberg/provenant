//! Host custody storage is kept outside lane-writable coordination storage.
use crate::*;

pub struct HostStore(pub(crate) Store);
impl HostStore {
    pub fn open_read_only(
        root: &provenant_core::StateRoot,
        deadline: std::time::Instant,
    ) -> Result<Option<Self>> {
        Store::open_read_only_host(root, deadline).map(|store| store.map(Self))
    }
    pub fn open(root: &provenant_core::StateRoot, request: &Request<'_>) -> Result<Self> {
        Store::open_host(root, request).map(Self)
    }
    /// Receipts and custody effects share the host ledger's transaction.
    pub fn coordination_operation<T, E>(
        &self,
        project: ProjectId,
        actor: ActorId,
        id: &provenant_core::OperationId,
        input: &impl Serialize,
        request: &Request<'_>,
        action: impl FnOnce() -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        T: Serialize + DeserializeOwned,
        E: From<Error>,
    {
        self.0
            .coordination_operation(project, actor, id, input, request, action)
    }
}

impl Store {
    pub fn host_custody(&self, request: &Request<'_>) -> Result<HostStore> {
        HostStore::open(&self.state_root, request)
    }
}
