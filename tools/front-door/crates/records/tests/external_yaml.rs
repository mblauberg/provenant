use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use provenant_records::{parse_frontmatter, parse_sidecar};

fn collect_fixtures(directory: &Path, fixtures: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_fixtures(&path, fixtures)?;
        } else if path.file_name().is_some_and(|name| name == "SKILL.md")
            || (path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "agents")
                && path
                    .extension()
                    .is_some_and(|extension| extension == "yaml"))
        {
            fixtures.push(path);
        }
    }
    Ok(())
}

#[test]
fn skill_frontmatter_and_agent_sidecars_use_the_subset() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../");
    let mut fixtures = Vec::new();
    for directory in ["skills", "library"] {
        collect_fixtures(&repository.join(directory), &mut fixtures)
            .expect("walk skill and library fixtures");
    }
    fixtures.sort();
    assert!(
        !fixtures.is_empty(),
        "fixture walk should find YAML documents"
    );

    for path in fixtures {
        let source = fs::read_to_string(&path).expect("fixture source");
        let display = path.strip_prefix(&repository).unwrap_or(&path).display();
        let result = if path.file_name().is_some_and(|name| name == "SKILL.md") {
            parse_frontmatter(&source)
        } else {
            parse_sidecar(&source)
        };
        assert!(result.is_ok(), "{display}: {:?}", result.err());
    }
}
