use crate::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NamedSession {
    pub name: String,
    pub adapter: Option<String>,
    pub provider_session_id: Option<String>,
    pub run_id: Option<String>,
    pub task_id: Option<String>,
    pub attempt: Option<i64>,
    pub result_path: Option<String>,
    pub turn_claim: String,
    pub turn_kind: String,
    pub turn_run_id: Option<String>,
    pub turn_task_id: Option<String>,
    pub turn_attempt: Option<i64>,
    pub turn_status: Option<String>,
    pub turn_pid: Option<i64>,
    pub created_by: String,
    pub updated_by: String,
    pub updated_at: String,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionTurnKind {
    Start,
    Resume,
    Fresh,
}
impl SessionTurnKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Resume => "resume",
            Self::Fresh => "fresh",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLaunch {
    pub run_id: String,
    pub task_id: String,
    pub attempt: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPointer {
    pub adapter: Option<String>,
    pub provider_session_id: Option<String>,
    pub run_id: String,
    pub task_id: String,
    pub attempt: i64,
    pub result_path: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum SessionClaim {
    Claim {
        claim: String,
    },
    Conflict {
        #[serde(skip_serializing_if = "Option::is_none")]
        conflict: Option<Box<NamedSession>>,
    },
}

impl Store {
    pub fn session(&self, project: &str, name: &str) -> Result<Option<NamedSession>> {
        Ok(self.read_sessions(project, Some(name))?.into_iter().next())
    }
    pub fn sessions(&self, project: &str) -> Result<Vec<NamedSession>> {
        self.read_sessions(project, None)
    }
    fn read_sessions(&self, project: &str, name: Option<&str>) -> Result<Vec<NamedSession>> {
        let rows = self.db.prepare("SELECT name, adapter, provider_session_id, run_id, task_id, attempt, result_path,
            turn_claim, turn_kind, turn_run_id, turn_task_id, turn_attempt, turn_status, turn_pid, created_by, updated_by, updated_at
            FROM sessions WHERE project = ? AND (? IS NULL OR name = ?) ORDER BY name")?.query_map(params![project, name, name], |r| Ok((NamedSession {
                name: r.get(0)?, adapter: r.get(1)?, provider_session_id: r.get(2)?, run_id: r.get(3)?, task_id: r.get(4)?, attempt: r.get(5)?, result_path: r.get(6)?,
                turn_claim: r.get(7)?, turn_kind: r.get(8)?, turn_run_id: r.get(9)?, turn_task_id: r.get(10)?, turn_attempt: r.get(11)?, turn_status: r.get(12)?, turn_pid: r.get(13)?,
                created_by: r.get(14)?, updated_by: r.get(15)?, updated_at: String::new(),
            }, r.get::<_, i64>(16)?)))?.collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(mut row, at)| {
                row.updated_at = iso(at)?;
                Ok(row)
            })
            .collect()
    }
    pub fn claim_session_turn(
        &self,
        who: &Identity,
        name: &str,
        observed_claim: Option<&str>,
        kind: SessionTurnKind,
        pid: i64,
        ids: &dyn IdSource,
    ) -> Result<SessionClaim> {
        let claim = ids.uuid()?;
        self.claim_session_turn_with_id(who, name, observed_claim, kind, pid, &claim)
    }
    pub(crate) fn claim_session_turn_with_id(
        &self,
        who: &Identity,
        name: &str,
        observed_claim: Option<&str>,
        kind: SessionTurnKind,
        pid: i64,
        claim: &str,
    ) -> Result<SessionClaim> {
        let now = self.now();
        self.transaction(true, || {
            let existing = self.session(&who.project, name)?;
            if existing.as_ref().map_or(observed_claim.is_some(), |s| s.turn_status.is_none() || Some(s.turn_claim.as_str()) != observed_claim) {
                return Ok(SessionClaim::Conflict { conflict: existing.map(Box::new) });
            }
            self.db.execute("INSERT INTO sessions(project, name, turn_claim, turn_kind, turn_run_id, turn_task_id, turn_attempt, turn_status,
                turn_pid, created_by, updated_by, created_at, updated_at) VALUES (?, ?, ?, ?, NULL, NULL, NULL, NULL, ?, ?, ?, ?, ?)
                ON CONFLICT(project, name) DO UPDATE SET turn_claim = excluded.turn_claim, turn_kind = excluded.turn_kind,
                turn_run_id = excluded.turn_run_id, turn_task_id = excluded.turn_task_id, turn_attempt = excluded.turn_attempt,
                turn_status = NULL, turn_pid = excluded.turn_pid, updated_by = excluded.updated_by, updated_at = excluded.updated_at",
                params![who.project, name, claim, kind.as_str(), pid, who.agent_id, who.agent_id, now, now])?;
            self.log(who, "session", &format!("{name} {}", kind.as_str()))?;
            Ok(SessionClaim::Claim { claim: claim.into() })
        })
    }
    pub fn bind_session_turn(
        &self,
        who: &Identity,
        name: &str,
        claim: &str,
        turn: &SessionLaunch,
    ) -> Result<()> {
        let changed = self.db.execute("UPDATE sessions SET turn_run_id = ?, turn_task_id = ?, turn_attempt = ?, updated_at = ?
            WHERE project = ? AND name = ? AND turn_claim = ? AND turn_status IS NULL", params![turn.run_id, turn.task_id, turn.attempt, self.now(), who.project, name, claim])?;
        if changed != 1 {
            return Err(Error::SessionTurnLost { name: name.into() });
        }
        Ok(())
    }
    pub fn release_session_launcher(&self, who: &Identity, name: &str, claim: &str) -> Result<()> {
        self.db.execute("UPDATE sessions SET turn_pid = NULL WHERE project = ? AND name = ? AND turn_claim = ? AND turn_status IS NULL", params![who.project, name, claim])?;
        Ok(())
    }
    pub fn settle_session_turn(
        &self,
        who: &Identity,
        name: &str,
        claim: &str,
        status: &str,
        advance: Option<&SessionPointer>,
    ) -> Result<()> {
        self.transaction(true, || {
            let Some(row) = self.session(&who.project, name)? else { return Ok(()); };
            if row.turn_claim != claim || row.turn_status.is_some() { return Ok(()); }
            if row.run_id.is_none() && row.turn_run_id.is_none() && advance.is_none() {
                self.db.execute("DELETE FROM sessions WHERE project = ? AND name = ? AND turn_claim = ?", params![who.project, name, claim])?;
                return Ok(());
            }
            self.db.execute("UPDATE sessions SET turn_status = ?, turn_pid = NULL, updated_at = ?,
                adapter = COALESCE(?, adapter), provider_session_id = CASE WHEN ? THEN ? ELSE provider_session_id END,
                run_id = COALESCE(?, run_id), task_id = COALESCE(?, task_id), attempt = COALESCE(?, attempt),
                result_path = CASE WHEN ? THEN ? ELSE result_path END WHERE project = ? AND name = ? AND turn_claim = ?",
                params![status, self.now(), advance.and_then(|a| a.adapter.as_deref()), advance.is_some(), advance.and_then(|a| a.provider_session_id.as_deref()),
                    advance.map(|a| a.run_id.as_str()), advance.map(|a| a.task_id.as_str()), advance.map(|a| a.attempt), advance.is_some(), advance.and_then(|a| a.result_path.as_deref()), who.project, name, claim])?;
            Ok(())
        })
    }
    pub fn forget_session(&self, who: &Identity, name: &str) -> Result<bool> {
        self.transaction(true, || {
            let removed = self.db.execute(
                "DELETE FROM sessions WHERE project = ? AND name = ? AND turn_status IS NOT NULL",
                params![who.project, name],
            )? == 1;
            if removed {
                self.log(who, "session", &format!("{name} forgotten"))?;
            }
            Ok(removed)
        })
    }
}
