use super::*;
use provenant_core::{PathResolver, RootInputs};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
struct Resolver;
impl PathResolver for Resolver {
    fn resolve_existing_ancestor(&self, path: &Path) -> std::io::Result<PathBuf> {
        Ok(path.to_owned())
    }
}

fn message(id: &str, at: &str, input: u64, creation: u64, one_hour: u64, output: u64) -> String {
    format!(
        "{{\"type\":\"assistant\",\"timestamp\":\"{at}\",\"message\":{{\"id\":\"{id}\",\"model\":\"claude-opus\",\"content\":\"private words\",\"usage\":{{\"input_tokens\":{input},\"cache_read_input_tokens\":7,\"cache_creation_input_tokens\":{creation},\"cache_creation\":{{\"ephemeral_1h_input_tokens\":{one_hour}}},\"output_tokens\":{output}}}}}}}\n"
    )
}

struct Fixture {
    _temp: tempfile::TempDir,
    base: PathBuf,
    roots: Roots,
}
impl Fixture {
    fn new() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let base = std::fs::canonicalize(temp.path())?;
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        let roots = inputs.resolve(&Resolver)?;
        provenant_os::fixture::private_state_dir(roots.state.as_path())?;
        Ok(Self {
            _temp: temp,
            base,
            roots,
        })
    }
    fn dir(&self, name: &str) -> PathBuf {
        self.base.join(name)
    }
    fn write(&self, dir: &str, relative: &str, text: &str) -> Result {
        let path = self.dir(dir).join(relative);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, text)?;
        Ok(())
    }
    fn run(&self, dirs: &[(&str, &str)], chairs: &[&str], since: Option<&str>) -> Result<Report> {
        let dirs: Vec<_> = dirs
            .iter()
            .map(|(account, dir)| ConfigDir {
                account: (*account).into(),
                path: self.dir(dir),
            })
            .collect();
        let chairs = chairs.iter().map(|s| (*s).to_owned()).collect();
        Ok(ingest(
            &self.roots,
            &Request {
                dirs: &dirs,
                chair_sessions: &chairs,
                provider: None,
                since: since.map(str::parse).transpose()?,
                deadline: None,
                incremental: false,
            },
        )?)
    }
}

const DIRS: [(&str, &str); 3] = [
    ("anese", "claude-anese"),
    ("pro", "claude-pro"),
    ("unattributed", "claude"),
];

fn populate(f: &Fixture) -> Result {
    // A streamed message repeats with growing counts; the largest is final.
    f.write(
        "claude-anese",
        "projects/p/s1.jsonl",
        &(message("m1", "2026-10-01T00:00:00Z", 10, 100, 40, 1)
            + &message("m1", "2026-10-01T00:00:01Z", 10, 100, 40, 5)
            + &message("m2", "2026-10-01T00:01:00Z", 20, 0, 0, 6)
            + "{\"type\":\"user\",\"message\":{\"content\":\"private words\"}}\n"),
    )?;
    f.write(
        "claude-anese",
        "projects/p/s1/subagents/agent-a.jsonl",
        &message("m3", "2026-10-01T00:02:00Z", 1, 2, 0, 3),
    )?;
    f.write(
        "claude-pro",
        "projects/p/s2.jsonl",
        &message("m4", "2026-10-02T00:00:00Z", 5, 0, 0, 5),
    )?;
    f.write(
        "claude",
        "projects/p/s3.jsonl",
        &message("m5", "2026-10-03T00:00:00Z", 9, 0, 0, 9),
    )?;
    Ok(())
}

/// Every segment's text, for checks on what is stored.
fn stored(f: &Fixture) -> Result<String> {
    let mut text = String::new();
    for name in crate::segments::list(&Directory::open_anchor(&f.base.join("state/ingest"))?)? {
        text.push_str(&std::fs::read_to_string(
            f.base.join("state/ingest").join(name),
        )?);
    }
    Ok(text)
}

fn segment_files(f: &Fixture) -> Result<Vec<String>> {
    Ok(crate::segments::list(&Directory::open_anchor(
        &f.base.join("state/ingest"),
    )?)?)
}

#[test]
fn a_message_seen_on_both_sides_of_midnight_is_counted_once() -> Result {
    let f = Fixture::new()?;
    f.write(
        "claude",
        "projects/p/s1.jsonl",
        &message("m1", "2026-10-01T23:59:59Z", 5, 0, 0, 1),
    )?;
    f.run(&DIRS, &[], None)?;
    // A later run reads only the stream's tail, which is stamped the next day.
    f.write(
        "claude",
        "projects/p/s1.jsonl",
        &message("m1", "2026-10-02T00:00:01Z", 5, 0, 0, 9),
    )?;
    let report = f.run(&DIRS, &[], None)?;
    assert_eq!((report.added, report.already_present), (0, 1));
    let rows = read_rows(&f.roots, &RowFilter::default())?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].output, 9);
    assert_eq!(segment_files(&f)?, ["2026-10-01.jsonl"]);
    Ok(())
}

#[test]
fn ingest_removes_staging_files_a_crash_left_in_the_store() -> Result {
    let f = Fixture::new()?;
    f.write(
        "claude",
        "projects/p/s1.jsonl",
        &message("m1", "2026-10-01T00:00:00Z", 1, 0, 0, 1),
    )?;
    f.run(&DIRS, &[], None)?;
    let stale = f
        .base
        .join("state/ingest/.ingest-.provenant-staging.abc.tmp");
    std::fs::write(&stale, b"partial")?;
    f.run(&DIRS, &[], None)?;
    assert!(!stale.exists());
    Ok(())
}

#[test]
fn effort_is_kept_only_when_it_is_a_short_identifier() -> Result {
    let f = Fixture::new()?;
    f.write(
        "claude",
        "projects/p/s1.jsonl",
        concat!(
            "{\"type\":\"assistant\",\"timestamp\":\"2026-10-01T00:00:00Z\",\"effort\":\"high\",\"message\":{\"id\":\"m1\",\"model\":\"m\",\"usage\":{\"input_tokens\":1}}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"2026-10-01T00:00:01Z\",\"message\":{\"id\":\"m2\",\"model\":\"m\",\"effort\":\"PRIVATE PROMPT\",\"usage\":{\"input_tokens\":1}}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"2026-10-01T00:00:02Z\",\"message\":{\"id\":\"m3\",\"model\":\"m\",\"usage\":{\"input_tokens\":1}}}\n"
        ),
    )?;
    let report = f.run(&DIRS, &[], None)?;
    assert_eq!(report.added, 3);
    let rows = read_rows(&f.roots, &RowFilter::default())?;
    let effort = |id: &str| {
        rows.iter()
            .find(|row| row.message == id)
            .unwrap()
            .effort
            .clone()
    };
    assert_eq!(effort("m1").as_deref(), Some("high"));
    assert_eq!(effort("m2"), None);
    assert_eq!(effort("m3"), None);
    Ok(())
}

#[test]
fn rerun_adds_nothing_and_accounts_stay_distinct() -> Result {
    let f = Fixture::new()?;
    populate(&f)?;
    let first = f.run(&DIRS, &["s1"], None)?;
    assert_eq!((first.added, first.already_present), (5, 0));
    let again = f.run(&DIRS, &["s1"], None)?;
    assert_eq!((again.added, again.already_present), (0, 5));
    let rows = read_rows(&f.roots, &RowFilter::default())?;
    let account = |label: &str| rows.iter().filter(|r| r.account == label).count();
    assert_eq!(
        (account("anese"), account("pro"), account("unattributed")),
        (3, 1, 1)
    );
    // Rows add up to the message usage, each message once at its largest counts.
    let m1 = rows.iter().find(|r| r.message == "m1").unwrap();
    assert_eq!(m1.buckets(), [10, 7, 60, 40, 5]);
    assert_eq!(
        rows.iter().map(|r| r.output).sum::<u64>(),
        5 + 6 + 3 + 5 + 9
    );
    let source = |id: &str| rows.iter().find(|r| r.message == id).unwrap().source;
    assert_eq!(source("m1"), Source::Chair);
    assert_eq!(source("m3"), Source::Subagent);
    assert_eq!(source("m4"), Source::Lane);
    assert!(!stored(&f)?.contains("private"));
    Ok(())
}

#[test]
fn one_account_on_two_directories_counts_once() -> Result {
    let f = Fixture::new()?;
    populate(&f)?;
    // The same session is visible under a second directory of the account.
    f.write(
        "claude-anese2",
        "projects/p/s1.jsonl",
        &message("m1", "2026-10-01T00:00:00Z", 10, 100, 40, 5),
    )?;
    let dirs = [
        ("anese", "claude-anese"),
        ("anese", "claude-anese2"),
        ("anese", "claude-anese/../claude-anese"),
    ];
    let report = f.run(&dirs, &[], None)?;
    assert_eq!(report.directories, 2);
    assert_eq!(report.added, 3);
    Ok(())
}

#[test]
fn streaming_updates_replace_the_existing_message() -> Result {
    let f = Fixture::new()?;
    let at = "2026-10-01T00:00:00Z";
    f.write(
        "claude",
        "projects/p/s1.jsonl",
        &message("m1", at, 10, 0, 0, 1),
    )?;
    assert_eq!(f.run(&DIRS, &[], None)?.added, 1);
    f.write(
        "claude",
        "projects/p/s1.jsonl",
        &(message("m1", at, 10, 0, 0, 1) + &message("m1", at, 10, 0, 0, 9)),
    )?;
    let report = f.run(&DIRS, &[], None)?;
    assert_eq!((report.added, report.already_present), (0, 1));
    let rows = read_rows(&f.roots, &RowFilter::default())?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].output, 9);
    Ok(())
}

#[test]
fn since_and_provider_narrow_the_import() -> Result {
    let f = Fixture::new()?;
    populate(&f)?;
    let report = f.run(&DIRS, &[], Some("2026-10-02T00:00:00Z"))?;
    assert_eq!(report.added, 2);
    let none = ingest(
        &f.roots,
        &Request {
            dirs: &[],
            chair_sessions: &BTreeSet::new(),
            provider: Some("codex"),
            since: None,
            deadline: None,
            incremental: false,
        },
    )?;
    assert_eq!(none.added, 0);
    Ok(())
}

#[test]
fn links_and_missing_timestamps_are_not_followed_or_guessed() -> Result {
    let f = Fixture::new()?;
    f.write(
        "claude",
        "projects/p/s1.jsonl",
        "{\"type\":\"assistant\",\"message\":{\"id\":\"x\",\"model\":\"m\",\"usage\":{\"input_tokens\":1}}}\n",
    )?;
    f.write(
        "outside",
        "s9.jsonl",
        &message("o", "2026-10-01T00:00:00Z", 1, 0, 0, 1),
    )?;
    std::os::unix::fs::symlink(
        f.dir("outside/s9.jsonl"),
        f.dir("claude/projects/p/s9.jsonl"),
    )?;
    let report = f.run(&DIRS, &[], None)?;
    assert_eq!((report.added, report.skipped_no_timestamp), (0, 1));
    Ok(())
}

#[test]
fn linked_projects_and_session_directories_are_not_followed() -> Result {
    let f = Fixture::new()?;
    f.write(
        "outside",
        "p/s1.jsonl",
        &message("m1", "2026-10-01T00:00:00Z", 1, 0, 0, 1),
    )?;
    std::fs::create_dir_all(f.dir("linked"))?;
    std::os::unix::fs::symlink(f.dir("outside"), f.dir("linked/projects"))?;
    f.write(
        "outside",
        "subagents/agent-a.jsonl",
        &message("m2", "2026-10-01T00:00:00Z", 1, 0, 0, 1),
    )?;
    std::fs::create_dir_all(f.dir("regular/projects/p"))?;
    std::os::unix::fs::symlink(f.dir("outside"), f.dir("regular/projects/p/s2"))?;
    let report = f.run(&[("a", "linked"), ("a", "regular")], &[], None)?;
    assert_eq!(report.added, 0);
    Ok(())
}

#[test]
fn registered_directories_get_their_label_and_the_rest_are_unattributed() -> Result {
    let f = Fixture::new()?;
    populate(&f)?;
    let home = f.base.clone();
    std::fs::create_dir_all(home.join(".claude/projects"))?;
    std::fs::create_dir_all(home.join(".claude-pro/projects"))?;
    std::fs::create_dir_all(home.join(".claude-extra/projects"))?;
    std::fs::create_dir_all(home.join(".claude-empty"))?;
    std::fs::write(home.join(".claude.json"), "{}")?;
    let accounts = vec![("pro".to_owned(), vec![home.join(".claude-pro")])];
    let dirs = config_dirs(&accounts, &home);
    let labels: Vec<_> = dirs
        .iter()
        .map(|d| {
            (
                d.account.as_str(),
                d.path.file_name().unwrap().to_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        labels,
        [
            ("pro", ".claude-pro"),
            ("unattributed", ".claude"),
            ("unattributed", ".claude-extra")
        ]
    );
    Ok(())
}

#[test]
fn lines_this_build_cannot_read_survive_a_rewrite() -> Result {
    let f = Fixture::new()?;
    f.write(
        "claude",
        "projects/p/s1.jsonl",
        &message("m1", "2026-10-01T00:00:00Z", 10, 0, 0, 1),
    )?;
    f.run(&DIRS, &[], None)?;
    let day = f.base.join("state/ingest/2026-10-01.jsonl");
    // A row another schema version wrote, then a torn line from a crash.
    let current = std::fs::read_to_string(&day)?;
    let foreign = current.replacen("{\"schema_version\":2,", "{\"schema_version\":3,", 1);
    let torn = "{\"schema_version\":2,\"account\":\"half";
    std::fs::write(&day, format!("{foreign}{torn}\n"))?;
    f.write(
        "claude",
        "projects/p/s2.jsonl",
        &message("m2", "2026-10-01T00:01:00Z", 10, 0, 0, 2),
    )?;
    let report = f.run(&DIRS, &[], None)?;
    assert_eq!(report.added, 2);
    let text = std::fs::read_to_string(&day)?;
    assert!(
        text.lines()
            .any(|line| line == foreign.trim_end_matches('\n'))
    );
    assert!(text.lines().any(|line| line == torn));
    assert_eq!(read_rows(&f.roots, &RowFilter::default())?.len(), 2);
    Ok(())
}

#[test]
fn a_one_hour_cache_split_without_a_total_is_still_counted() -> Result {
    let f = Fixture::new()?;
    f.write(
        "claude",
        "projects/p/s1.jsonl",
        "{\"type\":\"assistant\",\"timestamp\":\"2026-10-01T00:00:00Z\",\"message\":{\"id\":\"m1\",\"model\":\"m\",\"usage\":{\"input_tokens\":1,\"cache_creation\":{\"ephemeral_1h_input_tokens\":40}}}}\n",
    )?;
    f.run(&DIRS, &[], None)?;
    let rows = read_rows(&f.roots, &RowFilter::default())?;
    assert_eq!(rows[0].buckets(), [1, 0, 0, 40, 0]);
    Ok(())
}

#[test]
fn a_lane_row_is_upgraded_to_chair_when_the_hook_records_it() -> Result {
    let f = Fixture::new()?;
    f.write(
        "claude",
        "projects/p/s1.jsonl",
        &message("m1", "2026-10-01T00:00:00Z", 10, 0, 0, 1),
    )?;
    assert_eq!(f.run(&DIRS, &[], None)?.added, 1);
    assert_eq!(
        read_rows(&f.roots, &RowFilter::default())?[0].source,
        Source::Lane
    );
    // The same session is now recorded by the chair hook: the row is upgraded
    // in place, which counts as a change, and nothing is added.
    let report = f.run(&DIRS, &["s1"], None)?;
    assert_eq!((report.added, report.already_present), (0, 1));
    let rows = read_rows(&f.roots, &RowFilter::default())?;
    assert_eq!((rows.len(), rows[0].source), (1, Source::Chair));
    Ok(())
}

#[test]
fn one_message_in_two_session_files_counts_twice() -> Result {
    let f = Fixture::new()?;
    let at = "2026-10-01T00:00:00Z";
    f.write(
        "claude",
        "projects/p/s1.jsonl",
        &message("m1", at, 10, 0, 0, 1),
    )?;
    f.write(
        "claude",
        "projects/p/s2.jsonl",
        &message("m1", at, 10, 0, 0, 1),
    )?;
    // Rows are keyed by session and message, so a copy of a message in another
    // session file is a second row. Whether resumed sessions copy messages is
    // not confirmed, so the key is left as it is.
    let report = f.run(&DIRS, &[], None)?;
    assert_eq!(report.added, 2);
    Ok(())
}

#[test]
#[allow(clippy::disallowed_methods)]
fn an_expired_deadline_reads_nothing_and_keeps_the_store() -> Result {
    let f = Fixture::new()?;
    populate(&f)?;
    f.run(&DIRS, &[], None)?;
    let store = f.base.join("state/ingest/2026-10-01.jsonl");
    let before = std::fs::read(&store)?;
    f.write(
        "claude",
        "projects/p/s9.jsonl",
        &message("m9", "2026-10-04T00:00:00Z", 1, 0, 0, 1),
    )?;
    let dirs: Vec<ConfigDir> = DIRS
        .iter()
        .map(|(account, dir)| ConfigDir {
            account: (*account).into(),
            path: f.dir(dir),
        })
        .collect();
    let report = ingest(
        &f.roots,
        &Request {
            dirs: &dirs,
            chair_sessions: &BTreeSet::new(),
            provider: None,
            since: None,
            deadline: Some(Instant::now()),
            incremental: false,
        },
    )?;
    assert!(report.deadline_reached);
    assert_eq!((report.files, report.added), (0, 0));
    assert_eq!(std::fs::read(&store)?, before);
    Ok(())
}

#[test]
fn incremental_ingest_resumes_complete_lines_and_recovers_replacements() -> Result {
    use std::io::Write;
    let f = Fixture::new()?;
    let at = "2026-10-01T00:00:00Z";
    let path = "projects/p/s1.jsonl";
    let first = message("m1", at, 10, 0, 0, 1);
    f.write("claude", path, &first)?;
    let dirs = [ConfigDir {
        account: "unattributed".into(),
        path: f.dir("claude"),
    }];
    let chairs = BTreeSet::new();
    // A recently modified long session may contain messages older than the
    // hook's file cutoff; its cursor must not discard them.
    let since = "2026-10-02T00:00:00Z".parse()?;
    let run = || {
        ingest(
            &f.roots,
            &Request {
                dirs: &dirs,
                chair_sessions: &chairs,
                provider: None,
                since: Some(since),
                deadline: None,
                incremental: true,
            },
        )
    };
    assert_eq!(run()?.added, 1);
    assert_eq!(run()?.bytes_read, 0);
    let update = message("m1", at, 10, 0, 0, 9);
    let second = message("m2", at, 2, 0, 0, 3);
    let split = second.len() / 2;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(f.dir("claude").join(path))?;
    file.write_all(update.as_bytes())?;
    file.write_all(&second.as_bytes()[..split])?;
    let partial = run()?;
    assert_eq!((partial.added, partial.already_present), (0, 1));
    assert_eq!(read_rows(&f.roots, &RowFilter::default())?[0].output, 9);
    file.write_all(&second.as_bytes()[split..])?;
    let appended = run()?;
    assert_eq!(appended.added, 1);
    assert_eq!(appended.bytes_read, second.len() as u64);
    assert_eq!(run()?.bytes_read, 0);
    drop(file);
    // Replacing a transcript invalidates its cursor, even when it grows.
    let replacement = message("m3", at, 5, 0, 0, 7);
    std::fs::remove_file(f.dir("claude").join(path))?;
    f.write("claude", path, &replacement)?;
    assert_eq!(run()?.added, 1);
    assert_eq!(read_rows(&f.roots, &RowFilter::default())?.len(), 3);
    // A removed store must not leave valid cursors pointing past its contents.
    for name in segment_files(&f)? {
        std::fs::remove_file(f.base.join("state/ingest").join(name))?;
    }
    assert_eq!(run()?.added, 1);
    let cursor = std::fs::read_to_string(f.base.join("state/ingest-cursors.json"))?;
    assert!(!cursor.contains("private"));
    assert!(!cursor.contains("projects"));
    Ok(())
}

#[test]
fn incremental_ingest_upgrades_existing_chair_without_rereading() -> Result {
    let f = Fixture::new()?;
    f.write(
        "claude",
        "projects/p/s1.jsonl",
        &message("m1", "2026-10-01T00:00:00Z", 1, 0, 0, 1),
    )?;
    let dirs = [ConfigDir {
        account: "a".into(),
        path: f.dir("claude"),
    }];
    let chairs = BTreeSet::new();
    let recorded_chairs = BTreeSet::from(["s1".to_owned()]);
    let request = |chairs| Request {
        dirs: &dirs,
        chair_sessions: chairs,
        provider: None,
        since: None,
        deadline: None,
        incremental: true,
    };
    ingest(&f.roots, &request(&chairs))?;
    // Recording the chair re-attributes stored rows; the source is not reread.
    assert_eq!(ingest(&f.roots, &request(&recorded_chairs))?.bytes_read, 0);
    let rows = read_rows(&f.roots, &RowFilter::default())?;
    assert_eq!(
        rows.len(),
        1,
        "idempotent attribution without duplicate rows"
    );
    assert_eq!(rows[0].source, Source::Chair);
    Ok(())
}

#[test]
fn native_attempt_links_only_its_matching_subagent_and_updates_after_settlement() -> Result {
    use crate::native_attempts::{self, Attempt, End, Settlement};
    let f = Fixture::new()?;
    let at = "2026-10-01T00:00:00Z";
    f.write(
        "claude",
        "projects/p/s1/subagents/agent-a.jsonl",
        &message("m1", at, 1, 0, 0, 4),
    )?;
    f.write(
        "claude",
        "projects/p/s1/subagents/agent-b.jsonl",
        &message("m2", at, 2, 0, 0, 6),
    )?;
    f.write(
        "other",
        "projects/p/s1/subagents/agent-a.jsonl",
        &message("m3", at, 3, 0, 0, 8),
    )?;
    let config = native_attempts::config_key(&f.dir("claude"));
    native_attempts::start(
        &f.roots,
        Attempt {
            id: "tool-1".into(),
            alias: "reviewer".into(),
            model: "claude/opus+1".into(),
            effort: Some("high".into()),
            session: "s1".into(),
            config: config.clone(),
            started_at: at.into(),
            ended_at: None,
            settlement: None,
            agent_id: None,
        },
    )?;
    let dirs = [
        ConfigDir {
            account: "a".into(),
            path: f.dir("claude"),
        },
        ConfigDir {
            account: "b".into(),
            path: f.dir("other"),
        },
    ];
    let chairs = BTreeSet::new();
    let request = Request {
        dirs: &dirs,
        chair_sessions: &chairs,
        provider: None,
        since: None,
        deadline: None,
        incremental: true,
    };
    assert_eq!(ingest(&f.roots, &request)?.added, 3);
    assert!(
        read_rows(&f.roots, &RowFilter::default())?
            .iter()
            .all(|r| r.native_attempt.is_none())
    );
    native_attempts::end(
        &f.roots,
        End {
            config: &config,
            session: "s1",
            id: "tool-1",
            ended_at: "2026-10-01T00:03:00Z",
            settlement: Settlement::Succeeded,
            agent_id: Some("a"),
        },
    )?;
    assert_eq!(ingest(&f.roots, &request)?.bytes_read, 0);
    let rows = read_rows(&f.roots, &RowFilter::default())?;
    let matched = rows
        .iter()
        .find(|r| r.message == "m1")
        .ok_or("matched row")?;
    let unmatched = rows
        .iter()
        .find(|r| r.message == "m2")
        .ok_or("unmatched row")?;
    assert_eq!(matched.native_attempt.as_deref(), Some("tool-1"));
    assert_eq!(matched.buckets(), [1, 7, 0, 0, 4]);
    assert_eq!(unmatched.native_attempt, None);
    assert_eq!(unmatched.source, Source::Subagent);
    let other = rows
        .iter()
        .find(|r| r.message == "m3")
        .ok_or("other account")?;
    assert_eq!(other.account, "b");
    assert_eq!(other.native_attempt, None);
    let native = native_attempts::report(native_attempts::read(&f.roots)?, &rows);
    assert_eq!(native.attempts[0].attempt.alias, "reviewer");
    assert_eq!(native.attempts[0].attempt.model, "claude/opus+1");
    assert_eq!(native.attempts[0].models["claude-opus"][4], 4);
    assert_eq!(native.unlinked[0].buckets[4], 14);
    assert!(
        !std::fs::read_to_string(f.base.join("state/native-attempts.json"))?.contains("private")
    );
    Ok(())
}

#[test]
fn native_attempt_store_preserves_unknown_versions_and_rejects_ambiguous_links() -> Result {
    use crate::native_attempts::{self, Attempt, End, Settlement};
    let f = Fixture::new()?;
    std::fs::create_dir_all(f.dir("claude"))?;
    let config = native_attempts::config_key(&f.dir("claude"));
    let attempt = |id: &str| Attempt {
        id: id.into(),
        alias: "reviewer".into(),
        model: "claude-opus".into(),
        effort: None,
        session: "s1".into(),
        config: config.clone(),
        started_at: "2026-10-01T00:00:00Z".into(),
        ended_at: None,
        settlement: None,
        agent_id: None,
    };
    native_attempts::start(&f.roots, attempt("tool-1"))?;
    native_attempts::start(&f.roots, attempt("tool-2"))?;
    fn end<'a>(config: &'a str, id: &'a str) -> End<'a> {
        End {
            config,
            session: "s1",
            id,
            ended_at: "2026-10-01T00:01:00Z",
            settlement: Settlement::Succeeded,
            agent_id: Some("a"),
        }
    }
    native_attempts::end(&f.roots, end(&config, "tool-1"))?;
    assert!(matches!(
        native_attempts::end(&f.roots, end(&config, "tool-2")),
        Err(native_attempts::AttemptError::Conflict)
    ));
    let mut bad = attempt("tool-3");
    bad.alias = "private prompt words".into();
    assert!(matches!(
        native_attempts::start(&f.roots, bad),
        Err(native_attempts::AttemptError::Invalid)
    ));
    native_attempts::start(&f.roots, attempt("tool-3"))?;
    native_attempts::end(
        &f.roots,
        End {
            config: &config,
            session: "s1",
            id: "tool-3",
            ended_at: "2026-10-01T00:02:00Z",
            settlement: Settlement::Failed,
            agent_id: None,
        },
    )?;
    native_attempts::end(
        &f.roots,
        End {
            config: &config,
            session: "s1",
            id: "tool-3",
            ended_at: "2026-10-01T00:03:00Z",
            settlement: Settlement::Failed,
            agent_id: Some("b"),
        },
    )?;
    assert_eq!(
        native_attempts::read(&f.roots)?[2].agent_id.as_deref(),
        Some("b")
    );
    let path = f.base.join("state/native-attempts.json");
    let future = br#"{"schema_version":2,"attempts":[]}"#;
    std::fs::write(&path, future)?;
    assert!(matches!(
        native_attempts::start(&f.roots, attempt("tool-4")),
        Err(native_attempts::AttemptError::Store)
    ));
    assert_eq!(std::fs::read(&path)?, future);
    Ok(())
}

#[test]
fn concurrent_native_attempt_updates_wait_and_preserve_both() -> Result {
    use crate::native_attempts::{self, Attempt};
    use provenant_os::fs::{Directory, LockMode};
    use std::{ffi::OsStr, sync::mpsc};

    let f = Fixture::new()?;
    let directory = Directory::open_anchor(f.roots.state.as_path())?;
    let lock = directory.try_lock(OsStr::new("native-attempts.lock"), LockMode::Exclusive)?;
    std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel();
        let handles: Vec<_> = (0..2)
            .map(|i| {
                let tx = tx.clone();
                let roots = &f.roots;
                scope.spawn(move || {
                    tx.send(()).unwrap();
                    native_attempts::start(
                        roots,
                        Attempt {
                            id: format!("tool-{i}"),
                            alias: "alias".into(),
                            model: "model".into(),
                            effort: None,
                            session: "session".into(),
                            config: "a".repeat(64),
                            started_at: "2026-10-01T00:00:00Z".into(),
                            ended_at: None,
                            settlement: None,
                            agent_id: None,
                        },
                    )
                })
            })
            .collect();
        rx.recv().unwrap();
        rx.recv().unwrap();
        drop(lock);
        for handle in handles {
            handle.join().unwrap().unwrap();
        }
    });
    let attempts = native_attempts::read(&f.roots)?;
    assert_eq!(attempts.len(), 2);
    assert!(attempts.iter().any(|a| a.id == "tool-0"));
    assert!(attempts.iter().any(|a| a.id == "tool-1"));
    Ok(())
}

#[test]
fn ambiguous_native_trigger_and_unreported_reasoning_stay_distinct() -> Result {
    let fixture = Fixture::new()?;
    let transcript = concat!(
        "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"invented direction\"},{\"type\":\"tool_result\",\"tool_use_id\":\"fixture-tool\",\"is_error\":true,\"content\":\"invented failure\"}]}}\n",
        "{\"type\":\"system\",\"subtype\":\"compact_boundary\",\"compactMetadata\":{\"trigger\":\"auto\",\"preTokens\":123}}\n"
    ).to_owned() + &message("fixture-reply", "2026-10-01T00:00:00Z", 10, 100, 40, 3);
    fixture.write("native", "projects/p/fixture-session.jsonl", &transcript)?;
    fixture.run(&[("fixture-account", "native")], &[], None)?;
    let rows = read_rows(&fixture.roots, &RowFilter::default())?;
    assert_eq!(rows.len(), 1);
    let reply = rows[0].reply.as_ref().unwrap();
    assert_eq!(reply.inferred, crate::signals::Trigger::Mixed);
    assert!(reply.recorded.contains(&crate::signals::Event::UserMessage));
    assert!(reply.recorded.contains(&crate::signals::Event::ToolError));
    assert_eq!(reply.compactions, 1);
    assert_eq!(rows[0].reasoning_tokens, None);
    let thinking = crate::signals::thinking(&rows);
    assert_eq!(
        thinking[&crate::signals::Trigger::Mixed].reasoning_unknown,
        1
    );
    assert_eq!(thinking[&crate::signals::Trigger::Mixed].reasoning_known, 0);
    let mut notification = crate::signals::ReplyContext::default();
    notification.observe_native(&serde_json::json!({"type":"user","message":{"content":[{"type":"text","text":"<task-notification><task-id>fixture-task</task-id><status>completed</status></task-notification>"}]}}));
    assert_eq!(
        notification.inferred,
        crate::signals::Trigger::LaneNotification
    );
    assert_eq!(
        notification.recorded,
        vec![crate::signals::Event::UserMessage]
    );
    let mut cli_error = crate::signals::ReplyContext::default();
    cli_error.observe_native(&serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"fixture-tool","is_error":true,"content":"{\"schema_version\":1,\"command\":\"run\",\"status\":\"invalid\",\"data\":null,\"error\":{\"code\":\"INVALID\",\"message\":\"invented error\"}}"}]}}));
    assert_eq!(
        cli_error.inferred,
        crate::signals::Trigger::ProvenantCliError
    );
    assert_eq!(cli_error.recorded, vec![crate::signals::Event::ToolError]);
    let unknown = serde_json::json!({"type":"user","message":{"content":"<unsupported-notification>invented</unsupported-notification>"}});
    let direction = serde_json::json!({"type":"user","message":{"content":"invented direction"}});
    for events in [[&unknown, &direction], [&direction, &unknown]] {
        let mut context = crate::signals::ReplyContext::default();
        for event in events {
            context.observe_native(event);
        }
        assert_eq!(context.inferred, crate::signals::Trigger::Mixed);
        assert!(context.unknown_evidence);
    }
    // Excluded replies consume their preceding events; a streamed repeat of
    // that reply must not consume events destined for the next distinct reply.
    let since_fixture = Fixture::new()?;
    let filtered = transcript
        + "{\"type\":\"user\",\"message\":{\"content\":\"status?\"}}\n"
        + &message("fixture-reply", "2026-10-01T00:00:00Z", 10, 100, 40, 3)
        + &message("fixture-next", "2026-10-01T00:00:02Z", 2, 0, 0, 1);
    since_fixture.write("native", "projects/p/fixture-session.jsonl", &filtered)?;
    since_fixture.run(
        &[("fixture-account", "native")],
        &[],
        Some("2026-10-01T00:00:01Z"),
    )?;
    let included = read_rows(&since_fixture.roots, &RowFilter::default())?;
    assert_eq!(included.len(), 1);
    let isolated = included[0].reply.as_ref().unwrap();
    assert_eq!(
        isolated.inferred,
        crate::signals::Trigger::UserStatusRequest
    );
    assert_eq!(isolated.compactions, 0);
    assert_eq!(isolated.recorded, vec![crate::signals::Event::UserMessage]);
    Ok(())
}

#[test]
fn enrichment_preserves_existing_token_buckets_and_excludes_text() -> Result {
    let fixture = Fixture::new()?;
    fixture.write(
        "native",
        "projects/p/fixture-session.jsonl",
        &message("fixture-reply", "2026-10-01T00:00:00Z", 10, 100, 40, 3),
    )?;
    fixture.run(&[("fixture-account", "native")], &[], None)?;
    let before = read_rows(&fixture.roots, &RowFilter::default())?;
    assert_eq!(before[0].buckets(), [10, 7, 60, 40, 3]);
    fixture.run(&[("fixture-account", "native")], &[], None)?;
    let after = read_rows(&fixture.roots, &RowFilter::default())?;
    assert_eq!(before[0].buckets(), after[0].buckets());
    assert!(after[0].reply.is_some());
    assert!(!stored(&fixture)?.contains("private words"));
    let row = serde_json::to_value(&after[0])?;
    assert!(row.get("content").is_none());
    assert!(row.get("prompt").is_none());
    assert!(row.get("response").is_none());
    assert_eq!(row["reply"]["inferred"], "unknown");
    Ok(())
}
