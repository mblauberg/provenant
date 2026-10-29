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
        std::fs::create_dir_all(base.join("state"))?;
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

#[test]
fn an_oversized_store_is_refused_rather_than_truncated() -> Result {
    let f = Fixture::new()?;
    let file = std::fs::File::create(f.base.join("state/ingest.jsonl"))?;
    file.set_len(STORE_CAP + 1)?;
    let error = read_rows(&f.roots, &RowFilter::default())
        .err()
        .ok_or("oversized store was read")?;
    assert!(error.to_string().contains("size cap"));
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
    let store = std::fs::read_to_string(f.base.join("state/ingest.jsonl"))?;
    assert!(!store.contains("private"));
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
