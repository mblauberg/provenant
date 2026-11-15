//! Declared outward interfaces: everything a consuming repository may read from
//! Provenant or hand to it, each with its stability.
//!
//! Every list is derived from the declaration its owner keeps, never typed out
//! a second time: environment from `provenant_core::consumer_env`, repository
//! paths from `provenant_config::repository_files`, process-tree fields from
//! the supervisor, exit codes from `exit_status`, commands from the parser and
//! run files from `AttemptFile`. `docs/specs/consumer-contract.md` carries the
//! same tables, and a test below checks that it agrees with this declaration.
use provenant_core::consumer_env::{Channel, Stability};
use provenant_records::{AttemptFile, AttemptFileClass, Outcome};
use serde::{Deserialize, Serialize};

/// Current outward interfaces, represented as data so documentation can be generated from one declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalContract {
    pub schema_version: u32,
    pub interfaces: Vec<ExternalInterface>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExternalInterface {
    Environment {
        variables: Vec<Variable>,
    },
    StateRoot {
        stability: Stability,
        default: String,
        environment: String,
        previous_major: String,
    },
    RepositoryFiles {
        files: Vec<RepositoryPath>,
    },
    JsonOutput {
        command: String,
        stability: Stability,
        schema: JsonOutputSchema,
    },
    WaitJsonLines {
        command: String,
        stability: Stability,
        event_fields: Vec<String>,
        terminal_schema: SchemaReference,
    },
    ProcessTreeJson {
        command: String,
        stability: Stability,
        environment: String,
        envelope: SchemaReference,
        sample: SchemaReference,
        sample_fields: Vec<String>,
        process_fields: Vec<String>,
    },
    ExitCodes {
        stability: Stability,
        codes: Vec<ExitCode>,
    },
    Commands {
        /// Core commands: their names and target spelling hold for 2.x.
        stable: Vec<String>,
        provisional: Vec<String>,
    },
    RunFiles {
        stability: Stability,
        directory: String,
        files: Vec<RunFile>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Variable {
    pub name: String,
    pub channel: Channel,
    pub stability: Stability,
    pub meaning: String,
    pub when: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryPath {
    pub path: String,
    pub entry: provenant_config::repository_files::Entry,
    pub access: provenant_config::repository_files::Access,
    pub stability: Stability,
    pub purpose: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExitCode {
    pub status: Outcome,
    pub code: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunFileClass {
    Temporary,
    ProviderState,
    Retained,
    Record,
}

impl From<AttemptFileClass> for RunFileClass {
    fn from(class: AttemptFileClass) -> Self {
        match class {
            AttemptFileClass::Temporary => Self::Temporary,
            AttemptFileClass::ProviderState => Self::ProviderState,
            AttemptFileClass::Retained => Self::Retained,
            AttemptFileClass::Record => Self::Record,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunFile {
    pub name: String,
    pub directory: bool,
    pub class: RunFileClass,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum JsonOutputSchema {
    CommandResult {
        envelope: SchemaReference,
    },
    WorkIdentity {
        envelope: SchemaReference,
        identity: SchemaReference,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaOwner {
    CommandResult,
    WorkKey,
    ProcessTreeSample,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaReference {
    pub owner: SchemaOwner,
    pub version: u64,
}

const fn command_result_schema() -> SchemaReference {
    SchemaReference {
        owner: SchemaOwner::CommandResult,
        version: provenant_records::wire::ENVELOPE_VERSION,
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

/// Commands the parser marks as core (`provenant help` lists them).
fn core_commands() -> Vec<String> {
    crate::command_model()
        .get_subcommands()
        .filter(|command| command.get::<crate::metadata::CoreCommand>().is_some())
        .map(|command| command.get_name().to_owned())
        .collect()
}

/// Whether `verb` is the command `core` or one of its subcommands.
fn within(verb: &str, core: &str) -> bool {
    verb.strip_prefix(core)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
}

/// The sole declaration of outward interfaces for the current release.
pub fn external_contract() -> ExternalContract {
    use provenant_core::consumer_env::{CONSUMER_ENV, PROCESS_TREE};
    use provenant_supervisor::process_tree;

    let envelope = command_result_schema();
    let core = core_commands();
    let provisional = crate::command_verbs()
        .into_iter()
        .filter(|verb| !core.iter().any(|core| within(verb, core)))
        .collect();
    ExternalContract {
        schema_version: 2,
        interfaces: vec![
            ExternalInterface::Environment {
                variables: CONSUMER_ENV
                    .iter()
                    .map(|var| Variable {
                        name: var.name.into(),
                        channel: var.channel,
                        stability: var.stability,
                        meaning: var.meaning.into(),
                        when: var.when.into(),
                    })
                    .collect(),
            },
            ExternalInterface::StateRoot {
                stability: Stability::Stable,
                default: format!("~/{}", provenant_core::DEFAULT_STATE_SUFFIX),
                environment: provenant_core::STATE_DIRECTORY_ENV.into(),
                previous_major: format!("~/{}", provenant_core::LEGACY_V1_STATE_SUFFIX),
            },
            ExternalInterface::RepositoryFiles {
                files: provenant_config::repository_files::REPOSITORY_FILES
                    .iter()
                    .map(|file| RepositoryPath {
                        path: file.path.into(),
                        entry: file.entry,
                        access: file.access,
                        stability: file.stability,
                        purpose: file.purpose.into(),
                    })
                    .collect(),
            },
            ExternalInterface::JsonOutput {
                command: "commands emitting command-result JSON".into(),
                stability: Stability::Stable,
                schema: JsonOutputSchema::CommandResult { envelope },
            },
            ExternalInterface::JsonOutput {
                command: "work-record JSON outputs".into(),
                stability: Stability::Stable,
                schema: JsonOutputSchema::WorkIdentity {
                    envelope,
                    identity: SchemaReference {
                        owner: SchemaOwner::WorkKey,
                        version: provenant_records::STATE_FORMAT_VERSION,
                    },
                },
            },
            ExternalInterface::WaitJsonLines {
                command: "provenant wait --json".into(),
                stability: Stability::Provisional,
                event_fields: strings(&["event", "target", "settle", "line", "digests"]),
                terminal_schema: envelope,
            },
            ExternalInterface::ProcessTreeJson {
                command: "provenant process-tree --json".into(),
                stability: Stability::Stable,
                environment: PROCESS_TREE.into(),
                envelope,
                sample: SchemaReference {
                    owner: SchemaOwner::ProcessTreeSample,
                    version: u64::from(process_tree::SAMPLE_SCHEMA_VERSION),
                },
                sample_fields: strings(process_tree::SAMPLE_FIELDS),
                process_fields: strings(process_tree::PROCESS_FIELDS),
            },
            ExternalInterface::ExitCodes {
                stability: Stability::Stable,
                codes: Outcome::ALL
                    .into_iter()
                    .map(|status| ExitCode {
                        status,
                        code: crate::exit_status(&status),
                    })
                    .collect(),
            },
            ExternalInterface::Commands {
                stable: core,
                provisional,
            },
            ExternalInterface::RunFiles {
                stability: Stability::Internal,
                directory: "attempts/<attempt>/ under the project's run store in the state root"
                    .into(),
                files: AttemptFile::ALL
                    .into_iter()
                    .map(|file| RunFile {
                        name: file.name().into(),
                        directory: file.is_directory(),
                        class: file.class().into(),
                    })
                    .collect(),
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const SPEC: &str = "../../../../docs/specs/consumer-contract.md";

    fn spec() -> String {
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SPEC))
            .unwrap()
    }

    /// The body rows of the first table under `heading`, as trimmed cells; a
    /// cell that is one code span loses its ticks.
    fn table(spec: &str, heading: &str) -> BTreeSet<Vec<String>> {
        let (_, section) = spec
            .split_once(&format!("\n{heading}\n"))
            .unwrap_or_else(|| panic!("spec has no `{heading}` section"));
        section
            .lines()
            .skip_while(|line| !line.starts_with('|'))
            .take_while(|line| line.starts_with('|'))
            .skip(2)
            .map(|line| {
                line.trim()
                    .trim_matches('|')
                    .split(" | ")
                    .map(|cell| {
                        let cell = cell.trim();
                        match cell.strip_prefix('`').and_then(|c| c.strip_suffix('`')) {
                            Some(code) if !code.contains('`') => code.to_owned(),
                            _ => cell.to_owned(),
                        }
                    })
                    .collect()
            })
            .collect()
    }

    fn label<T: Serialize>(value: T) -> String {
        serde_json::to_value(value)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    fn rows<I, R>(items: I) -> BTreeSet<Vec<String>>
    where
        I: IntoIterator<Item = R>,
        R: IntoIterator<Item = String>,
    {
        items
            .into_iter()
            .map(|row| row.into_iter().collect())
            .collect()
    }

    #[test]
    fn the_consumer_contract_spec_agrees_with_the_declaration() {
        let spec = spec();
        let contract = external_contract();
        for interface in &contract.interfaces {
            match interface {
                ExternalInterface::Environment { variables } => assert_eq!(
                    table(&spec, "## Environment"),
                    rows(variables.iter().map(|var| [
                        var.name.clone(),
                        label(var.channel),
                        label(var.stability),
                        var.meaning.clone(),
                        var.when.clone(),
                    ])),
                ),
                ExternalInterface::RepositoryFiles { files } => assert_eq!(
                    table(&spec, "## Repository files"),
                    rows(files.iter().map(|file| [
                        file.path.clone(),
                        label(file.entry),
                        label(file.access),
                        label(file.stability),
                        file.purpose.clone(),
                    ])),
                ),
                ExternalInterface::ExitCodes { codes, .. } => assert_eq!(
                    table(&spec, "## Exit codes"),
                    rows(
                        codes
                            .iter()
                            .map(|code| [label(code.status), code.code.to_string()])
                    ),
                ),
                ExternalInterface::ProcessTreeJson {
                    sample_fields,
                    process_fields,
                    ..
                } => assert_eq!(
                    table(&spec, "## Process tree"),
                    rows(
                        sample_fields
                            .iter()
                            .map(|field| ["data".to_owned(), field.clone()])
                            .chain(
                                process_fields
                                    .iter()
                                    .map(|field| ["processes[]".to_owned(), field.clone()])
                            )
                    ),
                ),
                ExternalInterface::StateRoot {
                    default,
                    environment,
                    previous_major,
                    ..
                } => {
                    for value in [default, environment, previous_major] {
                        assert!(spec.contains(&format!("`{value}`")), "{value}");
                    }
                }
                // Generated reference lists these; the spec states their rules.
                ExternalInterface::JsonOutput { .. }
                | ExternalInterface::WaitJsonLines { .. }
                | ExternalInterface::Commands { .. }
                | ExternalInterface::RunFiles { .. } => {}
            }
        }
    }

    #[test]
    fn the_declaration_round_trips_and_partitions_the_command_surface() {
        let declared = external_contract();
        let encoded = serde_json::to_vec(&declared).unwrap();
        let decoded: ExternalContract = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, declared);

        let verbs: BTreeSet<_> = crate::command_verbs().into_iter().collect();
        for interface in &declared.interfaces {
            if let ExternalInterface::Commands {
                stable,
                provisional,
            } = interface
            {
                assert!(!stable.is_empty());
                let stable: BTreeSet<_> = stable.iter().cloned().collect();
                let provisional: BTreeSet<_> = provisional.iter().cloned().collect();
                assert!(stable.is_disjoint(&provisional));
                assert!(provisional.is_subset(&verbs));
                assert!(
                    stable
                        .iter()
                        .all(|core| verbs.iter().any(|verb| within(verb, core)))
                );
                assert!(verbs.iter().all(|verb| provisional.contains(verb)
                    || stable.iter().any(|core| within(verb, core))));
            }
        }
    }
}
