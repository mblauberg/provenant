//! Durable per-head work records. Dispatch and forge policy belong to adapters.
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    pub repository: String,
    pub pr: u64,
    pub head: String,
    pub policy_digest: String,
    pub lens: String,
    pub base_digest: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    NeedsReview,
    Reviewing,
    Verdict,
    AwaitingChecks,
    Landable,
    Landing,
    Landed,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effect {
    Verdict,
    Merge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    pub id: String,
    pub effect: Effect,
    /// A receipt is the remote's stable identifier, not an inferred success.
    pub receipt: Option<String>,
    /// A definitive absence permits retry of this same intent; unknown does not.
    pub retryable: bool,
    /// Fences observations started before a retry.
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub id: String,
    pub lease_owner: String,
    pub lease_until: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub schema_version: u32,
    pub key: Key,
    pub state: State,
    pub cancelled: bool,
    pub attempt: Option<Attempt>,
    pub verdict: Option<String>,
    pub intents: Vec<Intent>,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("unsupported work record version")]
    Version,
    #[error("work record not found")]
    Missing,
    #[error("invalid state transition or stale work")]
    Invalid,
    #[error("lease is held or expired")]
    Lease,
    #[error("publication outcome must be reconciled")]
    Uncertain,
}

/// Caller supplies the database location and timeout; SQLite serialises writers.
pub struct Store {
    db: Connection,
}
impl Store {
    pub fn open(db: Connection) -> Result<Self, Error> {
        db.execute_batch("CREATE TABLE IF NOT EXISTS pr_work (key TEXT PRIMARY KEY, repository TEXT NOT NULL, pr TEXT NOT NULL, head TEXT NOT NULL, body TEXT NOT NULL); CREATE INDEX IF NOT EXISTS pr_work_head ON pr_work(repository, pr, head);")?;
        Ok(Self { db })
    }
    fn id(key: &Key) -> Result<String, Error> {
        Ok(serde_json::to_string(key)?)
    }
    fn get_from(db: &Connection, key: &Key) -> Result<Record, Error> {
        let body: Option<String> = db
            .query_row(
                "SELECT body FROM pr_work WHERE key=?1",
                [Self::id(key)?],
                |r| r.get(0),
            )
            .optional()?;
        let record: Record = serde_json::from_str(&body.ok_or(Error::Missing)?)?;
        if record.schema_version != 1 {
            return Err(Error::Version);
        }
        Ok(record)
    }
    fn save(db: &Connection, record: &Record) -> Result<(), Error> {
        db.execute(
            "UPDATE pr_work SET body=?1 WHERE key=?2",
            params![serde_json::to_string(record)?, Self::id(&record.key)?],
        )?;
        Ok(())
    }
    pub fn get(&self, key: &Key) -> Result<Record, Error> {
        Self::get_from(&self.db, key)
    }
    /// Enumerate work, including cancelled records with effects to reconcile.
    pub fn for_pull_request(&self, repository: &str, pr: u64) -> Result<Vec<Record>, Error> {
        let mut stmt = self
            .db
            .prepare("SELECT body FROM pr_work WHERE repository=?1 AND pr=?2")?;
        let rows = stmt.query_map(params![repository, pr.to_string()], |row| {
            row.get::<_, String>(0)
        })?;
        let mut records = Vec::new();
        for body in rows {
            let record: Record = serde_json::from_str(&body?)?;
            if record.schema_version != 1 {
                return Err(Error::Version);
            }
            records.push(record);
        }
        Ok(records)
    }
    /// The caller supplies an authoritative current head, not an unordered event head.
    pub fn observe_head(&mut self, key: Key) -> Result<Record, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut stmt =
            tx.prepare("SELECT body FROM pr_work WHERE repository=?1 AND pr=?2 AND head<>?3")?;
        let rows = stmt
            .query_map(params![key.repository, key.pr.to_string(), key.head], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for body in rows {
            let mut old: Record = serde_json::from_str(&body)?;
            if old.schema_version != 1 {
                return Err(Error::Version);
            }
            if !old.cancelled {
                old.cancelled = true;
                Self::save(&tx, &old)?;
            }
        }
        let id = Self::id(&key)?;
        let record = Record {
            schema_version: 1,
            key: key.clone(),
            state: State::NeedsReview,
            cancelled: false,
            attempt: None,
            verdict: None,
            intents: vec![],
        };
        tx.execute(
            "INSERT OR IGNORE INTO pr_work(key,repository,pr,head,body) VALUES(?1,?2,?3,?4,?5)",
            params![
                id,
                key.repository,
                key.pr.to_string(),
                key.head,
                serde_json::to_string(&record)?
            ],
        )?;
        let result = Self::get_from(&tx, &key)?;
        tx.commit()?;
        Ok(result)
    }
    fn update<T>(
        &mut self,
        key: &Key,
        f: impl FnOnce(&mut Record) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut record = Self::get_from(&tx, key)?;
        let result = f(&mut record)?;
        Self::save(&tx, &record)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn start(&mut self, key: &Key, attempt: Attempt, now: i64) -> Result<(), Error> {
        self.update(key, |r| {
            if r.cancelled || r.state != State::NeedsReview || r.attempt.is_some() {
                return Err(Error::Invalid);
            }
            if attempt.lease_until <= now || attempt.id.is_empty() || attempt.lease_owner.is_empty()
            {
                return Err(Error::Lease);
            }
            r.attempt = Some(attempt);
            r.state = State::Reviewing;
            Ok(())
        })
    }
    /// Reclaim the existing attempt after expiry; never create another attempt for the key.
    pub fn renew(
        &mut self,
        key: &Key,
        id: &str,
        owner: &str,
        now: i64,
        until: i64,
    ) -> Result<(), Error> {
        self.update(key, |r| {
            if r.cancelled {
                return Err(Error::Invalid);
            }
            let a = r.attempt.as_mut().ok_or(Error::Missing)?;
            if a.id != id
                || owner.is_empty()
                || until <= now
                || (a.lease_until > now && a.lease_owner != owner)
            {
                return Err(Error::Lease);
            }
            a.lease_owner = owner.into();
            a.lease_until = until;
            Ok(())
        })
    }
    fn leased(r: &Record, id: &str, owner: &str, now: i64) -> Result<(), Error> {
        if r.cancelled {
            return Err(Error::Invalid);
        }
        match &r.attempt {
            Some(a) if a.id == id && a.lease_owner == owner && a.lease_until > now => Ok(()),
            _ => Err(Error::Lease),
        }
    }
    pub fn verdict(
        &mut self,
        key: &Key,
        id: &str,
        owner: &str,
        now: i64,
        verdict: String,
    ) -> Result<(), Error> {
        self.update(key, |r| {
            Self::leased(r, id, owner, now)?;
            if r.state != State::Reviewing || r.verdict.is_some() || verdict.is_empty() {
                return Err(Error::Invalid);
            }
            r.verdict = Some(verdict);
            r.state = State::Verdict;
            Ok(())
        })
    }
    pub fn transition(
        &mut self,
        key: &Key,
        id: &str,
        owner: &str,
        now: i64,
        next: State,
    ) -> Result<(), Error> {
        self.update(key, |r| {
            Self::leased(r, id, owner, now)?;
            if !matches!(
                (r.state, next),
                (State::Verdict, State::AwaitingChecks)
                    | (State::AwaitingChecks, State::Landable)
                    | (
                        State::Reviewing | State::Verdict | State::AwaitingChecks | State::Landable,
                        State::Failed
                    )
            ) {
                return Err(Error::Invalid);
            }
            if next == State::AwaitingChecks
                && !r
                    .intents
                    .iter()
                    .any(|i| i.effect == Effect::Verdict && i.receipt.is_some())
            {
                return Err(Error::Uncertain);
            }
            r.state = next;
            Ok(())
        })
    }
    /// Persist before issuing an external effect. The adapter must pin `key.head`.
    pub fn intent(
        &mut self,
        key: &Key,
        attempt: &str,
        owner: &str,
        now: i64,
        id: String,
        effect: Effect,
    ) -> Result<(), Error> {
        self.update(key, |r| {
            Self::leased(r, attempt, owner, now)?;
            let allowed = match effect {
                Effect::Verdict => r.state == State::Verdict,
                Effect::Merge => r.state == State::Landable,
            };
            if !allowed
                || id.is_empty()
                || r.intents.iter().any(|i| i.effect == effect || i.id == id)
            {
                return Err(Error::Invalid);
            }
            r.intents.push(Intent {
                id,
                effect,
                receipt: None,
                retryable: false,
                generation: 0,
            });
            if effect == Effect::Merge {
                r.state = State::Landing;
            }
            Ok(())
        })
    }
    /// Query remote state before deciding whether the same intent may be retried.
    pub fn reconcile_remote(
        &mut self,
        key: &Key,
        intent: &str,
        remote: &mut impl Remote,
    ) -> Result<(), Error> {
        let record = self.get(key)?;
        let pending = record
            .intents
            .iter()
            .find(|item| item.id == intent)
            .ok_or(Error::Missing)?;
        if pending.receipt.is_some() {
            return Ok(());
        }
        let observation = remote.observe(key, pending);
        self.reconcile(key, intent, pending.generation, observation)
    }
    /// Record only authoritative remote observations. Unknown remains unresolved.
    pub fn reconcile(
        &mut self,
        key: &Key,
        intent: &str,
        generation: u64,
        observation: Observation,
    ) -> Result<(), Error> {
        self.update(key, |r| {
            let i = r
                .intents
                .iter_mut()
                .find(|i| i.id == intent)
                .ok_or(Error::Missing)?;
            if i.generation != generation {
                return Err(Error::Uncertain);
            }
            if i.receipt.is_some() {
                return Ok(());
            }
            match observation {
                Observation::Unknown => i.retryable = false,
                Observation::Absent => i.retryable = true,
                Observation::Found(receipt) if !receipt.is_empty() => {
                    i.receipt = Some(receipt);
                    i.retryable = false;
                    if i.effect == Effect::Merge {
                        r.state = State::Landed;
                    }
                }
                Observation::Found(_) => return Err(Error::Invalid),
            }
            Ok(())
        })
    }
    /// Consume permission to retry before issuing the effect; a crash makes it uncertain again.
    pub fn retry(
        &mut self,
        key: &Key,
        attempt: &str,
        owner: &str,
        now: i64,
        intent: &str,
    ) -> Result<(), Error> {
        self.update(key, |r| {
            Self::leased(r, attempt, owner, now)?;
            let i = r
                .intents
                .iter_mut()
                .find(|i| i.id == intent)
                .ok_or(Error::Missing)?;
            if !i.retryable || i.receipt.is_some() {
                return Err(Error::Uncertain);
            }
            i.generation = i.generation.checked_add(1).ok_or(Error::Invalid)?;
            i.retryable = false;
            Ok(())
        })
    }
}
/// Implementations query the exact repository, PR, head and intent. Transport
/// failures and observations that cannot prove a settled absence return Unknown.
pub trait Remote {
    fn observe(&mut self, key: &Key, intent: &Intent) -> Observation;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Observation {
    Unknown,
    Absent,
    Found(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    fn key(head: &str) -> Key {
        Key {
            repository: "example/repo".into(),
            pr: 7,
            head: head.into(),
            policy_digest: "policy".into(),
            lens: "correctness".into(),
            base_digest: "base".into(),
        }
    }
    fn attempt() -> Attempt {
        Attempt {
            id: "a".into(),
            lease_owner: "worker".into(),
            lease_until: 100,
        }
    }
    fn start(s: &mut Store, k: &Key) {
        s.observe_head(k.clone()).unwrap();
        s.start(k, attempt(), 0).unwrap();
    }
    fn verdict(s: &mut Store, k: &Key) {
        s.verdict(k, "a", "worker", 1, "approved".into()).unwrap();
    }
    fn published(s: &mut Store, k: &Key) {
        s.intent(k, "a", "worker", 1, "v".into(), Effect::Verdict)
            .unwrap();
        s.reconcile(k, "v", 0, Observation::Found("review-1".into()))
            .unwrap();
        s.transition(k, "a", "worker", 1, State::AwaitingChecks)
            .unwrap();
    }
    fn landable(s: &mut Store, k: &Key) {
        published(s, k);
        s.transition(k, "a", "worker", 1, State::Landable).unwrap();
    }
    #[test]
    fn state_machine_and_undefined_transitions() {
        let mut s = Store::open(Connection::open_in_memory().unwrap()).unwrap();
        let k = key("one");
        assert_eq!(s.observe_head(k.clone()).unwrap().state, State::NeedsReview);
        assert!(s.transition(&k, "a", "worker", 1, State::Landed).is_err());
        s.start(&k, attempt(), 0).unwrap();
        assert_eq!(s.get(&k).unwrap().state, State::Reviewing);
        assert!(s.transition(&k, "a", "worker", 1, State::Landable).is_err());
        verdict(&mut s, &k);
        assert_eq!(s.get(&k).unwrap().state, State::Verdict);
        assert!(matches!(
            s.transition(&k, "a", "worker", 1, State::AwaitingChecks),
            Err(Error::Uncertain)
        ));
        published(&mut s, &k);
        assert_eq!(s.get(&k).unwrap().state, State::AwaitingChecks);
        s.transition(&k, "a", "worker", 1, State::Landable).unwrap();
        assert_eq!(s.get(&k).unwrap().state, State::Landable);
        s.intent(&k, "a", "worker", 1, "m".into(), Effect::Merge)
            .unwrap();
        assert_eq!(s.get(&k).unwrap().state, State::Landing);
        s.reconcile(&k, "m", 0, Observation::Found("merge-1".into()))
            .unwrap();
        assert_eq!(s.get(&k).unwrap().state, State::Landed);
        assert!(s.transition(&k, "a", "worker", 1, State::Failed).is_err());
        let k2 = key("two");
        start(&mut s, &k2);
        s.transition(&k2, "a", "worker", 1, State::Failed).unwrap();
        assert_eq!(s.get(&k2).unwrap().state, State::Failed);
    }
    #[test]
    fn superseded_head_cannot_publish_or_transfer_verdict() {
        let mut s = Store::open(Connection::open_in_memory().unwrap()).unwrap();
        let a = key("old");
        let b = key("new");
        start(&mut s, &a);
        verdict(&mut s, &a);
        s.observe_head(b.clone()).unwrap();
        assert!(s.get(&a).unwrap().cancelled);
        assert!(
            s.intent(&a, "a", "worker", 1, "v".into(), Effect::Verdict)
                .is_err()
        );
        assert_eq!(s.get(&b).unwrap().verdict, None);
        assert_eq!(s.get(&b).unwrap().state, State::NeedsReview);
    }
    #[test]
    fn duplicate_event_and_lease_recovery_keep_attempt() {
        let mut s = Store::open(Connection::open_in_memory().unwrap()).unwrap();
        let k = key("head");
        start(&mut s, &k);
        assert_eq!(s.observe_head(k.clone()).unwrap().attempt, Some(attempt()));
        assert!(s.start(&k, attempt(), 0).is_err());
        assert!(s.renew(&k, "a", "other", 1, 200).is_err());
        s.renew(&k, "a", "other", 100, 200).unwrap();
        assert!(s.verdict(&k, "a", "worker", 101, "yes".into()).is_err());
        s.verdict(&k, "a", "other", 101, "yes".into()).unwrap();
    }
    #[test]
    fn restart_drill_and_unknown_remote() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("work.db");
        let k = key("head");
        let open = || Store::open(Connection::open(&path).unwrap()).unwrap();
        let mut s = open();
        start(&mut s, &k);
        drop(s);
        let mut s = open();
        assert!(s.start(&k, attempt(), 0).is_err());
        verdict(&mut s, &k);
        drop(s);
        let mut s = open();
        assert!(s.verdict(&k, "a", "worker", 1, "again".into()).is_err());
        s.intent(&k, "a", "worker", 1, "v".into(), Effect::Verdict)
            .unwrap();
        drop(s);
        let mut s = open();
        s.reconcile(&k, "v", 0, Observation::Unknown).unwrap();
        assert!(matches!(
            s.retry(&k, "a", "worker", 1, "v"),
            Err(Error::Uncertain)
        ));
        s.reconcile(&k, "v", 0, Observation::Found("review-1".into()))
            .unwrap();
        drop(s);
        let mut s = open();
        assert!(
            s.intent(&k, "a", "worker", 1, "v2".into(), Effect::Verdict)
                .is_err()
        );
        s.transition(&k, "a", "worker", 1, State::AwaitingChecks)
            .unwrap();
        s.transition(&k, "a", "worker", 1, State::Landable).unwrap();
        s.intent(&k, "a", "worker", 1, "m".into(), Effect::Merge)
            .unwrap();
        drop(s);
        let mut s = open();
        s.reconcile(&k, "m", 0, Observation::Unknown).unwrap();
        assert!(matches!(
            s.retry(&k, "a", "worker", 1, "m"),
            Err(Error::Uncertain)
        ));
        s.reconcile(&k, "m", 0, Observation::Found("merge-1".into()))
            .unwrap();
        drop(s);
        let mut s = open();
        assert_eq!(s.get(&k).unwrap().state, State::Landed);
        assert!(
            s.intent(&k, "a", "worker", 1, "m2".into(), Effect::Merge)
                .is_err()
        );
    }
    #[test]
    fn retry_requires_definitive_absence_each_time() {
        let mut s = Store::open(Connection::open_in_memory().unwrap()).unwrap();
        let k = key("head");
        start(&mut s, &k);
        verdict(&mut s, &k);
        landable(&mut s, &k);
        s.intent(&k, "a", "worker", 1, "m".into(), Effect::Merge)
            .unwrap();
        s.reconcile(&k, "m", 0, Observation::Absent).unwrap();
        s.retry(&k, "a", "worker", 1, "m").unwrap();
        assert!(matches!(
            s.retry(&k, "a", "worker", 1, "m"),
            Err(Error::Uncertain)
        ));
        assert!(matches!(
            s.reconcile(&k, "m", 0, Observation::Absent),
            Err(Error::Uncertain)
        ));
    }
    #[derive(Default)]
    struct FakeRemote {
        verdicts: usize,
        merges: usize,
    }
    impl FakeRemote {
        fn send(&mut self, effect: Effect) -> Observation {
            match effect {
                Effect::Verdict => self.verdicts += 1,
                Effect::Merge => self.merges += 1,
            }
            Observation::Unknown // The effect happened but the response was lost.
        }
        fn lookup(&self, effect: Effect) -> Observation {
            let count = match effect {
                Effect::Verdict => self.verdicts,
                Effect::Merge => self.merges,
            };
            if count == 0 {
                Observation::Absent
            } else {
                Observation::Found(format!("receipt-{effect:?}"))
            }
        }
    }
    impl Remote for FakeRemote {
        fn observe(&mut self, _key: &Key, intent: &Intent) -> Observation {
            self.lookup(intent.effect)
        }
    }
    #[test]
    fn fake_remote_lost_responses_are_reconciled_before_retry() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("db");
        let open = || Store::open(Connection::open(&path).unwrap()).unwrap();
        let mut remote = FakeRemote::default();
        let k = key("head");
        let mut s = open();
        start(&mut s, &k);
        verdict(&mut s, &k);
        s.intent(&k, "a", "worker", 1, "v".into(), Effect::Verdict)
            .unwrap();
        s.reconcile(&k, "v", 0, remote.send(Effect::Verdict))
            .unwrap();
        drop(s);
        let mut s = open();
        assert!(matches!(
            s.retry(&k, "a", "worker", 1, "v"),
            Err(Error::Uncertain)
        ));
        s.reconcile_remote(&k, "v", &mut remote).unwrap();
        s.transition(&k, "a", "worker", 1, State::AwaitingChecks)
            .unwrap();
        s.transition(&k, "a", "worker", 1, State::Landable).unwrap();
        s.intent(&k, "a", "worker", 1, "m".into(), Effect::Merge)
            .unwrap();
        s.reconcile(&k, "m", 0, remote.send(Effect::Merge)).unwrap();
        drop(s);
        let mut s = open();
        assert!(matches!(
            s.retry(&k, "a", "worker", 1, "m"),
            Err(Error::Uncertain)
        ));
        s.reconcile_remote(&k, "m", &mut remote).unwrap();
        assert_eq!((remote.verdicts, remote.merges), (1, 1));
        assert_eq!(s.get(&k).unwrap().state, State::Landed);
    }
    #[test]
    fn failure_from_each_active_stage_and_cancelled_intent_receipt() {
        for stage in [
            State::Reviewing,
            State::Verdict,
            State::AwaitingChecks,
            State::Landable,
        ] {
            let mut s = Store::open(Connection::open_in_memory().unwrap()).unwrap();
            let k = key("old");
            start(&mut s, &k);
            if stage != State::Reviewing {
                verdict(&mut s, &k);
            }
            if matches!(stage, State::AwaitingChecks | State::Landable) {
                published(&mut s, &k);
            }
            if stage == State::Landable {
                s.transition(&k, "a", "worker", 1, State::Landable).unwrap();
            }
            s.transition(&k, "a", "worker", 1, State::Failed).unwrap();
            assert_eq!(s.get(&k).unwrap().state, State::Failed);
            assert!(s.transition(&k, "a", "worker", 1, State::Landable).is_err());
        }
        let mut s = Store::open(Connection::open_in_memory().unwrap()).unwrap();
        let old = key("old");
        start(&mut s, &old);
        verdict(&mut s, &old);
        s.intent(&old, "a", "worker", 1, "v".into(), Effect::Verdict)
            .unwrap();
        s.observe_head(key("new")).unwrap();
        s.reconcile(&old, "v", 0, Observation::Absent).unwrap();
        assert!(s.retry(&old, "a", "worker", 1, "v").is_err());
        s.reconcile(&old, "v", 0, Observation::Found("review".into()))
            .unwrap();
        assert!(s.get(&old).unwrap().cancelled);
        assert!(
            s.transition(&old, "a", "worker", 1, State::AwaitingChecks)
                .is_err()
        );
    }
}
