use crate::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub message_id: String,
    pub from: String,
    pub body: String,
    pub kind: String,
    pub conversation_id: String,
    pub reply_to: Option<String>,
    pub at: String,
    pub claim_id: Option<String>,
    pub claim_expires_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
}
#[derive(Default)]
pub struct SendOptions<'a> {
    pub kind: Option<&'a str>,
    pub reply_to: Option<&'a str>,
    pub task_id: Option<&'a str>,
    pub output_path: Option<&'a str>,
}
#[derive(Default)]
pub struct RecipientContext<'a> {
    pub chair: Option<&'a str>,
    pub parent: Option<&'a str>,
}
pub struct InboxOptions<'a> {
    pub ids: Option<&'a [String]>,
    pub limit: i64,
    pub peek: bool,
    pub claim_ttl_ms: i64,
    pub busy_timeout_ms: u64,
    pub task_id: Option<&'a str>,
}
impl Default for InboxOptions<'_> {
    fn default() -> Self {
        Self {
            ids: None,
            limit: 20,
            peek: false,
            claim_ttl_ms: 300_000,
            busy_timeout_ms: 5000,
            task_id: None,
        }
    }
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sent {
    pub message_id: String,
    pub recipients: Vec<String>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Acknowledgement {
    pub message_id: String,
    pub acknowledged_at: String,
    pub already_acknowledged: bool,
}
#[derive(Debug, Serialize)]
pub struct InboxDigest {
    pub schema: &'static str,
    pub total: i64,
    pub truncated: bool,
    pub groups: Vec<DigestGroup>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DigestGroup {
    pub from: String,
    pub task_id: Option<String>,
    pub count: i64,
    pub sample_id: String,
    pub summary: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneAttempt {
    pub run_id: String,
    pub task_id: String,
    pub attempt: i64,
}

impl Store {
    pub fn send(
        &self, who: &Identity, to: &str, body: &str, options: SendOptions<'_>,
        context: RecipientContext<'_>, ids: &dyn IdSource,
    ) -> Result<Sent> {
        if options.task_id == Some("") { return Err(Error::EmptyTaskId); }
        if options.output_path == Some("") { return Err(Error::EmptyOutputPath); }
        let id = ids.uuid()?;
        self.send_with_id(who, to, body, options, context, &id)
    }
    pub(crate) fn send_with_id(
        &self,
        who: &Identity,
        to: &str,
        body: &str,
        options: SendOptions<'_>,
        context: RecipientContext<'_>,
        message_id: &str,
    ) -> Result<Sent> {
        if options.task_id == Some("") {
            return Err(Error::EmptyTaskId);
        }
        if options.output_path == Some("") {
            return Err(Error::EmptyOutputPath);
        }
        let now = self.now();
        self.transaction(true, || {
            let recipients = self.resolve_recipients(who, to, context)?;
            if recipients.is_empty() { return Err(self.recipient_fix(who, to)?); }
            let parent = options.reply_to.map(|id| self.db.query_row("SELECT conversation_id FROM messages WHERE project = ? AND message_id = ?", params![who.project, id], |r| r.get::<_, String>(0)).optional()).transpose()?;
            if let Some(id) = options.reply_to { if parent.as_ref().is_none_or(Option::is_none) { return Err(Error::ReplyParentMissing { id: id.into(), project: who.project.clone() }); } }
            if let Some(task) = options.task_id {
                if self.db.query_row("SELECT 1 FROM tasks WHERE project = ? AND task_id = ?", params![who.project, task], |_| Ok(())).optional()?.is_none() { return Err(Error::LinkedTaskMissing { task: task.into(), project: who.project.clone() }); }
            }
            let conversation = parent.flatten().unwrap_or_else(|| message_id.into());
            self.db.execute("INSERT INTO messages(message_id, project, sender_id, body, kind, conversation_id, reply_to, task_id, output_path, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![message_id, who.project, who.agent_id, body, options.kind.unwrap_or("note"), conversation, options.reply_to, options.task_id, options.output_path, now])?;
            for recipient in &recipients {
                self.db.execute("INSERT INTO deliveries(message_id, project, recipient_id, read_at) VALUES (?, ?, ?, NULL)", params![message_id, who.project, recipient])?;
                if options.kind == Some("run_terminal") {
                    if let Some(path) = options.output_path.filter(|p| !p.is_empty()) {
                        let mut parts = path.split(':'); let run = parts.next().unwrap_or(""); let task = parts.next().unwrap_or(""); let attempt = parts.next();
                        let seen = self.db.query_row("SELECT attempt FROM run_observations WHERE project = ? AND recipient_id = ? AND run_id = ? AND task_id = ?", params![who.project, recipient, run, task], |r| r.get::<_, i64>(0)).optional()?;
                        if seen.zip(attempt.and_then(pending::js_number)).is_some_and(|(seen, attempt)| seen as f64 >= attempt) {
                            self.db.execute("UPDATE deliveries SET read_at = ? WHERE message_id = ? AND recipient_id = ?", params![now, message_id, recipient])?;
                        }
                    }
                }
            }
            self.log(who, "send", &format!("to {to}: {}", pending::js_slice(body, 120)))?;
            Ok(Sent { message_id: message_id.into(), recipients })
        })
    }
    pub fn inbox(
        &self,
        who: &Identity,
        options: InboxOptions<'_>,
        ids: &dyn IdSource,
    ) -> Result<Vec<Message>> {
        if !(1..=MAX_SAFE_INTEGER).contains(&options.limit) {
            return Err(Error::InboxLimit);
        }
        if options.task_id == Some("") {
            return Err(Error::EmptyTaskId);
        }
        if !(1..=3_600_000).contains(&options.claim_ttl_ms) {
            return Err(Error::ClaimTtl);
        }
        if options.peek {
            let now = self.now();
            return self.delivery_rows(who, &options, now).and_then(|rows| {
                rows.into_iter()
                    .map(|(mut message, expires)| {
                        message.claim_expires_at =
                            expires.filter(|at| *at > now).map(iso).transpose()?;
                        Ok(message)
                    })
                    .collect()
            });
        }
        validate_timeout(options.busy_timeout_ms)?;
        self.db
            .busy_timeout(Duration::from_millis(options.busy_timeout_ms))?;
        let result = self.transaction(true, || {
            let now = self.now();
            let mut output = Vec::new();
            for (mut message, _) in self.delivery_rows(who, &options, now)? {
                let claim = ids.uuid()?;
                let expires = now.checked_add(options.claim_ttl_ms).ok_or(Error::NumberOutOfRange)?;
                let changed = self.db.execute("INSERT INTO delivery_claims(message_id, project, recipient_id, claim_id, claimed_at, expires_at) VALUES (?, ?, ?, ?, ?, ?)
                    ON CONFLICT(message_id, recipient_id) DO UPDATE SET project = excluded.project, claim_id = excluded.claim_id,
                    claimed_at = excluded.claimed_at, expires_at = excluded.expires_at WHERE delivery_claims.expires_at <= ?", params![message.message_id, who.project, who.agent_id, claim, now, expires, now])?;
                if changed == 1 { message.claim_id = Some(claim); message.claim_expires_at = Some(iso(expires)?); output.push(message); }
            }
            Ok(output)
        });
        if options.busy_timeout_ms != 5000 {
            self.restore_default_busy_timeout()?;
        }
        result
    }
    fn delivery_rows(
        &self,
        who: &Identity,
        options: &InboxOptions<'_>,
        now: i64,
    ) -> Result<Vec<(Message, Option<i64>)>> {
        let ids = options
            .ids
            .map(serde_json::to_string)
            .transpose()
            ?;
        let rows = self.db.prepare("SELECT m.message_id, m.sender_id, m.body, m.kind, m.conversation_id, m.reply_to, m.task_id, m.output_path, m.created_at, c.expires_at
            FROM deliveries d JOIN messages m ON m.message_id = d.message_id LEFT JOIN delivery_claims c ON c.message_id = d.message_id AND c.recipient_id = d.recipient_id
            WHERE d.project = ? AND d.recipient_id = ? AND d.read_at IS NULL AND m.created_at >= ?
            AND (? = 1 OR c.expires_at IS NULL OR c.expires_at <= ?) AND (? IS NULL OR m.task_id = ?)
            AND (? IS NULL OR m.message_id IN (SELECT value FROM json_each(?))) ORDER BY m.created_at, m.message_id LIMIT ?")?.query_map(
            params![who.project, who.agent_id, now.checked_sub(14 * 86400000_i64).ok_or(Error::NumberOutOfRange)?, options.peek, now, options.task_id, options.task_id, ids, ids, options.limit],
            |r| Ok((Message { message_id: r.get(0)?, from: r.get(1)?, body: r.get(2)?, kind: r.get(3)?, conversation_id: r.get(4)?, reply_to: r.get(5)?, task_id: r.get(6)?, output_path: r.get(7)?, at: String::new(), claim_id: None, claim_expires_at: None }, r.get::<_, i64>(8)?, r.get::<_, Option<i64>>(9)?)))?.collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(mut m, at, expires)| {
                m.at = iso(at)?;
                Ok((m, expires))
            })
            .collect()
    }
    pub fn inbox_digest(&self, who: &Identity, task_id: Option<&str>) -> Result<InboxDigest> {
        let since = self
            .now()
            .checked_sub(14 * 86400000_i64)
            .ok_or(Error::NumberOutOfRange)?;
        let predicate = "FROM deliveries d JOIN messages m ON m.message_id = d.message_id WHERE d.project = ? AND d.recipient_id = ? AND d.read_at IS NULL AND m.created_at >= ? AND (? IS NULL OR m.task_id = ?)";
        let args = params![who.project, who.agent_id, since, task_id, task_id];
        let total = self
            .db
            .query_row(&format!("SELECT count(*) {predicate}"), args, |r| r.get(0))?;
        let mut groups = self.db.prepare(&format!("WITH ranked AS (SELECT m.sender_id AS sender, m.task_id AS task_id, m.message_id AS sample_id,
            substr(replace(replace(m.body, char(10), ' '), char(13), ' '), 1, 80) AS summary,
            count(*) OVER (PARTITION BY m.sender_id, m.task_id) AS count,
            row_number() OVER (PARTITION BY m.sender_id, m.task_id ORDER BY m.message_id) AS rank {predicate})
            SELECT sender, task_id, count, sample_id, summary FROM ranked WHERE rank = 1 ORDER BY count DESC, sender, task_id LIMIT 21"))?.query_map(args, |r| Ok(DigestGroup { from: r.get(0)?, task_id: r.get(1)?, count: r.get(2)?, sample_id: r.get(3)?, summary: r.get(4)? }))?.collect::<std::result::Result<Vec<_>, _>>()?;
        let truncated = groups.len() > 20;
        groups.truncate(20);
        Ok(InboxDigest {
            schema: "fabric.inbox_digest.v1",
            total,
            truncated,
            groups,
        })
    }
    pub fn acknowledge(
        &self,
        who: &Identity,
        message_id: &str,
        claim_id: &str,
    ) -> Result<Acknowledgement> {
        self.transaction(true, || {
            let row = self.db.query_row("SELECT d.read_at, c.claim_id, c.expires_at FROM deliveries d LEFT JOIN delivery_claims c ON c.message_id = d.message_id AND c.recipient_id = d.recipient_id WHERE d.project = ? AND d.recipient_id = ? AND d.message_id = ?", params![who.project, who.agent_id, message_id], |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<i64>>(2)?))).optional()?;
            let Some((read, claim, expires)) = row else { return Err(Error::DeliveryMissing { message: message_id.into(), agent: who.agent_id.clone(), project: who.project.clone() }); };
            if claim.as_deref() != Some(claim_id) { return Err(Error::DeliveryNotOwned { claim: claim_id.into(), message: message_id.into() }); }
            if let Some(at) = read { return Ok(Acknowledgement { message_id: message_id.into(), acknowledged_at: iso(at)?, already_acknowledged: true }); }
            let now = self.now();
            if expires.is_none_or(|at| at <= now) { return Err(Error::DeliveryExpired { claim: claim_id.into(), message: message_id.into() }); }
            self.db.execute("UPDATE deliveries SET read_at = ? WHERE project = ? AND recipient_id = ? AND message_id = ? AND read_at IS NULL", params![now, who.project, who.agent_id, message_id])?;
            self.log(who, "ack", message_id)?;
            Ok(Acknowledgement { message_id: message_id.into(), acknowledged_at: iso(now)?, already_acknowledged: false })
        })
    }
    pub fn unseen_lanes(&self, who: &Identity, rows: &[LaneAttempt]) -> Result<Vec<usize>> {
        let mut unseen = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            if row.run_id.is_empty()
                || row.task_id.is_empty()
                || !(0..=MAX_SAFE_INTEGER).contains(&row.attempt)
            {
                continue;
            }
            let prior = self.db.query_row("SELECT attempt FROM run_observations WHERE project = ? AND recipient_id = ? AND run_id = ? AND task_id = ?", params![who.project, who.agent_id, row.run_id, row.task_id], |r| r.get::<_, i64>(0)).optional()?;
            if prior.is_none_or(|a| a < row.attempt) {
                unseen.push(index);
            }
        }
        Ok(unseen)
    }
    pub fn mark_lanes_seen(&self, who: &Identity, rows: &[LaneAttempt]) -> Result<()> {
        self.transaction(true, || {
            for row in rows {
                self.record_observation(who, &row.run_id, &row.task_id, row.attempt)?;
            }
            Ok(())
        })
    }
    fn record_observation(
        &self,
        who: &Identity,
        run: &str,
        task: &str,
        attempt: i64,
    ) -> Result<()> {
        self.db.execute("INSERT INTO run_observations(project, recipient_id, run_id, task_id, attempt) VALUES (?, ?, ?, ?, ?) ON CONFLICT(project, recipient_id, run_id, task_id) DO UPDATE SET attempt = MAX(attempt, excluded.attempt)", params![who.project, who.agent_id, run, task, attempt])?;
        Ok(())
    }
    /// Typed terminal rows are normalised by fabric-runtime before reaching the store.
    pub fn acknowledge_terminal(
        &self,
        who: &Identity,
        run: &str,
        task: &str,
        attempt: i64,
        run_dir: Option<&str>,
        busy_timeout_ms: u64,
    ) -> Result<()> {
        if !(1..=5000).contains(&busy_timeout_ms) {
            return Err(Error::NoticeTimeout);
        }
        let prefix = format!("{run}:{task}:");
        self.db
            .busy_timeout(Duration::from_millis(busy_timeout_ms))?;
        let result = self.transaction(true, || {
            self.record_observation(who, run, task, attempt)?;
            let length = prefix.encode_utf16().count();
            self.db.execute("UPDATE deliveries SET read_at = ? WHERE project = ? AND recipient_id = ? AND read_at IS NULL AND message_id IN (SELECT message_id FROM messages WHERE project = ? AND kind = 'run_terminal' AND ((substr(output_path, 1, ?) = ? AND CAST(substr(output_path, ?) AS INTEGER) <= ?) OR output_path = ?))", params![self.now(), who.project, who.agent_id, who.project, length as i64, prefix, length as i64 + 1, attempt, run_dir])?;
            Ok(())
        });
        self.restore_default_busy_timeout()?;
        result
    }
}

impl Store {
    /// Prerequisite from unit-01, needed by recipient resolution and tests.
    pub fn announce(&self, who: &Identity) -> Result<()> {
        if who.agent_id == "all" {
            return Err(Error::ReservedRecipient);
        }
        let now = self.now();
        self.transaction(true, || {
            let provider = self.db.query_row("SELECT provider FROM agents WHERE project = ? AND agent_id = ?", params![who.project, who.agent_id], |r| r.get::<_, String>(0)).optional()?;
            if provider.is_none() && self.db.query_row("SELECT 1 FROM teams WHERE project = ? AND team_id = ?", params![who.project, who.agent_id], |_| Ok(())).optional()?.is_some() { return Err(Error::AgentTeamCollision { agent: who.agent_id.clone(), project: who.project.clone() }); }
            let changed = self.db.execute("INSERT INTO agents(project, agent_id, provider, first_seen, last_seen) VALUES (?, ?, ?, ?, ?) ON CONFLICT(project, agent_id) DO UPDATE SET last_seen = excluded.last_seen WHERE agents.provider = excluded.provider", params![who.project, who.agent_id, who.provider, now, now])?;
            if changed == 0 { return Err(Error::SeatMismatch { agent: who.agent_id.clone(), project: who.project.clone(), existing: provider.unwrap_or_else(|| "unknown".into()), refused: who.provider.clone() }); } Ok(())
        })
    }
    fn agent_ids(&self, project: &str) -> Result<Vec<String>> {
        Ok(self
            .db
            .prepare("SELECT agent_id FROM agents WHERE project = ? ORDER BY last_seen DESC")?
            .query_map([project], |r| r.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }
    fn recipient_fix(&self, who: &Identity, to: &str) -> Result<Error> {
        let mut agents = self.agent_ids(&who.project)?;
        agents.sort_by_cached_key(|a| a.encode_utf16().collect::<Vec<_>>());
        let teams = self.db.prepare("SELECT DISTINCT team_id FROM team_members WHERE project = ? AND agent_id != ? ORDER BY team_id")?.query_map(params![who.project, who.agent_id], |r| r.get::<_, String>(0))?.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(Error::UnboundRecipient { to: to.into(), agents, teams })
    }
    fn resolve_recipients(
        &self,
        who: &Identity,
        to: &str,
        context: RecipientContext<'_>,
    ) -> Result<Vec<String>> {
        let known = self.agent_ids(&who.project)?;
        let contains = |id: &str| known.iter().any(|k| k == id);
        let chair = context
            .chair
            .filter(|c| !c.is_empty() && contains(c))
            .or_else(|| contains("chair").then_some("chair"));
        let to = if ["chair", "/root", "root", "parent"].contains(&to) {
            let seat = if to == "chair" {
                chair
            } else {
                context
                    .parent
                    .filter(|p| !p.is_empty() && contains(p))
                    .or(chair)
            };
            seat.ok_or_else(|| {
                self.recipient_fix(who, to).unwrap_or_else(|e| e)
            })?
        } else {
            to
        };
        if to == "all" {
            return Ok(known.into_iter().filter(|k| k != &who.agent_id).collect());
        }
        let team = self
            .db
            .prepare("SELECT agent_id FROM team_members WHERE project = ? AND team_id = ?")?
            .query_map(params![who.project, to], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if !team.is_empty() {
            return Ok(team.into_iter().filter(|id| id != &who.agent_id).collect());
        }
        Ok(if contains(to) {
            vec![to.into()]
        } else {
            Vec::new()
        })
    }
}
