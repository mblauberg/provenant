//! `init` — write a repository's minimal port table from detection.
//!
//! The remote and default branch come from one resolved `ForgeContext` and the
//! toolchain from port-table stack detection. Only choices a later read cannot
//! re-derive are written; `verify.tiers` and `verify.default_tier` stay owned by
//! detection. Every choice is printed with the source that produced it, and an
//! existing declaration is never overwritten.
//!
//! Spec: docs/specs/state.md#one-sourced-port-table

use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::CommandResult;
use provenant_records::provenance::Source;
use provenant_records::{CommandError, Outcome};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

pub static DOMAIN: Domain = Domain {
    name: "init",
    verbs: &[Verb {
        path: &["init"],
        stability: Stability::Evolving,
        run,
    }],
};

/// What one init run decided, including whether it wrote anything.
#[derive(Debug)]
struct Report {
    plan: provenant_config::ports::InitPlan,
    wrote: bool,
    path: PathBuf,
}

/// Resolve the choices for a checkout. Host declarations are part of the
/// effective port table, so a remote declared outside the repository reaches
/// the forge context with its own source.
fn plan(
    project: &Path,
    state: &Path,
) -> Result<provenant_config::ports::InitPlan, Box<dyn std::error::Error>> {
    let ports = provenant_config::PortTable::read_for(Some(project), state)?;
    let forge = provenant_config::resolve_for(project, &ports)?;
    let detection = provenant_config::detect::detect(project);
    Ok(provenant_config::ports::init_plan(&forge, &detection))
}

/// Create the project declaration without ever replacing an existing one.
/// `create_new` closes the check-then-write race: a file that appears between
/// the existence check and the open is left alone too.
fn write_initial(path: &Path, document: &serde_json::Value) -> io::Result<bool> {
    if path.is_file() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut bytes = serde_json::to_vec_pretty(document).map_err(io::Error::other)?;
    bytes.push(b'\n');
    let mut file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error),
    };
    if let Err(error) = file.write_all(&bytes) {
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(true)
}

/// The layer and exact origin of a choice, in one phrase.
fn source_text(source: &Source) -> String {
    match source.origin.as_deref() {
        Some(origin) => format!("{}: {origin}", source.layer.description()),
        None => source.layer.description().to_owned(),
    }
}

/// Human rendering: what happened, then each choice and the source behind it.
fn render(report: &Report) -> String {
    let mut text = if report.wrote {
        format!("Wrote {}\n", report.path.display())
    } else {
        format!(
            "{} exists; left untouched. Would add:\n",
            report.path.display()
        )
    };
    for choice in &report.plan.choices {
        let value =
            serde_json::to_string(&choice.value).unwrap_or_else(|_| choice.value.to_string());
        text.push_str(&format!(
            "  {}: {} ({})\n",
            choice.key,
            value,
            source_text(&choice.source)
        ));
    }
    text
}

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Init => {
            let result = (|| -> Result<Report, Box<dyn std::error::Error>> {
                let context = crate::roots::resolve(
                    globals,
                    crate::roots::Workspace::Project,
                    crate::roots::Product::Optional,
                )?;
                let plan = plan(&context.project, context.roots.state.as_path())?;
                let path = context.project.join(provenant_config::PORTS_CONFIG);
                let wrote = write_initial(&path, &plan.document)?;
                Ok(Report { plan, wrote, path })
            })();
            match result {
                Ok(report) => {
                    let display = report.path.display().to_string();
                    let human = render(&report);
                    let data = serde_json::json!({
                        "path": display,
                        "written": report.wrote,
                        "choices": &report.plan.choices,
                    });
                    crate::emit(
                        &CommandResult::new("init", Outcome::Ok, data),
                        globals,
                        &human,
                        false,
                    )
                }
                Err(error) => crate::fail(
                    "init",
                    Outcome::Error,
                    "INIT",
                    &error.to_string(),
                    crate::REPAIR_ROOT,
                    globals,
                ),
            }
        }
        _ => return crate::commands::mismatched(),
    };
    crate::commands::rendered(status)
}

#[cfg(test)]
#[allow(unused_imports, dead_code)] // The worktree driver includes the library without a test harness.
mod tests {
    use super::*;
    use provenant_records::provenance::Layer;

    fn write(root: &Path, name: &str, data: &str) {
        let path = root.join(name);
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        fs::write(path, data).expect("write fixture");
    }

    /// A checkout with one remote and a remote HEAD, plus a Rust workspace so
    /// stack detection has a toolchain to report.
    fn fixture(root: &Path) {
        write(
            root,
            ".git/config",
            "[remote \"upstream\"]\n\turl = git@example.com:up.git\n",
        );
        write(root, ".git/HEAD", "ref: refs/heads/topic\n");
        write(root, ".git/refs/heads/topic", "0000");
        write(
            root,
            ".git/refs/remotes/upstream/HEAD",
            "ref: refs/heads/trunk\n",
        );
        write(root, "Cargo.toml", "[workspace]\n");
    }

    fn keys(plan: &provenant_config::ports::InitPlan) -> Vec<&str> {
        plan.choices.iter().map(|choice| choice.key).collect()
    }

    fn value<'a>(plan: &'a provenant_config::ports::InitPlan, key: &str) -> &'a serde_json::Value {
        &plan
            .choices
            .iter()
            .find(|choice| choice.key == key)
            .expect("choice")
            .value
    }

    /// Spec: docs/specs/state.md#one-sourced-port-table
    #[test]
    fn init_writes_the_minimal_declaration_and_reports_each_source() {
        let root = tempfile::tempdir().expect("temp dir");
        let project = root.path();
        let state = root.path().join("state");
        fs::create_dir_all(&state).expect("state");
        fixture(project);
        let first = plan(project, &state).expect("plan");

        assert_eq!(
            keys(&first),
            [
                "git.remote",
                "git.protected",
                "verify.remote",
                "verify.base",
                "build.stacks"
            ]
        );
        assert_eq!(value(&first, "git.remote").as_str(), Some("upstream"));
        assert_eq!(value(&first, "verify.remote").as_str(), Some("upstream"));
        assert_eq!(
            value(&first, "verify.base").as_str(),
            Some("upstream/trunk")
        );
        assert_eq!(
            value(&first, "git.protected"),
            &serde_json::json!(["trunk"])
        );
        assert_eq!(value(&first, "build.stacks"), &serde_json::json!(["rust"]));

        let remote = &first.choices[0].source;
        assert_eq!(remote.layer, Layer::Detected);
        assert_eq!(remote.origin.as_deref(), Some("the sole Git remote"));
        let base = &first.choices[3].source;
        assert_eq!(base.layer, Layer::Detected);
        assert_eq!(base.origin.as_deref(), Some("refs/remotes/upstream/HEAD"));
        let stacks = &first.choices[4].source;
        assert_eq!(stacks.layer, Layer::Detected);

        // Detection-owned leaves are deliberately absent from the document.
        assert_eq!(first.document["schema_version"], 3);
        assert_eq!(first.document["git"]["remote"], "upstream");
        assert_eq!(first.document["verify"]["base"], "upstream/trunk");
        assert_eq!(
            first.document["build"]["stacks"],
            serde_json::json!(["rust"])
        );
        assert!(first.document["verify"].get("tiers").is_none());
        assert!(first.document["verify"].get("default_tier").is_none());

        let path = project.join(provenant_config::PORTS_CONFIG);
        assert!(write_initial(&path, &first.document).expect("write"));
        let written = fs::read(&path).expect("read back");
        let reparsed: serde_json::Value = serde_json::from_slice(&written).expect("decode");
        assert_eq!(reparsed, first.document);

        // A second run leaves the file byte-identical and says what it would add.
        let again = plan(project, &state).expect("second plan");
        assert!(!write_initial(&path, &again.document).expect("no overwrite"));
        assert_eq!(fs::read(&path).expect("read again"), written);
        let report = Report {
            plan: again,
            wrote: false,
            path: path.clone(),
        };
        let human = render(&report);
        assert!(human.contains("left untouched"), "{human}");
        assert!(human.contains("verify.base"), "{human}");
        assert!(human.contains("refs/remotes/upstream/HEAD"), "{human}");
    }
}
