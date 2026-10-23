use crate::*;

pub const STALE_LANDING: &str = "stale landing lease: it expired, was released, or belongs to another session or generation; re-acquire with fabric_landing_lease action=acquire (or wait for it first)";
const PUSH_HOLD_MS: i64 = 150_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LandingLease {
    pub holder: String,
    pub generation: i64,
    pub expected_sha: String,
    pub expires_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pushing_until_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub taken_over_from: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ReleaseWire", into = "ReleaseWire")]
pub enum LandingRelease {
    Refused { reason: ReleaseReason },
    Released,
}
#[derive(Serialize, Deserialize)]
struct ReleaseWire {
    released: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<ReleaseReason>,
}
impl TryFrom<ReleaseWire> for LandingRelease {
    type Error = &'static str;
    fn try_from(wire: ReleaseWire) -> std::result::Result<Self, Self::Error> {
        match (wire.released, wire.reason) {
            (true, None) => Ok(Self::Released),
            (false, Some(reason)) => Ok(Self::Refused { reason }),
            _ => Err("a refused release requires a reason; a successful release has none"),
        }
    }
}
impl From<LandingRelease> for ReleaseWire {
    fn from(value: LandingRelease) -> Self {
        match value {
            LandingRelease::Released => Self {
                released: true,
                reason: None,
            },
            LandingRelease::Refused { reason } => Self {
                released: false,
                reason: Some(reason),
            },
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseReason {
    Expired,
    NotHeld,
    Pushing,
}
impl LandingRelease {
    fn refused(reason: ReleaseReason) -> Self {
        Self::Refused { reason }
    }
    pub fn released(&self) -> bool {
        matches!(self, Self::Released)
    }
}
/// Store failures and caller failures stay distinct, including failed cleanup.
#[derive(Debug, thiserror::Error)]
pub enum PushError<E> {
    #[error(transparent)]
    Store(#[from] Error),
    #[error("{0}")]
    Callback(#[source] E),
    #[error("{cleanup}")]
    Cleanup {
        callback: E,
        #[source]
        cleanup: Error,
    },
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PushOutcome<T> {
    pub result: T,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_warning: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct LandingWait {
    pub changed: bool,
    pub lease: Option<LandingLease>,
}

impl Store {
    pub fn landing_lease(&self, project: &str, now: i64) -> Result<Option<LandingLease>> {
        Ok(self.db.query_row("SELECT holder, generation, expected_sha, expires_at, pushing_until FROM landing_leases
            WHERE project = ? AND released_at IS NULL AND (expires_at > ? OR pushing_until > ?)", params![project, now, now], |row| Ok(LandingLease {
            holder: row.get(0)?, generation: row.get(1)?, expected_sha: row.get(2)?, expires_at_ms: row.get(3)?, pushing_until_ms: row.get(4)?, taken_over_from: None,
        })).optional()?)
    }
    /// The caller supplies a bounded poll wait, allowing cancellation and deterministic clocks.
    /// No transaction spans the wait. Push-start under the same generation is not a change.
    pub fn wait_landing(
        &self,
        project: &str,
        seconds: f64,
        poll_ms: u64,
        mut aborted: impl FnMut() -> bool,
        mut wait: impl FnMut(Duration),
    ) -> Result<LandingWait> {
        if !seconds.is_finite() || seconds * 1000.0 > i64::MAX as f64 {
            return Err(Error::NumberOutOfRange);
        }
        let key =
            |lease: &Option<LandingLease>| lease.as_ref().map(|l| (l.holder.clone(), l.generation));
        let start = key(&self.landing_lease(project, self.now())?);
        let deadline = self.now() as f64 + seconds * 1000.0;
        // Independent of the injected clock and wait callback. This also bounds
        // frozen/backwards clocks and callbacks which return without waiting.
        let budget_ms = (seconds * 1000.0).clamp(0.0, 3_600_000.0).ceil() as u64;
        let mut waited_ms = 0_u64;
        loop {
            let lease = self.landing_lease(project, self.now())?;
            if key(&lease) != start {
                return Ok(LandingWait {
                    changed: true,
                    lease,
                });
            }
            let remaining = deadline - self.now() as f64;
            if remaining <= 0.0 || aborted() || waited_ms >= budget_ms {
                return Ok(LandingWait {
                    changed: false,
                    lease,
                });
            }
            let delay = poll_ms
                .max(1)
                .min(remaining.max(1.0) as u64)
                .min(budget_ms - waited_ms);
            waited_ms += delay;
            wait(Duration::from_millis(delay));
        }
    }
    pub fn acquire_landing(
        &self,
        who: &Identity,
        session: &str,
        expected_sha: &str,
        seconds: i64,
        now: i64,
    ) -> Result<LandingLease> {
        let owner = holder(who, session)?;
        let until = expiry(seconds, now)?;
        if expected_sha.len() != 40
            || !expected_sha
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(Error::ExpectedSha);
        }
        self.transaction(true, || {
            if let Some(live) = self.landing_lease(&who.project, now)? { return Err(Error::LandingHeld { holder: live.holder, until: iso(live.expires_at_ms.max(live.pushing_until_ms.unwrap_or(0)))? }); }
            let prior = self.db.query_row("SELECT holder, expires_at, pushing_until, released_at FROM landing_leases WHERE project = ?", [&who.project], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, Option<i64>>(3)?))).optional()?;
            self.db.execute("INSERT INTO landing_leases(project, holder, generation, expected_sha, expires_at, released_at)
                VALUES (?, ?, 1, ?, ?, NULL) ON CONFLICT(project) DO UPDATE SET holder = excluded.holder,
                generation = landing_leases.generation + 1, expected_sha = excluded.expected_sha,
                expires_at = excluded.expires_at, pushing_until = NULL, released_at = NULL", params![who.project, owner, expected_sha, until])?;
            let mut lease = self.landing_lease(&who.project, now)?.ok_or(Error::StaleLanding)?;
            if let Some((previous, expires, push, None)) = prior.filter(|p| p.1 <= now && p.2.unwrap_or(0) <= now) {
                let _ = (expires, push);
                self.log(who, "landing_takeover", &format!("{owner} took over from {previous} generation {}", lease.generation))?;
                lease.taken_over_from = Some(previous);
            } else { self.log(who, "landing_acquire", &format!("{owner} generation {}", lease.generation))?; }
            Ok(lease)
        })
    }
    pub fn verify_landing(
        &self,
        who: &Identity,
        session: &str,
        generation: i64,
        expected_sha: &str,
        now: i64,
    ) -> Result<LandingLease> {
        let lease = self
            .landing_lease(&who.project, now)?
            .ok_or(Error::StaleLanding)?;
        if lease.holder != holder(who, session)? || lease.generation != generation {
            return Err(Error::StaleLanding);
        }
        if lease.expected_sha != expected_sha {
            return Err(Error::ShaMismatch);
        }
        Ok(lease)
    }
    pub fn renew_landing(
        &self,
        who: &Identity,
        session: &str,
        generation: i64,
        seconds: i64,
        now: i64,
    ) -> Result<LandingLease> {
        let owner = holder(who, session)?;
        let until = expiry(seconds, now)?;
        self.transaction(true, || {
            let changed = self.db.execute(
                "UPDATE landing_leases SET expires_at = MAX(expires_at, ?) WHERE project = ?
                AND holder = ? AND generation = ? AND released_at IS NULL AND expires_at > ?",
                params![until, who.project, owner, generation, now],
            )?;
            if changed != 1 {
                return Err(Error::StaleLanding);
            }
            self.log(
                who,
                "landing_renew",
                &format!("{owner} generation {generation}"),
            )?;
            self.landing_lease(&who.project, now)?
                .ok_or(Error::StaleLanding)
        })
    }
    pub fn with_landing_push<T, E>(
        &self,
        who: &Identity,
        session: &str,
        generation: i64,
        expected_sha: &str,
        push: impl FnOnce() -> std::result::Result<T, E>,
    ) -> std::result::Result<PushOutcome<T>, PushError<E>> {
        let owner = holder(who, session)?;
        self.transaction(true, || {
            self.verify_landing(who, session, generation, expected_sha, self.now())?;
            let now = self.now();
            let until = now.checked_add(PUSH_HOLD_MS).ok_or(Error::NumberOutOfRange)?;
            let changed = self.db.execute("UPDATE landing_leases SET pushing_until = ? WHERE project = ? AND generation = ? AND (pushing_until IS NULL OR pushing_until <= ?)", params![until, who.project, generation, now])?;
            if changed != 1 { return Err(Error::PushInProgress); } Ok(())
        })?;
        let mut hold = PushHold {
            store: self,
            project: &who.project,
            owner: &owner,
            generation,
            armed: true,
        };
        let result = match push() {
            Ok(result) => result,
            Err(error) => {
                let cleared = hold.clear();
                hold.armed = false;
                return Err(match cleared {
                    Ok(()) => PushError::Callback(error),
                    Err(cleanup) => PushError::Cleanup {
                        callback: error,
                        cleanup,
                    },
                });
            }
        };
        // A release failure intentionally retains the bounded hold, as in TypeScript.
        hold.armed = false;
        let release = self.transaction(true, || {
            self.db.execute("UPDATE landing_leases SET released_at = ?, pushing_until = NULL WHERE project = ? AND holder = ? AND generation = ? AND released_at IS NULL", params![self.now(), who.project, owner, generation])?;
            self.log(who, "landing_release", &format!("{owner} generation {generation}"))
        });
        Ok(PushOutcome {
            result,
            release_warning: release.err().map(|e| e.to_string()),
        })
    }
    pub fn try_release_landing(
        &self,
        who: &Identity,
        session: &str,
        generation: i64,
        at: Option<i64>,
    ) -> Result<LandingRelease> {
        let owner = holder(who, session)?;
        self.transaction(true, || {
            let now = at.unwrap_or_else(|| self.now());
            let row = self.db.query_row("SELECT holder, generation, expires_at, pushing_until, released_at FROM landing_leases WHERE project = ?", [&who.project], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, Option<i64>>(3)?, r.get::<_, Option<i64>>(4)?))).optional()?;
            let Some((current, gen, expires, pushing, released)) = row else { return Ok(LandingRelease::refused(ReleaseReason::NotHeld)); };
            if current != owner || gen != generation || released.is_some() { return Ok(LandingRelease::refused(ReleaseReason::NotHeld)); }
            if pushing.unwrap_or(0) > now { return Ok(LandingRelease::refused(ReleaseReason::Pushing)); }
            if expires <= now { return Ok(LandingRelease::refused(ReleaseReason::Expired)); }
            self.db.execute("UPDATE landing_leases SET released_at = ? WHERE project = ? AND generation = ?", params![now, who.project, generation])?;
            self.log(who, "landing_release", &format!("{owner} generation {generation}"))?;
            Ok(LandingRelease::Released)
        })
    }
    pub fn release_landing(&self, who: &Identity, session: &str, generation: i64) -> Result<()> {
        if !self
            .try_release_landing(who, session, generation, None)?
            .released()
        {
            return Err(Error::StaleLanding);
        }
        Ok(())
    }
}

/// Only armed across the caller callback. Explicit Result paths propagate write
/// errors; unwinding cannot return an error, so it reports a failed cleanup.
/// Abort/SIGKILL cannot run Drop; the persisted 150-second fence still expires.
struct PushHold<'a> {
    store: &'a Store,
    project: &'a str,
    owner: &'a str,
    generation: i64,
    armed: bool,
}
impl PushHold<'_> {
    fn clear(&self) -> Result<()> {
        self.store.db.execute("UPDATE landing_leases SET pushing_until = NULL WHERE project = ? AND holder = ? AND generation = ? AND released_at IS NULL", params![self.project, self.owner, self.generation])?;
        Ok(())
    }
}
impl Drop for PushHold<'_> {
    fn drop(&mut self) {
        if self.armed {
            if let Err(error) = self.clear() {
                eprintln!("landing push unwind cleanup failed: {error}");
            }
        }
    }
}
