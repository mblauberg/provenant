use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingWriterProvisioning {
    /// The host's graph/node owner. Present for nested provisioning; a
    /// recovery may reuse only a checkout bearing this exact creation receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<provenant_core::Digest>,
    pub path: String,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancellationRequestReceipt {
    pub request_id: String,
}

pub const MAX_PENDING_GRAPH_STEERS: usize = 16;
pub const MAX_PENDING_GRAPH_STEER_BYTES: usize = 64 * 1024;
pub const MAX_PENDING_GRAPH_STEER_METADATA_BYTES: usize = 4096;
pub const MAX_PENDING_GRAPH_STEER_ENCODED_BYTES: usize = MAX_PENDING_GRAPH_STEER_BYTES * 6
    + MAX_PENDING_GRAPH_STEERS * MAX_PENDING_GRAPH_STEER_METADATA_BYTES;

/// One authorised graph steer waiting for its exact accepted run owner to appear.
/// The stored requester address preserves inbox replay scope; dispatch still
/// revalidates the launch actor, operation, WorkKey and live attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingGraphSteer {
    pub operation_id: provenant_core::OperationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_operation_id: Option<provenant_core::OperationId>,
    pub work_key: provenant_records::WorkKey,
    pub graph_context: crate::inbox::GraphSteerContext,
    pub requester_host: provenant_core::HostId,
    pub requester_agent: String,
    pub requester_actor: provenant_core::ActorId,
    pub text: String,
    /// Accepted text remains available for inspection after a safe refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<PendingSteerRefusal>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingSteerRefusal {
    TargetChanged,
    RequesterChanged,
    OwnerRefused,
    Unsupported,
}

/// Durable, bounded graph-scoped inbox actions. A transparent wrapper keeps the
/// checkpoint wire shape as an operation-keyed map and omits it when empty.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PendingSteerQueue(std::collections::BTreeMap<String, PendingGraphSteer>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingSteerQueueError {
    InvalidKey,
    TooMany,
    TooLarge,
    OperationConflict,
}

impl PendingSteerQueue {
    fn validate_entry(entry: &PendingGraphSteer) -> Result<(), PendingSteerQueueError> {
        if entry.text.len() > provenant_records::control::CONTROL_TEXT_BYTES {
            return Err(PendingSteerQueueError::TooLarge);
        }
        let mut metadata = entry.clone();
        metadata.text.clear();
        if serde_json::to_vec(&metadata)
            .map_err(|_| PendingSteerQueueError::TooLarge)?
            .len()
            > MAX_PENDING_GRAPH_STEER_METADATA_BYTES
        {
            return Err(PendingSteerQueueError::TooLarge);
        }
        Ok(())
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn has_pending(&self) -> bool {
        self.0.values().any(|entry| entry.refusal.is_none())
    }
    pub fn refuse(&mut self, key: &str, reason: PendingSteerRefusal) {
        if let Some(entry) = self.0.get_mut(key) {
            entry.refusal = Some(reason);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &PendingGraphSteer)> {
        self.0.iter()
    }

    pub fn get(&self, key: &str) -> Option<&PendingGraphSteer> {
        self.0.get(key)
    }

    pub fn validate(&self) -> Result<(), PendingSteerQueueError> {
        if self.0.len() > MAX_PENDING_GRAPH_STEERS {
            return Err(PendingSteerQueueError::TooMany);
        }
        let mut bytes = 0usize;
        for (key, entry) in &self.0 {
            if key.is_empty() || key.len() > 128 || key != &entry.operation_id.to_string() {
                return Err(PendingSteerQueueError::InvalidKey);
            }
            Self::validate_entry(entry)?;
            bytes = bytes.saturating_add(entry.text.len());
        }
        if bytes > MAX_PENDING_GRAPH_STEER_BYTES {
            return Err(PendingSteerQueueError::TooLarge);
        }
        Ok(())
    }

    pub fn enqueue(
        &mut self,
        key: String,
        pending: PendingGraphSteer,
    ) -> Result<bool, PendingSteerQueueError> {
        self.validate()?;
        Self::validate_entry(&pending)?;
        if key.is_empty() || key.len() > 128 || key != pending.operation_id.to_string() {
            return Err(PendingSteerQueueError::InvalidKey);
        }
        if let Some(saved) = self.0.get(&key) {
            let mut original = saved.clone();
            original.refusal = None;
            return if original == pending {
                Ok(false)
            } else {
                Err(PendingSteerQueueError::OperationConflict)
            };
        }
        if self.0.len() >= MAX_PENDING_GRAPH_STEERS {
            return Err(PendingSteerQueueError::TooMany);
        }
        if pending.text.len() > provenant_records::control::CONTROL_TEXT_BYTES
            || self.0.values().map(|entry| entry.text.len()).sum::<usize>() + pending.text.len()
                > MAX_PENDING_GRAPH_STEER_BYTES
        {
            return Err(PendingSteerQueueError::TooLarge);
        }
        self.0.insert(key, pending);
        Ok(true)
    }

    pub fn remove(&mut self, key: &str) -> Option<PendingGraphSteer> {
        self.0.remove(key)
    }
}
