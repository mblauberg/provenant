//! Serial reconciliation of durable hints against a forge adapter.
//!
//! The caller owns scheduling and serialises access to this state. A failed
//! request never advances the sweep watermark or completes a delivery.
use super::{Intake, Target, gh::Forge};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Clone, Default, Serialize, Deserialize)]
struct Cached {
    etag: Option<String>,
    body: serde_json::Value,
}

/// Observable counters and timing from the reconciliation queue.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Status {
    pub requests: u64,
    pub responses_200: u64,
    pub responses_304: u64,
    #[serde(default)]
    pub ratio_304_to_200: Option<String>,
    pub rate_limit: BTreeMap<String, String>,
    pub last_successful_sweep: Option<u64>,
    pub oldest_pending_marker: Option<u64>,
    pub retry_at: Option<u64>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct State {
    version: u32,
    #[serde(default)]
    repository: String,
    status: Status,
    cache: BTreeMap<String, Cached>,
    /// Current data, keyed by PR number and REST resource name.
    current: BTreeMap<u64, BTreeMap<String, serde_json::Value>>,
}

/// Persistent reconciliation state, separate from delivery admission.
pub struct Reconciler {
    path: PathBuf,
    repository: String,
    interval: u64,
    state: State,
}

impl Reconciler {
    pub fn open(path: impl AsRef<Path>, repository: String, interval: u64) -> io::Result<Self> {
        if interval == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "sweep interval must be positive",
            ));
        }
        let path = path.as_ref().to_owned();
        let mut state = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<State>(&bytes).map_err(io::Error::other)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => State {
                version: 1,
                ..State::default()
            },
            Err(e) => return Err(e),
        };
        if state.version != 1 {
            return Err(io::Error::other("unsupported reconciliation state version"));
        }
        if !state.repository.is_empty() && state.repository != repository {
            return Err(io::Error::other(
                "reconciliation state belongs to another repository",
            ));
        }
        state.repository = repository.clone();
        Ok(Self {
            path,
            repository,
            interval,
            state,
        })
    }

    pub fn status(&self, intake: &Intake) -> Status {
        let mut status = self.state.status.clone();
        status.ratio_304_to_200 = (status.responses_200 != 0).then(|| {
            format!(
                "{:.3}",
                status.responses_304 as f64 / status.responses_200 as f64
            )
        });
        status.oldest_pending_marker = intake
            .markers()
            .values()
            .filter(|m| m.pending)
            .map(|m| m.received_at)
            .min();
        status
    }

    pub fn current(&self) -> &BTreeMap<u64, BTreeMap<String, serde_json::Value>> {
        &self.state.current
    }

    /// Snapshot work while admission is locked, then release that lock before
    /// making network calls. Complete only the returned IDs after success.
    pub fn pending(intake: &Intake) -> (Vec<Target>, Vec<String>) {
        (
            intake.pending_targets(),
            intake
                .markers()
                .iter()
                .filter(|(_, m)| m.pending)
                .map(|(id, _)| id.clone())
                .collect(),
        )
    }

    /// Process a previously captured work snapshot. Call once at startup with
    /// `startup` set even when a previous sweep watermark exists.
    pub fn run_snapshot<F: Forge>(
        &mut self,
        targets: Vec<Target>,
        forge: &mut F,
        now: u64,
        startup: bool,
    ) -> io::Result<bool> {
        if self.state.status.retry_at.is_some_and(|at| now < at) {
            return Ok(false);
        }
        let sweep = startup
            || targets.contains(&Target::Repository)
            || self
                .state
                .status
                .last_successful_sweep
                .is_none_or(|at| now.saturating_sub(at) >= self.interval);
        if !sweep && targets.is_empty() {
            return Ok(false);
        }
        let previous = self.state.clone();
        let result = (|| {
            let numbers = if sweep {
                self.sweep_numbers(forge, now)?
            } else {
                targets
                    .into_iter()
                    .filter_map(|t| match t {
                        Target::PullRequest(n) => Some(n),
                        Target::Repository => None,
                    })
                    .collect()
            };
            for number in numbers {
                self.refresh(forge, now, number)?;
            }
            if sweep {
                self.state.status.last_successful_sweep = Some(now);
            }
            self.persist()?;
            Ok(true)
        })();
        if result.is_err() {
            let status = self.state.status.clone();
            self.state = previous;
            self.state.status.requests = status.requests;
            self.state.status.responses_200 = status.responses_200;
            self.state.status.responses_304 = status.responses_304;
            self.state.status.rate_limit = status.rate_limit;
            self.state.status.retry_at = status.retry_at;
            self.persist()?;
        }
        result
    }

    pub fn run<F: Forge>(
        &mut self,
        intake: &mut Intake,
        forge: &mut F,
        now: u64,
        startup: bool,
    ) -> io::Result<bool> {
        let (targets, ids) = Self::pending(intake);
        let worked = self.run_snapshot(targets, forge, now, startup)?;
        if worked {
            intake.complete(&ids)?;
        }
        Ok(worked)
    }

    fn fetch<F: Forge>(
        &mut self,
        forge: &mut F,
        path: &str,
        now: u64,
    ) -> io::Result<serde_json::Value> {
        let etag = self.state.cache.get(path).and_then(|c| c.etag.as_deref());
        self.state.status.requests += 1;
        let response = match forge.get(path, etag) {
            Ok(response) => response,
            Err(error) => {
                self.state.status.retry_at = Some(now.saturating_add(60));
                return Err(error);
            }
        };
        for (key, value) in &response.headers {
            if key.starts_with("x-ratelimit-") {
                self.state
                    .status
                    .rate_limit
                    .insert(key.clone(), value.clone());
            }
        }
        if let Some(delay) = response.headers.get("retry-after").and_then(|s| {
            s.parse::<u64>()
                .ok()
                .map(|delay| now.saturating_add(delay))
                .or_else(|| {
                    let date = s.strip_suffix(" GMT")?;
                    let timestamp = jiff::Timestamp::strptime(
                        "%a, %d %b %Y %H:%M:%S %z",
                        format!("{date} +0000"),
                    )
                    .ok()?;
                    u64::try_from(timestamp.as_second()).ok()
                })
        }) {
            self.state.status.retry_at = Some(delay.max(now));
        } else {
            self.state.status.retry_at = None;
        }
        match response.status {
            200 => {
                self.state.status.responses_200 += 1;
                let body: serde_json::Value =
                    serde_json::from_slice(&response.body).map_err(io::Error::other)?;
                self.state.cache.insert(
                    path.to_owned(),
                    Cached {
                        etag: response.headers.get("etag").cloned(),
                        body: body.clone(),
                    },
                );
                Ok(body)
            }
            304 => {
                self.state.status.responses_304 += 1;
                self.state
                    .cache
                    .get(path)
                    .map(|c| c.body.clone())
                    .ok_or_else(|| io::Error::other("304 without cached body"))
            }
            code => {
                if matches!(code, 403 | 429 | 500..=599) && self.state.status.retry_at.is_none() {
                    let reset = (response
                        .headers
                        .get("x-ratelimit-remaining")
                        .map(String::as_str)
                        == Some("0"))
                    .then(|| {
                        response
                            .headers
                            .get("x-ratelimit-reset")
                            .and_then(|s| s.parse::<u64>().ok())
                    })
                    .flatten();
                    self.state.status.retry_at = Some(
                        reset
                            .filter(|at| *at > now)
                            .unwrap_or_else(|| now.saturating_add(60)),
                    );
                }
                Err(io::Error::other(format!(
                    "forge returned HTTP {code} for {path}"
                )))
            }
        }
    }

    fn pages<F: Forge>(
        &mut self,
        forge: &mut F,
        path: &str,
        now: u64,
    ) -> io::Result<Vec<serde_json::Value>> {
        let mut all = Vec::new();
        for page in 1..=10000 {
            let separator = if path.contains('?') { '&' } else { '?' };
            let url = format!("{path}{separator}per_page=100&page={page}");
            let body = self.fetch(forge, &url, now)?;
            let entries = body
                .as_array()
                .ok_or_else(|| io::Error::other("expected paginated array"))?;
            all.extend(entries.iter().cloned());
            if entries.len() < 100 {
                return Ok(all);
            }
        }
        Err(io::Error::other("pagination limit exceeded"))
    }

    fn sweep_numbers<F: Forge>(&mut self, forge: &mut F, now: u64) -> io::Result<Vec<u64>> {
        let base = format!("repos/{}/pulls", self.repository);
        let mut numbers = Vec::new();
        for pr in self.pages(forge, &format!("{base}?state=open"), now)? {
            numbers.push(
                pr.get("number")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| io::Error::other("pull request missing number"))?,
            );
        }
        if let Some(last) = self.state.status.last_successful_sweep {
            for page in 1..=10000 {
                let path = format!(
                    "{base}?state=closed&sort=updated&direction=desc&per_page=100&page={page}"
                );
                let body = self.fetch(forge, &path, now)?;
                let entries = body
                    .as_array()
                    .ok_or_else(|| io::Error::other("expected paginated array"))?;
                let mut older = false;
                for pr in entries {
                    let updated = pr
                        .get("updated_at")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<jiff::Timestamp>().ok())
                        .and_then(|t| u64::try_from(t.as_second()).ok());
                    if updated.is_some_and(|value| value < last) {
                        older = true;
                        break;
                    }
                    numbers.push(
                        pr.get("number")
                            .and_then(|v| v.as_u64())
                            .ok_or_else(|| io::Error::other("pull request missing number"))?,
                    );
                }
                if older || entries.len() < 100 {
                    return Ok(numbers);
                }
                if page == 10000 {
                    return Err(io::Error::other("pagination limit exceeded"));
                }
            }
        }
        Ok(numbers)
    }

    fn refresh<F: Forge>(&mut self, forge: &mut F, now: u64, number: u64) -> io::Result<()> {
        let base = format!("repos/{}", self.repository);
        let pr = self.fetch(forge, &format!("{base}/pulls/{number}"), now)?;
        let sha = pr
            .pointer("/head/sha")
            .and_then(|v| v.as_str())
            .ok_or_else(|| io::Error::other("pull request missing head SHA"))?
            .to_owned();
        let mut resources = BTreeMap::new();
        resources.insert("pull_request".into(), pr);
        for (name, path) in [
            ("issue_comments", format!("{base}/issues/{number}/comments")),
            ("review_comments", format!("{base}/pulls/{number}/comments")),
            ("reviews", format!("{base}/pulls/{number}/reviews")),
        ] {
            resources.insert(
                name.into(),
                serde_json::Value::Array(self.pages(forge, &path, now)?),
            );
        }
        for (name, endpoint, key) in [
            ("status", "status", "statuses"),
            ("check_runs", "check-runs", "check_runs"),
        ] {
            let mut items = Vec::new();
            for page in 1..=10000 {
                let response = self.fetch(
                    forge,
                    &format!("{base}/commits/{sha}/{endpoint}?per_page=100&page={page}"),
                    now,
                )?;
                let entries = response
                    .get(key)
                    .and_then(|v| v.as_array())
                    .ok_or_else(|| io::Error::other(format!("{endpoint} missing array")))?;
                items.extend(entries.iter().cloned());
                if entries.len() < 100 {
                    break;
                }
                if page == 10000 {
                    return Err(io::Error::other("pagination limit exceeded"));
                }
            }
            resources.insert(name.into(), serde_json::Value::Array(items));
        }
        self.state.current.insert(number, resources);
        Ok(())
    }

    fn persist(&self) -> io::Result<()> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let tmp = self
            .path
            .with_extension(format!("{}.tmp", std::process::id()));
        let result = (|| {
            use io::Write;
            let mut file = fs::File::create(&tmp)?;
            serde_json::to_writer(&mut file, &self.state).map_err(io::Error::other)?;
            file.flush()?;
            file.sync_all()?;
            fs::rename(&tmp, &self.path)?;
            fs::File::open(parent)?.sync_all()
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result
    }
}

#[cfg(test)]
#[allow(dead_code, unused_imports)] // Also compiled by the harness=false worktree driver, without libtest.
mod tests {
    use super::*;
    use crate::event_intake::Admission;
    use crate::event_intake::gh::Response;

    #[derive(Default)]
    struct Fake {
        requests: Vec<(String, Option<String>)>,
        fail: bool,
        merged: bool,
    }
    impl Forge for Fake {
        fn get(&mut self, path: &str, etag: Option<&str>) -> io::Result<Response> {
            self.requests
                .push((path.to_owned(), etag.map(str::to_owned)));
            let mut headers = BTreeMap::new();
            headers.insert("etag".into(), format!("tag-{}", self.merged));
            headers.insert("x-ratelimit-remaining".into(), "4999".into());
            if self.fail {
                return Ok(Response {
                    status: 503,
                    headers,
                    body: vec![],
                });
            }
            if etag == Some(format!("tag-{}", self.merged).as_str()) {
                return Ok(Response {
                    status: 304,
                    headers,
                    body: vec![],
                });
            }
            let body = if path.contains("pulls?state=open") && !self.merged
                || path.contains("pulls?state=closed") && self.merged
            {
                serde_json::json!([{ "number": 12, "state": if self.merged { "closed" } else { "open" }, "updated_at": if self.merged { "1970-01-01T00:10:00Z" } else { "1970-01-01T00:03:20Z" } }])
            } else if path.contains("pulls?state=") {
                serde_json::json!([])
            } else if path.ends_with("/pulls/12") {
                serde_json::json!({ "number": 12, "state": if self.merged { "closed" } else { "open" }, "head": { "sha": "abc" } })
            } else if path.contains("comments?") || path.contains("reviews?") {
                serde_json::json!([])
            } else if path.contains("check-runs?") {
                serde_json::json!({"check_runs": []})
            } else if path.contains("status?") {
                serde_json::json!({"statuses": []})
            } else {
                serde_json::json!({})
            };
            Ok(Response {
                status: 200,
                headers,
                body: serde_json::to_vec(&body).map_err(io::Error::other)?,
            })
        }
    }

    #[test]
    fn later_delivery_is_not_completed_with_earlier_snapshot() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let mut intake = Intake::open(dir.path().join("intake"), "o/r".into(), b"secret".to_vec())?;
        let body = br#"{"repository":{"full_name":"o/r"},"pull_request":{"number":12}}"#;
        let signature = "sha256=db4061419529a8addfd7db7f148ff8677579ec4723d46ba890f9c7c80f8629ab";
        assert_eq!(
            intake.admit("first", "pull_request", Some(signature), body, 5)?,
            Admission::Accepted
        );
        let (targets, ids) = Reconciler::pending(&intake);
        assert_eq!(
            intake.admit("later", "pull_request", Some(signature), body, 6)?,
            Admission::Accepted
        );
        let mut reconciler = Reconciler::open(dir.path().join("state"), "o/r".into(), 300)?;
        let mut forge = Fake::default();
        assert!(reconciler.run_snapshot(targets, &mut forge, 7, false)?);
        intake.complete_at(&ids, Some(8))?;
        assert!(!intake.markers()["first"].pending);
        assert!(intake.markers()["later"].pending);
        Ok(())
    }

    #[test]
    fn polling_sweep_recovers_merge_and_persists_etags_and_status() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let mut intake = Intake::open(dir.path().join("intake"), "o/r".into(), Vec::new())?;
        let path = dir.path().join("reconcile");
        let mut reconciler = Reconciler::open(&path, "o/r".into(), 300)?;
        let mut forge = Fake::default();
        assert!(reconciler.run(&mut intake, &mut forge, 100, true)?);
        assert_eq!(reconciler.current()[&12]["pull_request"]["state"], "open");
        drop(reconciler);
        let mut reconciler = Reconciler::open(&path, "o/r".into(), 300)?;
        assert!(!reconciler.run(&mut intake, &mut forge, 101, false)?);
        assert!(reconciler.run(&mut intake, &mut forge, 400, false)?);
        assert!(forge.requests.iter().any(|(_, etag)| etag.is_some()));
        let status = reconciler.status(&intake);
        assert_eq!(status.last_successful_sweep, Some(400));
        assert!(status.responses_304 > 0);
        assert!(status.ratio_304_to_200.is_some());
        assert_eq!(status.requests, status.responses_200 + status.responses_304);
        assert_eq!(
            status
                .rate_limit
                .get("x-ratelimit-remaining")
                .map(String::as_str),
            Some("4999")
        );
        forge.merged = true;
        assert!(reconciler.run(&mut intake, &mut forge, 700, false)?);
        assert_eq!(reconciler.current()[&12]["pull_request"]["state"], "closed");
        Ok(())
    }

    #[test]
    fn unmappable_check_runs_sweep_and_failure_keeps_marker() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let mut intake = Intake::open(dir.path().join("intake"), "o/r".into(), b"secret".to_vec())?;
        let mut reconciler = Reconciler::open(dir.path().join("reconcile"), "o/r".into(), 300)?;
        let mut forge = Fake::default();
        reconciler.run(&mut intake, &mut forge, 100, true)?;
        let body = br#"{"repository":{"full_name":"o/r"},"check_run":{"pull_requests":[]}}"#;
        let sig = "sha256=ca86aaa421aac4e350cee5222e2817b7cf0aa5e3f62d1c9fc0de49e6ea22daf6";
        assert_eq!(
            intake.admit("check", "check_run", Some(sig), body, 110)?,
            Admission::Accepted
        );
        forge.fail = true;
        assert!(reconciler.run(&mut intake, &mut forge, 111, false).is_err());
        assert_eq!(reconciler.status(&intake).oldest_pending_marker, Some(110));
        assert_eq!(reconciler.status(&intake).last_successful_sweep, Some(100));
        forge.fail = false;
        forge.requests.clear();
        assert!(reconciler.run(&mut intake, &mut forge, 172, false)?);
        assert!(forge.requests[0].0.contains("pulls?state=open"));
        assert_eq!(reconciler.status(&intake).oldest_pending_marker, None);
        Ok(())
    }
}
