use crate::*;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub task_id: String,
    pub objective: String,
    pub owner: Option<String>,
    pub state: String,
    pub depends_on: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    pub seq: i64,
    pub at: String,
    pub agent_id: String,
    pub kind: String,
    pub detail: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Team {
    pub team_id: String,
    pub members: Vec<String>,
}

#[derive(Default)]
pub struct CreateTaskOptions<'a> {
    pub task_id: Option<&'a str>,
    pub owner: Option<&'a str>,
    pub depends_on: &'a [String],
}

impl Store {
    pub fn create_team(&self, who: &Identity, team_id: &str, members: &[String]) -> Result<Team> {
        if team_id == "all" || members.iter().any(|m| m == "all") {
            return Err(Error::ReservedRecipient);
        }
        let now = self.now();
        let mut effective = Vec::new();
        for member in members {
            if !effective.contains(member) {
                effective.push(member.clone());
            }
        }
        self.transaction(true, || {
            let existing = self
                .db
                .query_row(
                    "SELECT 1 FROM teams WHERE project = ? AND team_id = ?",
                    params![who.project, team_id],
                    |_| Ok(()),
                )
                .optional()?;
            if existing.is_none()
                && self
                    .db
                    .query_row(
                        "SELECT 1 FROM agents WHERE project = ? AND agent_id = ?",
                        params![who.project, team_id],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some()
            {
                return Err(Error::TeamAgentCollision {
                    team: team_id.into(),
                    project: who.project.clone(),
                });
            }
            self.db.execute(
                "INSERT OR IGNORE INTO teams(project, team_id, created_at) VALUES (?, ?, ?)",
                params![who.project, team_id, now],
            )?;
            self.db.execute(
                "DELETE FROM team_members WHERE project = ? AND team_id = ?",
                params![who.project, team_id],
            )?;
            for member in &effective {
                self.db.execute(
                    "INSERT INTO team_members(project, team_id, agent_id) VALUES (?, ?, ?)",
                    params![who.project, team_id, member],
                )?;
            }
            self.log(who, "team", &format!("{team_id}: {}", effective.join(", ")))
        })?;
        Ok(Team {
            team_id: team_id.into(),
            members: effective,
        })
    }
    pub fn create_task(
        &self,
        who: &Identity,
        objective: &str,
        options: CreateTaskOptions<'_>,
        ids: &dyn IdSource,
    ) -> Result<Task> {
        let generated;
        let task = match options.task_id {
            Some(id) => id,
            None => {
                generated = pending::js_slice(&ids.uuid()?, 8);
                &generated
            }
        };
        self.create_task_with_id(who, objective, task, options.owner, options.depends_on)
    }
    /// The runtime supplies the task id, using an eight-character UUID prefix by default.
    pub(crate) fn create_task_with_id(
        &self,
        who: &Identity,
        objective: &str,
        task_id: &str,
        owner: Option<&str>,
        depends_on: &[String],
    ) -> Result<Task> {
        if task_id.is_empty() {
            return Err(Error::EmptyTaskId);
        }
        let now = self.now();
        self.transaction(false, || {
            self.db.execute("INSERT INTO tasks(project, task_id, objective, owner_id, state, created_at, updated_at) VALUES (?, ?, ?, ?, 'open', ?, ?)", params![who.project, task_id, objective, owner, now, now])?;
            for parent in depends_on { self.db.execute("INSERT OR IGNORE INTO task_dependencies(project, task_id, depends_on) VALUES (?, ?, ?)", params![who.project, task_id, parent])?; }
            self.log(who, "task", &format!("{task_id} open: {}", pending::js_slice(objective, 120)))
        })?;
        Ok(Task {
            task_id: task_id.into(),
            objective: objective.into(),
            owner: owner.map(Into::into),
            state: "open".into(),
            depends_on: depends_on.to_vec(),
        })
    }
    pub fn update_task(
        &self,
        who: &Identity,
        task_id: &str,
        state: &str,
        note: Option<&str>,
    ) -> Result<Task> {
        if state == "claimed" {
            return Err(Error::ReservedClaimedState);
        }
        self.transaction(false, || {
            if self.db.execute(
                "UPDATE tasks SET state = ?, updated_at = ? WHERE project = ? AND task_id = ?",
                params![state, self.now(), who.project, task_id],
            )? == 0
            {
                return Err(Error::TaskMissing {
                    task: task_id.into(),
                    project: who.project.clone(),
                });
            }
            self.log(
                who,
                "task",
                &format!(
                    "{task_id} {state}{}",
                    note.map(|n| format!(": {n}")).unwrap_or_default()
                ),
            )?;
            self.task(&who.project, task_id)
        })
    }
    pub fn claim_task(&self, who: &Identity, task_id: &str) -> Result<Task> {
        self.transaction(true, || {
            let changed = self.db.execute("UPDATE tasks SET owner_id = ?, updated_at = ? WHERE project = ? AND task_id = ? AND state = 'open' AND owner_id IS NULL", params![who.agent_id, self.now(), who.project, task_id])?;
            if changed != 1 {
                let current = self.task(&who.project, task_id)?;
                if current.state == "open" && current.owner.as_deref() == Some(&who.agent_id) { return Ok(current); }
                if current.state == "open" { if let Some(owner) = current.owner { return Err(Error::TaskAssigned { task: task_id.into(), owner }); } }
                return Err(Error::TaskNotOpen { task: task_id.into(), project: who.project.clone() });
            }
            self.log(who, "task", &format!("{task_id} claimed by {}", who.agent_id))?;
            self.task(&who.project, task_id)
        })
    }
    pub fn tasks(&self, project: &str, state: Option<&str>) -> Result<Vec<Task>> {
        let mut rows = self.db.prepare("SELECT task_id, objective, owner_id, state FROM tasks WHERE project = ? AND (? IS NULL OR state = ?) ORDER BY created_at")?.query_map(params![project, state, state], |r| Ok(Task { task_id: r.get(0)?, objective: r.get(1)?, owner: r.get(2)?, state: r.get(3)?, depends_on: Vec::new() }))?.collect::<std::result::Result<Vec<_>, _>>()?;
        for row in &mut rows {
            row.depends_on = self
                .db
                .prepare(
                    "SELECT depends_on FROM task_dependencies WHERE project = ? AND task_id = ?",
                )?
                .query_map(params![project, row.task_id], |r| r.get(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
        }
        Ok(rows)
    }
    fn task(&self, project: &str, task_id: &str) -> Result<Task> {
        self.tasks(project, None)?
            .into_iter()
            .find(|t| t.task_id == task_id)
            .ok_or_else(|| Error::TaskVanished {
                task: task_id.into(),
            })
    }
    pub fn note(&self, who: &Identity, detail: &str) -> Result<()> {
        self.log(who, "note", detail)
    }
    pub fn activity(&self, project: &str, limit: Option<i64>) -> Result<Vec<Activity>> {
        self.read_activity(project, None, limit.unwrap_or(50))
    }
    pub fn activity_after(
        &self,
        project: &str,
        after_seq: i64,
        limit: Option<i64>,
    ) -> Result<Vec<Activity>> {
        if !(0..=MAX_SAFE_INTEGER).contains(&after_seq) {
            return Err(Error::ActivityCursor);
        }
        self.read_activity(project, Some(after_seq), limit.unwrap_or(200))
    }
    fn read_activity(
        &self,
        project: &str,
        after: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Activity>> {
        if !(1..=MAX_SAFE_INTEGER).contains(&limit) {
            return Err(Error::ActivityLimit);
        }
        let query = if after.is_some() {
            "SELECT seq, at, agent_id, kind, detail FROM activity WHERE project = ? AND seq > ? ORDER BY seq LIMIT ?"
        } else {
            "SELECT seq, at, agent_id, kind, detail FROM activity WHERE project = ? AND ? IS NULL ORDER BY seq DESC LIMIT ?"
        };
        let rows = self
            .db
            .prepare(query)?
            .query_map(params![project, after, limit], |r| {
                Ok((
                    Activity {
                        seq: r.get(0)?,
                        at: String::new(),
                        agent_id: r.get(2)?,
                        kind: r.get(3)?,
                        detail: r.get(4)?,
                    },
                    r.get::<_, i64>(1)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(mut a, at)| {
                a.at = iso(at)?;
                Ok(a)
            })
            .collect()
    }
}
