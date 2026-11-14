//! Command configuration dependencies. Parser paths, readers and help share
//! this declaration; children inherit their nearest declared parent.
use provenant_config::{ConfigError, ResolvedConfig};
use provenant_core::Roots;

pub struct ConfigRead {
    pub command: &'static str,
    /// None means the configuration inspection command reads every owner.
    pub domains: Option<&'static [&'static str]>,
}
macro_rules! reads {
    ($($command:literal => [$($domain:literal),*]),* $(,)?) => {
        pub const READS: &[ConfigRead] = &[
            $(ConfigRead { command: $command, domains: Some(&[$($domain),*]) },)*
            ConfigRead { command: "config", domains: None },
            ConfigRead { command: "check config", domains: None },
        ];
    };
}
reads! {
    "help" => [], "root" => [], "process-tree" => [],
    "ask" => ["fabric", "accounts"], "inbox" => ["fabric", "accounts"],
    "answer" => ["fabric", "accounts"], "accounts" => ["accounts"],
    "adopt" => ["fabric"], "release" => ["fabric"],
    "accounts register" => ["accounts", "quota", "fabric"],
    "accounts set" => ["accounts", "quota"],
    "accounts sync" => ["accounts", "fabric"],
    "hosts" => ["fabric"],
    "hosts doctor" => ["fabric", "accounts", "quota", "routing"], "fabric" => ["fabric", "routing", "accounts", "lanes", "run", "quota", "policy"],
    "fabric context" => ["fabric"],
    "install" => ["accounts", "fabric"], "upgrade" => ["accounts", "fabric"],
    "uninstall" => ["accounts", "fabric"],
    "route" => ["routing", "accounts", "quota", "policy", "fabric"],
    "route preferences" => ["routing"],
    "run" => ["fabric", "run", "routing", "accounts", "quota", "lanes", "policy"],
    "run admission" => ["policy"],
    "status" => ["fabric"], "wait" => ["wait", "fabric"],
    "land" => ["pr", "routing", "fabric", "run"],
    "land policy" => ["pr", "run"], "output" => ["fabric"],
    "trace" => ["fabric"], "stop" => ["fabric"],
    "retry" => ["fabric", "run", "routing", "accounts", "quota", "lanes", "policy"],
    "steer" => ["fabric"], "edit" => ["fabric", "routing"],
    "templates" => ["fabric", "routing"], "lane" => ["run", "fabric"],
    "reconcile" => ["fabric", "run", "routing", "accounts", "quota", "lanes", "policy"],
    "telemetry" => ["accounts", "routing"], "native" => ["accounts"],
    "quota" => ["accounts", "quota"], "console" => ["fabric"],
    "chair" => ["fabric", "accounts", "routing"], "worktree" => [],
    "pr" => ["fabric", "routing", "accounts", "pr"],
    "pr contributions" => ["fabric"],
    "gc" => ["fabric"], "tick" => ["fabric", "quota", "accounts", "routing", "steward"],
    "disk" => [], "cloud" => ["accounts"], "workflow" => ["routing"],
    "tracker" => [], "check" => [], "check install" => ["accounts"],
    "check providers" => ["accounts", "quota", "wait", "fabric", "run", "routing", "lanes", "policy"], "hook" => ["fabric", "run", "pr"],
    "internal" => ["fabric", "run", "routing", "accounts", "quota", "lanes", "policy", "steward"],
    "knobs" => []
}

pub fn declaration(path: &str) -> Option<&'static ConfigRead> {
    let path = path.strip_prefix("provenant ").unwrap_or(path);
    READS
        .iter()
        .filter(|row| {
            path == row.command
                || path
                    .strip_prefix(row.command)
                    .is_some_and(|tail| tail.starts_with(' '))
        })
        .max_by_key(|row| row.command.len())
}

pub fn domains_for(path: &str) -> Vec<&'static str> {
    match declaration(path) {
        Some(ConfigRead {
            domains: Some(names),
            ..
        }) => names.to_vec(),
        Some(_) => crate::domains::SECTIONS
            .entries
            .iter()
            .map(|entry| entry.name)
            .collect(),
        None => Vec::new(),
    }
}

pub fn read_for(
    roots: &Roots,
    command: &str,
) -> Result<ResolvedConfig<crate::domains::Domains>, ConfigError> {
    provenant_config::read_with_domains(roots, &sections_for(command)?)
}

pub fn show_for(
    roots: &Roots,
    command: &str,
) -> Result<ResolvedConfig<crate::domains::Domains>, ConfigError> {
    provenant_config::config_show(roots, &sections_for(command)?)
}

pub fn sections_for(
    command: &str,
) -> Result<provenant_config::DomainSections<crate::domains::Domains>, ConfigError> {
    let row = declaration(command)
        .ok_or_else(|| ConfigError::Refused(format!("missing config declaration for {command}")))?;
    Ok(match row.domains {
        Some(names) => crate::domains::SECTIONS.scoped(names),
        None => provenant_config::DomainSections::new(crate::domains::SECTIONS.entries),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    fn roots(temp: &tempfile::TempDir) -> Roots {
        let mut input =
            provenant_core::RootInputs::new(temp.path().join("home"), temp.path().join("assets"));
        input.state_dir = Some(temp.path().join("state"));
        input
            .resolve(&provenant_os::fs::SystemPathResolver)
            .unwrap()
    }
    fn command_paths(command: &clap::Command, parent: &str, paths: &mut Vec<String>) {
        let path = if parent.is_empty() {
            String::new()
        } else if parent == "provenant" {
            command.get_name().to_owned()
        } else {
            format!("{parent} {}", command.get_name())
        };
        if !path.is_empty() {
            paths.push(path.clone());
        }
        for child in command.get_subcommands() {
            command_paths(
                child,
                if path.is_empty() { "provenant" } else { &path },
                paths,
            );
        }
    }
    #[test]
    fn every_command_declares_its_config_and_unrelated_domains_cannot_refuse_it() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp);
        std::fs::create_dir_all(roots.state.as_path()).unwrap();
        let mut commands = Vec::new();
        command_paths(&crate::command_model(), "", &mut commands);
        commands.extend(READS.iter().map(|row| row.command.to_owned()));
        commands.sort();
        commands.dedup();
        let mut covered = std::collections::BTreeSet::new();
        for command in commands {
            let row = declaration(&command)
                .unwrap_or_else(|| panic!("{command} has no config declaration"));
            let selected = domains_for(&command);
            for name in &selected {
                assert!(
                    crate::domains::SECTIONS
                        .entries
                        .iter()
                        .any(|entry| entry.name == *name),
                    "{command}: unregistered domain {name}"
                );
            }
            if row.domains.is_none() || !covered.insert(selected.clone()) {
                continue;
            }
            for other in crate::domains::SECTIONS
                .entries
                .iter()
                .filter(|entry| !selected.contains(&entry.name))
            {
                std::fs::write(
                    roots.state.as_path().join("config.toml"),
                    format!("schema_version=1\n[domains]\n{}='broken'\n", other.name),
                )
                .unwrap();
                let config = read_for(&roots, &command).unwrap_or_else(|error| {
                    panic!("{command} read unrelated {}: {error}", other.name)
                });
                assert!(
                    config
                        .warnings
                        .iter()
                        .any(|warning| warning.rule == "unused_domain")
                );
                let prefix = format!("domains.{}.", other.name);
                assert!(
                    config
                        .origins
                        .iter()
                        .filter(|(key, _)| key.starts_with(&prefix))
                        .all(|(_, origin)| matches!(
                            origin,
                            provenant_config::Origin::Compiled { .. }
                        ))
                );
            }
        }
    }
    #[test]
    fn scoped_edit_preserves_unrelated_broken_settings() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp);
        std::fs::create_dir_all(roots.state.as_path()).unwrap();
        let path = roots.state.as_path().join("config.toml");
        std::fs::write(&path, "schema_version=1\n[domains]\nsteward='broken'\n").unwrap();
        provenant_config::edit_config(
            &roots,
            &sections_for("quota").unwrap(),
            &provenant_config::EditOptions::new("test"),
            |document, _| {
                document["profile"] = toml_edit::value("economy");
                Ok(())
            },
        )
        .unwrap();
        let document = std::fs::read_to_string(path)
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert_eq!(document["domains"]["steward"].as_str(), Some("broken"));
        assert_eq!(
            read_for(&roots, "quota").unwrap().profile,
            provenant_config::Profile::Economy
        );
    }
    #[test]
    fn operational_commands_resolve_compiled_defaults_without_project_or_provider_config() {
        let temp = tempfile::tempdir().unwrap();
        let roots = roots(&temp);
        for command in [
            "worktree", "gc", "disk", "quota", "route", "tracker", "check", "pr", "land",
        ] {
            let config = read_for(&roots, command).unwrap();
            assert!(config.domains.accounts.entries.is_empty());
            assert!(
                config
                    .origins
                    .values()
                    .all(|origin| matches!(origin, provenant_config::Origin::Compiled { .. }))
            );
            assert!(!roots.state.as_path().exists());
        }
        assert!(!temp.path().join(".provenant").exists());
    }
}
