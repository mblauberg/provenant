//! Repository issue/path claims, fenced by the persisted generation.
use crate::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkClaim {
    pub id: String,
    pub generation: i64,
    pub holder: String,
    pub issue: Option<String>,
    pub paths: Vec<String>,
    pub expires_at_ms: i64,
}
#[derive(Default)]
pub struct WorkScope<'a> {
    pub issue: Option<&'a str>,
    pub paths: &'a [String],
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub agent_id: String,
    pub provider: String,
    pub last_seen: String,
}
impl Store {
    pub fn close(self) -> Result<()> {
        self.db.close().map_err(|(_, error)| error.into())
    }
    pub fn agents(&self, project: &str) -> Result<Vec<Agent>> {
        let rows=self.db.prepare("SELECT agent_id, provider, last_seen FROM agents WHERE project = ? ORDER BY last_seen DESC")?
            .query_map([project], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?)))?
            .collect::<std::result::Result<Vec<_>,_>>()?;
        rows.into_iter()
            .map(|(agent_id, provider, at)| {
                Ok(Agent {
                    agent_id,
                    provider,
                    last_seen: iso(at)?,
                })
            })
            .collect()
    }
    pub fn projects(&self) -> Result<Vec<String>> {
        Ok(self
            .db
            .prepare("SELECT DISTINCT project FROM agents ORDER BY project")?
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }
    pub fn work_claims(&self, project: &str, now: i64) -> Result<Vec<WorkClaim>> {
        let rows=self.db.prepare("SELECT id, generation, holder, issue, paths, expires_at FROM work_claims WHERE project = ? AND released_at IS NULL AND expires_at > ? ORDER BY generation")?
            .query_map(params![project,now], |r| Ok((WorkClaim {id:r.get(0)?,generation:r.get(1)?,holder:r.get(2)?,issue:r.get(3)?,paths:Vec::new(),expires_at_ms:r.get(5)?},r.get::<_,String>(4)?)))?
            .collect::<std::result::Result<Vec<_>,_>>()?;
        rows.into_iter()
            .map(|(mut claim, paths)| {
                claim.paths =
                    serde_json::from_value(provenant_records::checked_json::from_str(&paths)?)?;
                Ok(claim)
            })
            .collect()
    }
    pub fn acquire_work(
        &self,
        who: &Identity,
        session: &str,
        scope: WorkScope<'_>,
        seconds: i64,
        now: i64,
        ids: &dyn IdSource,
    ) -> Result<WorkClaim> {
        let owner = holder(who, session)?;
        let paths = normal_paths(scope.paths)?;
        let issue = normal_issue(scope.issue);
        if issue.is_none() && paths.is_empty() {
            return Err(Error::EmptyWorkScope);
        }
        let until = expiry(seconds, now)?;
        let id = ids.uuid()?;
        self.transaction(true,|| {
            for existing in self.work_claims(&who.project,now)? {
                if (issue.is_some() && issue==normal_issue(existing.issue.as_deref())) || paths.iter().any(|p|existing.paths.iter().any(|q|overlaps(p,q))) {
                    return Err(Error::WorkHeld { holder:existing.holder, until:iso(existing.expires_at_ms)? });
                }
            }
            self.db.execute("INSERT INTO work_claims(id, project, holder, issue, paths, expires_at) VALUES (?, ?, ?, ?, ?, ?)",params![id,who.project,owner,issue,serde_json::to_string(&paths)?,until])?;
            let generation=self.db.last_insert_rowid();
            self.log(who,"work_claim",&format!("{owner} {} {}",issue.as_deref().unwrap_or(""),paths.join(",")))?;
            Ok(WorkClaim {id,generation,holder:owner,issue,paths,expires_at_ms:until})
        })
    }
    pub fn renew_work(
        &self,
        who: &Identity,
        session: &str,
        id: &str,
        generation: i64,
        seconds: i64,
        now: i64,
    ) -> Result<WorkClaim> {
        let owner = holder(who, session)?;
        let until = expiry(seconds, now)?;
        self.transaction(true,|| {
            let changed=self.db.execute("UPDATE work_claims SET expires_at = MAX(expires_at, ?) WHERE id = ? AND project = ? AND holder = ? AND generation = ? AND released_at IS NULL AND expires_at > ?",params![until,id,who.project,owner,generation,now])?;
            if changed!=1 {return Err(Error::StaleWork);}
            self.log(who,"work_renew",&format!("{owner} {id} generation {generation}"))?;
            self.work_claims(&who.project,now)?.into_iter().find(|c|c.id==id).ok_or(Error::StaleWork)
        })
    }
    pub fn verify_work(
        &self,
        who: &Identity,
        session: &str,
        id: &str,
        generation: i64,
        now: i64,
    ) -> Result<WorkClaim> {
        let claim = self
            .work_claims(&who.project, now)?
            .into_iter()
            .find(|c| c.id == id)
            .ok_or(Error::StaleWork)?;
        if claim.holder != holder(who, session)? || claim.generation != generation {
            return Err(Error::StaleWork);
        }
        Ok(claim)
    }
    pub fn release_work(
        &self,
        who: &Identity,
        session: &str,
        id: &str,
        generation: i64,
        now: i64,
    ) -> Result<()> {
        let owner = holder(who, session)?;
        self.transaction(true,|| {
            let changed=self.db.execute("UPDATE work_claims SET released_at = ? WHERE id = ? AND project = ? AND holder = ? AND generation = ? AND released_at IS NULL AND expires_at > ?",params![now,id,who.project,owner,generation,now])?;
            if changed!=1 {return Err(Error::StaleWork);}
            self.log(who,"work_release",&format!("{owner} {id} generation {generation}"))
        })
    }
}
fn normal_issue(issue: Option<&str>) -> Option<String> {
    let clean = issue?.trim_matches(pending::js_whitespace);
    let clean = clean.strip_prefix('#').unwrap_or(clean);
    (!clean.is_empty()).then(|| clean.into())
}
fn normal_paths(paths: &[String]) -> Result<Vec<String>> {
    if paths.len() > 100 {
        return Err(Error::TooManyPaths);
    }
    let mut clean = Vec::new();
    for path in paths {
        let value = path.replace('\\', "/");
        let value = value.strip_suffix('/').unwrap_or(&value);
        if value.is_empty()
            || value.starts_with('/')
            || value.split('/').any(|p| matches!(p, "" | ".." | "."))
        {
            return Err(Error::InvalidWorkPath { path: path.clone() });
        }
        clean.push(value.to_lowercase());
    }
    clean.sort_by_cached_key(|s| s.encode_utf16().collect::<Vec<_>>());
    clean.dedup();
    Ok(clean)
}
fn overlaps(left: &str, right: &str) -> bool {
    let left = left.to_lowercase();
    let right = right.to_lowercase();
    left == right
        || left.starts_with(&format!("{right}/"))
        || right.starts_with(&format!("{left}/"))
}
