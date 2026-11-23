//! `root` — print the resolved release root.
use crate::commands::{Call, Domain, Verb};
use provenant_cli::Command;
use provenant_core::consumer_env::Stability;
use provenant_records::{CommandError, CommandResult, Outcome, VersionInfo};
use serde::Serialize;

pub static DOMAIN: Domain = Domain {
    name: "root",
    verbs: &[Verb {
        path: &["root"],
        stability: Stability::Evolving,
        run,
    }],
};

fn run(inv: &Call) -> Result<Outcome, CommandError> {
    let globals = inv.globals;
    let command = inv.command.clone();
    let status = match command {
        Command::Root => {
            #[derive(Serialize)]
            struct RootInfo {
                assets: provenant_records::AbsolutePath,
                state_directory: provenant_records::AbsolutePath,
                project: provenant_records::AbsolutePath,
                package_version: &'static str,
                installation: VersionInfo,
            }
            let result = (|| -> Result<_, Box<dyn std::error::Error>> {
                let crate::roots::Resolved {
                    assets,
                    project,
                    roots,
                    ..
                } = crate::roots::resolve(
                    globals,
                    crate::roots::Workspace::Project,
                    crate::roots::Product::Required,
                )?;
                let data = RootInfo {
                    assets: provenant_records::AbsolutePath::new(&assets)?,
                    state_directory: provenant_records::AbsolutePath::new(roots.state.as_path())?,
                    project: provenant_records::AbsolutePath::new(&project)?,
                    package_version: env!("CARGO_PKG_VERSION"),
                    installation: crate::identity()?,
                };
                Ok((
                    data,
                    match assets.to_str() {
                        Some(path) => format!("{path}\n"),
                        None => {
                            format!("{}\n", assets.as_os_str().as_encoded_bytes().escape_ascii())
                        }
                    },
                ))
            })();
            match result {
                Ok((data, human)) => crate::emit(
                    &CommandResult::new("root", Outcome::Ok, data),
                    globals,
                    &human,
                    true,
                ),
                Err(error) => crate::fail(
                    "root",
                    Outcome::Error,
                    "ROOT",
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
