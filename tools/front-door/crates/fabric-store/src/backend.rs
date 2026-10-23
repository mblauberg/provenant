//! Object-safe runtime seam. Alternate stores implement policy, not SQL plumbing.
use crate::*;

pub trait MessageStore {
    fn send(&self, who: &Identity, to: &str, body: &str, options: SendOptions<'_>, context: RecipientContext<'_>, ids: &dyn IdSource) -> Result<Sent>;
    fn inbox(&self, who: &Identity, options: InboxOptions<'_>, ids: &dyn IdSource) -> Result<Vec<Message>>;
    fn inbox_digest(&self, who: &Identity, task_id: Option<&str>) -> Result<InboxDigest>;
    fn acknowledge(&self, who: &Identity, message_id: &str, claim_id: &str) -> Result<Acknowledgement>;
}
impl MessageStore for Store {
    fn send(&self, who: &Identity, to: &str, body: &str, options: SendOptions<'_>, context: RecipientContext<'_>, ids: &dyn IdSource) -> Result<Sent> {
        Store::send(self, who, to, body, options, context, ids)
    }
    fn inbox(&self, who: &Identity, options: InboxOptions<'_>, ids: &dyn IdSource) -> Result<Vec<Message>> { Store::inbox(self, who, options, ids) }
    fn inbox_digest(&self, who: &Identity, task_id: Option<&str>) -> Result<InboxDigest> { Store::inbox_digest(self, who, task_id) }
    fn acknowledge(&self, who: &Identity, message_id: &str, claim_id: &str) -> Result<Acknowledgement> { Store::acknowledge(self, who, message_id, claim_id) }
}

pub trait TaskStore {
    fn create_task(&self, who: &Identity, objective: &str, options: CreateTaskOptions<'_>, ids: &dyn IdSource) -> Result<Task>;
    fn tasks(&self, project: &str, state: Option<&str>) -> Result<Vec<Task>>;
    fn update_task(&self, who: &Identity, task_id: &str, state: &str, note: Option<&str>) -> Result<Task>;
    fn claim_task(&self, who: &Identity, task_id: &str) -> Result<Task>;
}
impl TaskStore for Store {
    fn create_task(&self, who: &Identity, objective: &str, options: CreateTaskOptions<'_>, ids: &dyn IdSource) -> Result<Task> { Store::create_task(self, who, objective, options, ids) }
    fn tasks(&self, project: &str, state: Option<&str>) -> Result<Vec<Task>> { Store::tasks(self, project, state) }
    fn update_task(&self, who: &Identity, task_id: &str, state: &str, note: Option<&str>) -> Result<Task> { Store::update_task(self, who, task_id, state, note) }
    fn claim_task(&self, who: &Identity, task_id: &str) -> Result<Task> { Store::claim_task(self, who, task_id) }
}
