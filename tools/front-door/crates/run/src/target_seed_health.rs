//! Advisory health of a project's host Cargo target seed.
//!
//! Seed refresh failures and stale generations are facts to show, not reasons
//! to refuse a lane. The store records the last refresh outcome beside the
//! generations; a reader joins that record with the newest complete generation
//! to render warnings. All reads tolerate missing, corrupt or future-schema
//! files and return `None` rather than failing the caller.

use provenant_core::WallTime;
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs, io,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

/// Schema of the persisted refresh record. A later schema is ignored by this
/// reader rather than being mistaken for a current outcome.
pub const REFRESH_SCHEMA: u32 = 1;
/// Bound on a stored reason, in bytes, so a failed build's output tail cannot
/// grow the record without limit.
pub const MAX_REASON_BYTES: usize = 2 * 1024;
/// Fallback when the configured duration cannot be parsed.
pub const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_REFRESH_READ_BYTES: u64 = 32 * 1024;

/// The last recorded attempt to build or reuse this project's seed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshRecord {
    pub schema: u32,
    pub at: WallTime,
    /// One of `built`, `current`, `busy`, `failed`, `prepare_failed`, `skipped`.
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The newest complete generation that belongs to the requested project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationAge {
    pub id: String,
    /// Modified time of the generation's `complete` marker.
    pub built_at: WallTime,
}

/// The seed facts a lane start or doctor view can display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeedHealth {
    pub newest: Option<GenerationAge>,
    pub age: Option<Duration>,
    pub last_refresh: Option<RefreshRecord>,
    pub stale_after: Duration,
    pub stale_after_source: String,
}

impl SeedHealth {
    /// Warnings to show, each naming the last outcome, the effective knob and
    /// the single repair a person can make. Never refuses a lane.
    pub fn warnings(&self) -> Vec<String> {
        let outcome = self.last_refresh.as_ref().map_or_else(
            || "no refresh recorded".to_owned(),
            |record| match &record.reason {
                Some(reason) if !reason.is_empty() => {
                    format!("{} ({})", record.outcome, one_line(reason, 160))
                }
                _ => record.outcome.clone(),
            },
        );
        let knob = format!(
            "build.seed.stale_after = {} (source: {})",
            format_duration(self.stale_after),
            self.stale_after_source
        );
        let repair = "run provenant worktree seed-build in this repository";
        let mut warnings = Vec::new();
        match &self.newest {
            None => warnings.push(format!(
                "no complete Cargo target seed generation for this repository; last refresh outcome: {outcome}; {knob}; {repair}"
            )),
            Some(generation) => {
                if self
                    .age
                    .is_some_and(|age| age > self.stale_after)
                {
                    warnings.push(format!(
                        "newest Cargo target seed generation {} is {} old; last refresh outcome: {outcome}; {knob}; {repair}",
                        generation.id,
                        self.age.map_or_else(|| "unknown".to_owned(), format_duration)
                    ));
                }
            }
        }
        if self
            .last_refresh
            .as_ref()
            .is_some_and(|record| matches!(record.outcome.as_str(), "failed" | "prepare_failed"))
        {
            warnings.push(format!(
                "last Cargo target seed refresh outcome: {outcome}; {knob}; {repair}"
            ));
        }
        warnings
    }
}

/// Effective staleness setting from a resolved port table. A malformed value
/// warns and falls back to 24h from the default; this never fails a lane.
pub fn stale_after_policy(
    table: &provenant_config::PortTable,
) -> (Duration, String, Option<String>) {
    match table.build.seed.stale_after_duration() {
        Ok(duration) => (
            duration,
            provenant_config::port_source_layer(table.build.source).to_owned(),
            None,
        ),
        Err(error) => (
            DEFAULT_STALE_AFTER,
            "default".to_owned(),
            Some(format!(
                "build.seed.stale_after could not be read ({error}); using 24h from default"
            )),
        ),
    }
}

/// Human form of a duration setting or a generation age.
pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds == 0 {
        let millis = duration.subsec_millis();
        return if millis == 0 {
            "0s".to_owned()
        } else {
            format!("{millis}ms")
        };
    }
    if duration.subsec_micros() != 0 {
        return format!("{}ms", duration.as_millis());
    }
    for (unit, seconds_per) in [("d", 86_400), ("h", 3_600), ("m", 60)] {
        if seconds.is_multiple_of(seconds_per) {
            return format!("{}{unit}", seconds / seconds_per);
        }
    }
    format!("{seconds}s")
}

/// Record a refresh outcome at `<state>/cargo-target-seeds/refresh/<key>.json`.
/// The file is published by writing a sibling temporary file and renaming it,
/// so a reader never sees a partial record.
pub fn record_refresh(
    state: &Path,
    project_key: &str,
    outcome: &str,
    reason: Option<&str>,
    at: WallTime,
) -> io::Result<()> {
    let path = refresh_path(state, project_key)?;
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::other("refresh record has no parent"))?;
    fs::create_dir_all(directory)?;
    let record = RefreshRecord {
        schema: REFRESH_SCHEMA,
        at,
        outcome: outcome.to_owned(),
        reason: reason.map(truncate_reason),
    };
    let bytes = serde_json::to_vec(&record).map_err(io::Error::other)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("refresh record has no file name"))?;
    for attempt in 0..16 {
        let temporary = directory.join(format!(
            ".{}.tmp-{}-{attempt}",
            name.to_string_lossy(),
            std::process::id()
        ));
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let published = (|| {
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, &path)
        })();
        if let Err(error) = published {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        return Ok(());
    }
    Err(io::Error::other(
        "could not reserve a temporary refresh record",
    ))
}

/// Read the last refresh outcome. Missing, corrupt, oversized and newer-schema
/// files are all reported as absent.
pub fn read_refresh(state: &Path, project_key: &str) -> Option<RefreshRecord> {
    let path = refresh_path(state, project_key).ok()?;
    let file = fs::File::open(path).ok()?;
    let mut bounded = file.take(MAX_REFRESH_READ_BYTES + 1);
    let mut bytes = Vec::new();
    bounded.read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_REFRESH_READ_BYTES {
        return None;
    }
    let record: RefreshRecord = serde_json::from_slice(&bytes).ok()?;
    (record.schema == REFRESH_SCHEMA).then_some(record)
}

/// Find the newest complete generation for `project_key`, using the same
/// completeness test as the seed builder: a read-only directory carrying a
/// `complete` marker and a manifest that names the project.
pub fn newest_generation(state: &Path, project_key: &str) -> Option<GenerationAge> {
    let generations = crate::target_seed::store_path(state).join("generations");
    let entries = fs::read_dir(generations).ok()?;
    let mut newest: Option<(WallTime, String)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry.file_type().is_ok_and(|kind| kind.is_dir())
            || entry.file_name().to_string_lossy().starts_with('.')
        {
            continue;
        }
        let marker = path.join("complete");
        if !fs::read(&marker).is_ok_and(|data| data == b"complete\n") {
            continue;
        }
        if !read_only(&path) {
            continue;
        }
        let Some(manifest_bytes) = fs::read(path.join("manifest.json")).ok() else {
            continue;
        };
        let Ok(manifest) = serde_json::from_slice::<crate::target_seed::Manifest>(&manifest_bytes)
        else {
            continue;
        };
        if manifest.project_key != project_key {
            continue;
        }
        let Some(modified) = fs::metadata(&marker)
            .and_then(|metadata| metadata.modified())
            .ok()
        else {
            continue;
        };
        let Ok(since_epoch) = modified.duration_since(std::time::UNIX_EPOCH) else {
            continue;
        };
        let Ok(micros) = i64::try_from(since_epoch.as_micros()) else {
            continue;
        };
        let Ok(built_at) = WallTime::from_micros(micros) else {
            continue;
        };
        if newest
            .as_ref()
            .is_none_or(|(current, _)| built_at > *current)
        {
            newest = Some((built_at, entry.file_name().to_string_lossy().into_owned()));
        }
    }
    newest.map(|(built_at, id)| GenerationAge { id, built_at })
}

/// Join the store facts with the staleness policy at the clock `now`.
pub fn seed_health(
    state: &Path,
    project_key: &str,
    stale_after: Duration,
    stale_after_source: &str,
    now: WallTime,
) -> SeedHealth {
    let newest = newest_generation(state, project_key);
    let age = newest.as_ref().and_then(|generation| {
        now.as_micros()
            .checked_sub(generation.built_at.as_micros())
            .and_then(|micros| u64::try_from(micros).ok())
            .map(Duration::from_micros)
    });
    SeedHealth {
        newest,
        age,
        last_refresh: read_refresh(state, project_key),
        stale_after,
        stale_after_source: stale_after_source.to_owned(),
    }
}

#[cfg(unix)]
fn read_only(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|metadata| metadata.permissions().mode() & 0o222 == 0)
}

#[cfg(not(unix))]
fn read_only(_path: &Path) -> bool {
    // Generation publication freezes directories on Unix; elsewhere the
    // complete marker alone establishes completeness.
    true
}

fn refresh_path(state: &Path, project_key: &str) -> io::Result<PathBuf> {
    if project_key.is_empty()
        || !project_key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid seed project key",
        ));
    }
    Ok(crate::target_seed::store_path(state)
        .join("refresh")
        .join(format!("{project_key}.json")))
}

fn truncate_reason(reason: &str) -> String {
    if reason.len() <= MAX_REASON_BYTES {
        return reason.to_owned();
    }
    let mut end = MAX_REASON_BYTES;
    while end > 0 && !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason[..end].to_owned()
}

fn one_line(text: &str, limit: usize) -> String {
    let single = text.replace(['\n', '\r'], " ");
    if single.chars().count() <= limit {
        return single;
    }
    let cut = single
        .char_indices()
        .nth(limit)
        .map_or(single.len(), |(index, _)| index);
    format!("{}...", &single[..cut])
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_core::WallTime;

    const NOW_MICROS: i64 = 1_800_000_000_000_000; // 2027-01-15T08:00:00Z

    fn now() -> WallTime {
        WallTime::from_micros(NOW_MICROS).unwrap()
    }

    fn at(offset: Duration) -> WallTime {
        WallTime::from_micros(NOW_MICROS - i64::try_from(offset.as_micros()).unwrap()).unwrap()
    }

    fn set_mtime(path: &Path, when: WallTime) {
        let modified =
            std::time::UNIX_EPOCH + Duration::from_micros(u64::try_from(when.as_micros()).unwrap());
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }

    fn generation(state: &Path, key: &str, id: &str, when: WallTime, complete: bool) -> PathBuf {
        let directory = crate::target_seed::store_path(state)
            .join("generations")
            .join(id);
        fs::create_dir_all(&directory).unwrap();
        let manifest = crate::target_seed::Manifest {
            project_key: key.into(),
            rustc_vv: "rustc 1.100.0".into(),
            host_triple: "aarch64-apple-darwin".into(),
            commit: "abc".into(),
            profile: "default".into(),
            cargo_profile: provenant_config::CargoBuildProfile {
                incremental: "1".into(),
                debug: "1".into(),
            },
            features: Vec::new(),
            flags: Vec::new(),
            allocated_bytes: None,
        };
        fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        if complete {
            let marker = directory.join("complete");
            fs::write(&marker, b"complete\n").unwrap();
            set_mtime(&marker, when);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).unwrap();
            }
        }
        directory
    }

    fn health(state: &Path, key: &str, stale_after: Duration) -> SeedHealth {
        seed_health(state, key, stale_after, "project", now())
    }

    #[test]
    fn health_warns_when_newest_generation_older_than_stale_after() {
        let state = tempfile::tempdir().unwrap();
        let key = "project-a";
        generation(
            state.path(),
            key,
            "002",
            at(Duration::from_secs(48 * 3600)),
            true,
        );
        record_refresh(
            state.path(),
            key,
            "built",
            None,
            at(Duration::from_secs(48 * 3600)),
        )
        .unwrap();
        let warnings = health(state.path(), key, Duration::from_secs(24 * 3600)).warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("002"), "{warnings:?}");
        assert!(warnings[0].contains("stale"), "{warnings:?}");
        assert!(warnings[0].contains("built"), "{warnings:?}");
        assert!(
            warnings[0].contains("build.seed.stale_after"),
            "{warnings:?}"
        );
        assert!(warnings[0].contains("24h"), "{warnings:?}");
        assert!(warnings[0].contains("source: project"), "{warnings:?}");
        assert!(
            warnings[0].contains("provenant worktree seed-build"),
            "{warnings:?}"
        );
    }

    #[test]
    fn health_warns_when_last_refresh_failed_and_names_outcome_and_knob() {
        let state = tempfile::tempdir().unwrap();
        let key = "project-a";
        generation(state.path(), key, "003", at(Duration::from_secs(60)), true);
        record_refresh(
            state.path(),
            key,
            "prepare_failed",
            Some("E0583: missing generated module"),
            at(Duration::from_secs(60)),
        )
        .unwrap();
        let warnings = health(state.path(), key, Duration::from_secs(24 * 3600)).warnings();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("prepare_failed"), "{warnings:?}");
        assert!(
            warnings[0].contains("missing generated module"),
            "{warnings:?}"
        );
        assert!(
            warnings[0].contains("build.seed.stale_after"),
            "{warnings:?}"
        );
        assert!(warnings[0].contains("24h"), "{warnings:?}");
        assert!(
            warnings[0].contains("provenant worktree seed-build"),
            "{warnings:?}"
        );
        record_refresh(
            state.path(),
            key,
            "failed",
            None,
            at(Duration::from_secs(60)),
        )
        .unwrap();
        let warnings = health(state.path(), key, Duration::from_secs(24 * 3600)).warnings();
        assert!(warnings.iter().any(|warning| warning.contains("failed")));
    }

    #[test]
    fn health_silent_when_fresh_and_last_refresh_ok() {
        let state = tempfile::tempdir().unwrap();
        let key = "project-a";
        generation(state.path(), key, "004", at(Duration::from_secs(60)), true);
        record_refresh(
            state.path(),
            key,
            "built",
            None,
            at(Duration::from_secs(60)),
        )
        .unwrap();
        let health = health(state.path(), key, Duration::from_secs(24 * 3600));
        assert!(health.age.is_some_and(|age| age == Duration::from_secs(60)));
        assert!(health.warnings().is_empty(), "{:?}", health.warnings());
        record_refresh(state.path(), key, "current", None, now()).unwrap();
        assert!(
            health(state.path(), key, Duration::from_secs(24 * 3600))
                .warnings()
                .is_empty()
        );
    }

    #[test]
    fn health_warns_when_no_generation_and_names_last_outcome() {
        let state = tempfile::tempdir().unwrap();
        let key = "project-a";
        record_refresh(
            state.path(),
            key,
            "failed",
            None,
            at(Duration::from_secs(60)),
        )
        .unwrap();
        let warnings = health(state.path(), key, Duration::from_secs(24 * 3600)).warnings();
        assert!(
            warnings.iter().any(|warning| {
                warning.contains("no complete Cargo target seed generation")
                    && warning.contains("failed")
                    && warning.contains("build.seed.stale_after")
            }),
            "{warnings:?}"
        );
    }

    #[test]
    fn refresh_record_round_trip_and_corrupt_file_is_none() {
        let state = tempfile::tempdir().unwrap();
        let key = "project-a";
        let reason = "build output tail";
        record_refresh(state.path(), key, "failed", Some(reason), now()).unwrap();
        let record = read_refresh(state.path(), key).unwrap();
        assert_eq!(record.schema, REFRESH_SCHEMA);
        assert_eq!(record.at, now());
        assert_eq!(record.outcome, "failed");
        assert_eq!(record.reason.as_deref(), Some(reason));
        assert!(read_refresh(state.path(), "missing").is_none());
        let path = refresh_path(state.path(), key).unwrap();
        fs::write(&path, b"{not json").unwrap();
        assert!(read_refresh(state.path(), key).is_none());
        let mut newer = serde_json::to_value(&record).unwrap();
        newer["schema"] = serde_json::json!(REFRESH_SCHEMA + 1);
        fs::write(&path, serde_json::to_vec(&newer).unwrap()).unwrap();
        assert!(read_refresh(state.path(), key).is_none());
        record_refresh(state.path(), key, "failed", Some(&"x".repeat(4096)), now()).unwrap();
        let record = read_refresh(state.path(), key).unwrap();
        assert_eq!(
            record.reason.map(|reason| reason.len()),
            Some(MAX_REASON_BYTES)
        );
        assert!(
            record
                .reason
                .is_some_and(|reason| reason.is_char_boundary(reason.len()))
        );
    }

    #[test]
    fn newest_generation_ignores_incomplete_and_other_projects() {
        let state = tempfile::tempdir().unwrap();
        generation(state.path(), "other", "099", now(), true);
        generation(
            state.path(),
            "project-a",
            "100",
            at(Duration::from_secs(10 * 3600)),
            false,
        );
        let newest = generation(
            state.path(),
            "project-a",
            "101",
            at(Duration::from_secs(2 * 3600)),
            true,
        );
        let found = newest_generation(state.path(), "project-a").unwrap();
        assert_eq!(found.id, "101");
        assert_eq!(found.built_at, at(Duration::from_secs(2 * 3600)));
        // A newer complete marker for another project cannot change this one's
        // answer, and an incomplete generation is never selected.
        generation(state.path(), "other", "102", now(), true);
        assert_eq!(
            newest_generation(state.path(), "project-a").unwrap().id,
            "101"
        );
        fs::write(newest.join("complete"), b"building\n").unwrap();
        assert_eq!(
            newest_generation(state.path(), "project-a").unwrap().id,
            "100"
        );
    }
}
