//! Declared outward interfaces: everything a consuming repository may read from
//! Provenant or hand to it, each with its stability.
//!
//! Every list is derived from the declaration its owner keeps, never typed out
//! a second time: environment (including build admission backend/source) from
//! `provenant_core::consumer_env`, repository
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
        /// Commands a stable entry names; their names and spelling hold for 2.x.
        stable: Vec<String>,
        evolving: Vec<String>,
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

/// The command behind the stable process-tree reading.
const PROCESS_TREE_COMMAND: &str = "process-tree";

/// Commands a stable entry names. Every other command is evolving.
fn stable_commands() -> Vec<String> {
    vec![PROCESS_TREE_COMMAND.to_owned()]
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
    let stable = stable_commands();
    let evolving = crate::command_verbs()
        .into_iter()
        .filter(|verb| !stable.iter().any(|core| within(verb, core)))
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
                stability: Stability::Evolving,
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
                stability: Stability::Evolving,
                schema: JsonOutputSchema::CommandResult { envelope },
            },
            ExternalInterface::JsonOutput {
                command: "work-record JSON outputs".into(),
                stability: Stability::Evolving,
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
                stability: Stability::Evolving,
                event_fields: strings(&["event", "target", "settle", "line", "digests"]),
                terminal_schema: envelope,
            },
            ExternalInterface::ProcessTreeJson {
                command: format!("provenant {PROCESS_TREE_COMMAND} --json"),
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
            ExternalInterface::Commands { stable, evolving },
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

    /// Remove Markdown inline-code delimiters while preserving the cell text.
    fn plain_cell(cell: &str) -> String {
        let mut plain = String::with_capacity(cell.len());
        let mut remaining = cell;
        loop {
            let Some(open) = remaining.find('`') else {
                plain.push_str(remaining);
                break;
            };
            let escaped = remaining[..open]
                .bytes()
                .rev()
                .take_while(|byte| *byte == b'\\')
                .count()
                % 2
                == 1;
            if escaped {
                plain.push_str(&remaining[..=open]);
                remaining = &remaining[open + 1..];
                continue;
            }
            plain.push_str(&remaining[..open]);
            let open_ticks = remaining.as_bytes()[open..]
                .iter()
                .take_while(|byte| **byte == b'`')
                .count();
            let delimiter = &remaining[open..open + open_ticks];
            let after_open = &remaining[open + open_ticks..];
            let mut search = 0;
            let close = loop {
                let Some(relative) = after_open[search..].find(delimiter) else {
                    break None;
                };
                let candidate = search + relative;
                let end = candidate + open_ticks;
                let follows_tick = candidate > 0 && after_open.as_bytes()[candidate - 1] == b'`';
                let precedes_tick = end < after_open.len() && after_open.as_bytes()[end] == b'`';
                if !follows_tick && !precedes_tick {
                    break Some(candidate);
                }
                search = end;
            };
            let Some(close) = close else {
                plain.push_str(&remaining[open..]);
                return plain;
            };
            plain.push_str(&after_open[..close]);
            remaining = &after_open[close + open_ticks..];
        }
        plain
    }

    #[test]
    fn contract_cells_normalise_code_spans_but_keep_literal_backticks() {
        assert_eq!(plain_cell("differs from `HOME`."), "differs from HOME.");
        assert_eq!(plain_cell(r"literal \`HOME\`"), r"literal \`HOME\`");
        assert_eq!(plain_cell("unbalanced `HOME"), "unbalanced `HOME");
    }

    /// The body rows of the first table under `heading`, as trimmed cells.
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
                    .map(|cell| plain_cell(cell.trim()))
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
            if let ExternalInterface::Commands { stable, evolving } = interface {
                assert!(!stable.is_empty());
                let stable: BTreeSet<_> = stable.iter().cloned().collect();
                let evolving: BTreeSet<_> = evolving.iter().cloned().collect();
                assert!(stable.is_disjoint(&evolving));
                assert!(evolving.is_subset(&verbs));
                assert!(
                    stable
                        .iter()
                        .all(|core| verbs.iter().any(|verb| within(verb, core)))
                );
                assert!(
                    verbs.iter().all(|verb| evolving.contains(verb)
                        || stable.iter().any(|core| within(verb, core)))
                );
            }
        }
    }
}
