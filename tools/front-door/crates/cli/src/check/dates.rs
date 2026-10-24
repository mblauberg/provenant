//! Textual guard against fixed dates coupled to a live wall clock in tests.
//! Explicit fixture clocks are stable; deliberate live-clock cases need a local
//! `timebomb: allow REASON` beside the date. No language parser or date threshold.
use regex::Regex;
use std::path::Path;

fn findings(source: &str) -> Result<Vec<usize>, regex::Error> {
    // Production clock reads before the test module do not implicate fixtures.
    let tests = Regex::new(r"(?m)^\s*#\[(?:cfg\(test\)|test)\]")?
        .find(source)
        .map(|matched| matched.start());
    let Some(start) = tests else {
        return Ok(Vec::new());
    };
    let prefix = source.get(..start).unwrap_or("").lines().count();
    let source = source.get(start..).unwrap_or("");
    let wall_clock = Regex::new(r"(?:SystemTime|Timestamp|Zoned)::now\s*\(")?;
    if !wall_clock.is_match(source) {
        return Ok(Vec::new());
    }
    let dates = Regex::new(r"\b[12][0-9]{3}-[01][0-9]-[0-3][0-9](?:T[0-9:.,+Z-]+)?\b")?;
    Ok(source
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let allowed = line
                .split_once("timebomb: allow ")
                .is_some_and(|(_, reason)| !reason.trim().is_empty());
            (dates.is_match(line) && !allowed).then_some(prefix + index + 1)
        })
        .collect())
}

#[test]
fn date_guard_distinguishes_live_and_injected_clocks() -> Result<(), Box<dyn std::error::Error>> {
    let live = concat!(
        "#[test]\nfn example() { let now = jiff::Timestamp::",
        "now(); let date = \"2040-01-01\"; }" // timebomb: allow intentional date scanner fixture
    ); // timebomb: allow scanner fixture for a deliberate fixed date
    assert_eq!(findings(live)?.len(), 1);
    let injected = live.replace("Timestamp::now()", "fixture_clock.now()");
    assert!(findings(&injected)?.is_empty());
    assert!(
        findings(&format!(
            "{live} // timebomb: allow explicit live-clock boundary"
        ))?
        .is_empty()
    );
    Ok(())
}
#[test]
fn workspace_rust_test_dates_use_injected_clocks() -> Result<(), Box<dyn std::error::Error>> {
    fn visit(path: &Path, failures: &mut Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
        let mut entries = std::fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let kind = entry.file_type()?;
            if kind.is_dir() {
                visit(&entry.path(), failures)?;
            } else if kind.is_file() && entry.path().extension().is_some_and(|s| s == "rs") {
                let text = std::fs::read_to_string(entry.path())?;
                for line in findings(&text)? {
                    failures.push(format!("{}:{line}", entry.path().display()));
                }
            }
        }
        Ok(())
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut failures = Vec::new();
    for directory in ["crates", "src", "tests"] {
        visit(&root.join(directory), &mut failures)?;
    }
    assert!(
        failures.is_empty(),
        "fixed dates coupled to a live clock need an injected clock or local timebomb: allow REASON: {failures:?}"
    );
    Ok(())
}
