//! Free-space guard settings shared by the CLI and scheduled agent.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiskGuardSettings {
    pub enabled: bool,
    pub warn: String,
    pub pause: String,
    pub resume: String,
    pub processes: Vec<String>,
}

impl Default for DiskGuardSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            warn: "60G".into(),
            pause: "25G".into(),
            resume: "50G".into(),
            processes: [
                "cargo",
                "rustc",
                "ld",
                "ld64",
                "clang",
                "clang++",
                "cc1",
                "cc1plus",
                "sccache",
                "swift-frontend",
                "swift-build",
                "xcodebuild",
                "turbo",
                "esbuild",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        }
    }
}

impl DiskGuardSettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        let warn = crate::parse_cache_size(&self.warn)?;
        let pause = crate::parse_cache_size(&self.pause)?;
        let resume = crate::parse_cache_size(&self.resume)?;
        if pause == 0 || pause >= resume || resume > warn {
            return Err("disk_guard_threshold_order");
        }
        if self.processes.len() > 128 {
            return Err("disk_guard_process_limit");
        }
        if self.processes.iter().any(|name| {
            name.is_empty()
                || name.len() > 255
                || name == "."
                || name == ".."
                || name.contains(['/', '\\'])
                || name.chars().any(|ch| ch.is_whitespace() || ch.is_control())
        }) {
            return Err("disk_guard_process_name");
        }
        Ok(())
    }

    /// Manual checks read only guard settings, without resolving domains or roots.
    pub fn read(path: &std::path::Path) -> std::io::Result<Self> {
        #[derive(Default, Deserialize)]
        struct GuardOnly {
            #[serde(default)]
            disk_guard: DiskGuardSettings,
        }
        let parent = path
            .parent()
            .ok_or_else(|| std::io::Error::other("missing config parent"))?;
        let read = provenant_os::fs::Directory::open_anchor(parent).and_then(|directory| {
            directory.read_bounded(
                "config.toml".as_ref(),
                provenant_records::Limits::CONFIG.max_bytes,
            )
        });
        let settings = match read {
            Ok(bytes) => {
                let text = std::str::from_utf8(&bytes).map_err(std::io::Error::other)?;
                toml_edit::de::from_str::<GuardOnly>(text)
                    .map_err(|_| std::io::Error::other("invalid disk_guard configuration"))?
                    .disk_guard
            }
            Err(error) if error.is_not_found() => Self::default(),
            Err(error) => return Err(std::io::Error::other(error)),
        };
        settings.validate().map_err(std::io::Error::other)?;
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use crate::{ConfigError, Origin, load, test_support};
    use provenant_core::{PathResolver, RootInputs, Roots};
    use std::{
        io,
        path::{Path, PathBuf},
    };

    struct Resolver;
    impl PathResolver for Resolver {
        fn resolve_existing_ancestor(&self, path: &Path) -> io::Result<PathBuf> {
            Ok(path.to_path_buf())
        }
    }
    fn roots() -> Roots {
        let mut inputs = RootInputs::new("/fixture/home".into(), "/fixture/checkout".into());
        inputs.state_dir = Some("/fixture/state".into());
        inputs.resolve(&Resolver).unwrap()
    }

    #[test]
    fn defaults_enable_guard_with_compiled_thresholds_and_exact_process_names() {
        let config = load::<&[u8]>(&roots(), None).unwrap();
        let value = serde_json::to_value(&config).unwrap();
        assert_eq!(value["disk_guard"]["enabled"], true);
        assert_eq!(value["disk_guard"]["warn"], "60G");
        assert_eq!(value["disk_guard"]["pause"], "25G");
        assert_eq!(value["disk_guard"]["resume"], "50G");
        assert_eq!(
            value["disk_guard"]["processes"],
            serde_json::json!([
                "cargo",
                "rustc",
                "ld",
                "ld64",
                "clang",
                "clang++",
                "cc1",
                "cc1plus",
                "sccache",
                "swift-frontend",
                "swift-build",
                "xcodebuild",
                "turbo",
                "esbuild"
            ])
        );
        test_support::assert_leaf_origins(&config);
        assert!(matches!(
            config.origins["disk_guard.processes.0"],
            Origin::Compiled { .. }
        ));
    }

    #[test]
    fn manual_reader_validates_only_the_disk_guard_table() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        assert_eq!(
            super::DiskGuardSettings::read(&path).unwrap(),
            super::DiskGuardSettings::default()
        );
        std::fs::write(
            &path,
            "schema_version='unrelated'\n[domains]\ninvalid=42\n[disk_guard]\nwarn='80G'",
        )
        .unwrap();
        assert_eq!(super::DiskGuardSettings::read(&path).unwrap().warn, "80G");
        std::fs::write(&path, "[disk_guard]\npause='70G'").unwrap();
        assert!(super::DiskGuardSettings::read(&path).is_err());
    }

    #[test]
    fn user_settings_parse_binary_sizes_and_keep_origins_for_inherited_values() {
        let config = load(
            &roots(),
            Some(b"schema_version=1\n[disk_guard]\nenabled=false\nwarn='80GiB'\npause='512MiB'\nresume='1'\nprocesses=['clang++','custom-build']".as_slice()),
        )
        .unwrap();
        assert!(!config.disk_guard.enabled);
        assert_eq!(config.disk_guard.warn, "80GiB");
        assert_eq!(config.disk_guard.pause, "512MiB");
        assert_eq!(config.disk_guard.resume, "1");
        assert_eq!(config.disk_guard.processes, ["clang++", "custom-build"]);
        assert!(matches!(
            config.origins["disk_guard.warn"],
            Origin::User { .. }
        ));
        assert!(matches!(
            config.origins["disk_guard.processes.1"],
            Origin::User { .. }
        ));
        test_support::assert_leaf_origins(&config);

        let partial = load(
            &roots(),
            Some(b"schema_version=1\n[disk_guard]\nenabled=false".as_slice()),
        )
        .unwrap();
        assert!(matches!(
            partial.origins["disk_guard.enabled"],
            Origin::User { .. }
        ));
        assert!(matches!(
            partial.origins["disk_guard.warn"],
            Origin::Compiled { .. }
        ));
        assert!(matches!(
            partial.origins["disk_guard.processes.0"],
            Origin::Compiled { .. }
        ));
        test_support::assert_leaf_origins(&partial);
    }

    #[test]
    fn invalid_sizes_and_threshold_order_are_rejected_by_config_loading() {
        for (fields, rule) in [
            ("warn='overflow'", "invalid_cache_size"),
            ("pause='18446744073709551615G'", "invalid_cache_size"),
            ("resume='1.5G'", "invalid_cache_size"),
            ("pause='0'", "disk_guard_threshold_order"),
            ("pause='50G'", "disk_guard_threshold_order"),
            ("resume='24G'", "disk_guard_threshold_order"),
            ("resume='61G'", "disk_guard_threshold_order"),
            ("enabled='yes'", "type"),
            ("unknown=true", "type"),
        ] {
            assert_refusal(fields, rule);
        }
        let config = load(
            &roots(),
            Some(b"schema_version=1\n[disk_guard]\nresume='60G'".as_slice()),
        )
        .unwrap();
        assert_eq!(config.disk_guard.resume, config.disk_guard.warn);
    }

    #[test]
    fn processes_are_bounded_exact_executable_names() {
        for name in [
            "",
            "cargo rustc",
            "cargo\trustc",
            "../cargo",
            "/bin/cargo",
            "bin\\cargo",
            ".",
            "..",
            "cargo\0",
        ] {
            let names = serde_json::to_string(&[name]).unwrap();
            assert_refusal(&format!("processes={names}"), "disk_guard_process_name");
        }
        let names = serde_json::to_string(&["x".repeat(256)]).unwrap();
        assert_refusal(&format!("processes={names}"), "disk_guard_process_name");
        let names = serde_json::to_string(&vec!["cargo"; 129]).unwrap();
        assert_refusal(&format!("processes={names}"), "disk_guard_process_limit");
        let config = load(
            &roots(),
            Some(b"schema_version=1\n[disk_guard]\nprocesses=[]".as_slice()),
        )
        .unwrap();
        assert!(config.disk_guard.processes.is_empty());
        test_support::assert_leaf_origins(&config);
    }

    fn assert_refusal(fields: &str, expected_rule: &str) {
        let text = format!("schema_version=1\n[disk_guard]\n{fields}");
        let error = load(&roots(), Some(text.as_bytes())).unwrap_err();
        match error {
            ConfigError::Schema { rule, location } => {
                assert_eq!(rule, expected_rule, "{fields}");
                assert_eq!(location.key_path, "disk_guard");
            }
            other => panic!("unexpected refusal for {fields}: {other:?}"),
        }
    }
}
