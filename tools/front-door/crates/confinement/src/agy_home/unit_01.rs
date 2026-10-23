//! agy home staging decisions; effects are owned by the later staging unit.
use super::*;

impl HomePlan<'_> {
    pub fn stages_home(self) -> bool {
        self.adapter == "agy" && self.confinement == Some("sandbox-exec")
    }

    pub fn needs_ps_shim(self, platform: &str) -> bool {
        platform == "darwin"
            && ((self.adapter == "codex" && self.sandbox != "full")
                || self.confinement == Some("sandbox-exec"))
    }
}

/// Token/executable prefixes and the overlay's own copy are not linked over.
pub fn links_entry(name: &str, part: Option<&str>) -> bool {
    Some(name) != part && !UNLINKED.iter().any(|prefix| name.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_confined_agy_stages_home() {
        for adapter in ["agy", "codex", "claude"] {
            for confinement in [None, Some("native"), Some("sandbox-exec")] {
                let plan = HomePlan {
                    adapter,
                    confinement,
                    sandbox: "full",
                };
                assert_eq!(
                    plan.stages_home(),
                    adapter == "agy" && confinement == Some("sandbox-exec")
                );
            }
        }
    }

    #[test]
    fn shim_is_required_for_all_macos_sandboxes() {
        for adapter in ["agy", "codex", "claude"] {
            for sandbox in ["full", "read_only", "workspace_write"] {
                for confinement in [None, Some("sandbox-exec")] {
                    let plan = HomePlan {
                        adapter,
                        confinement,
                        sandbox,
                    };
                    assert!(!plan.needs_ps_shim("linux"));
                    assert_eq!(
                        plan.needs_ps_shim("darwin"),
                        adapter == "codex" && sandbox != "full"
                            || confinement == Some("sandbox-exec")
                    );
                }
            }
        }
    }

    #[test]
    fn overlay_excludes_secret_shim_and_overlay_source_entries() {
        assert_eq!(OVERLAY_NAME_PREFIX, "agy-home-");
        assert_eq!(OVERLAY_NAME_SUFFIX_HEX_LEN, 12);
        assert_eq!(
            [
                PRIVATE_DIRECTORY_MODE,
                PRIVATE_FILE_MODE,
                STATE_FILE_MODE,
                EXECUTABLE_MODE,
            ],
            [0o700, 0o600, 0o644, 0o755]
        );
        for prefix in UNLINKED {
            assert!(!links_entry(prefix, None));
            assert!(!links_entry(&format!("{prefix}.backup"), None));
        }
        for name in ["history.jsonl", DATABASE, "dangling-link", "settings.json"] {
            assert!(links_entry(name, None));
        }
        assert!(!links_entry(".gemini", Some(".gemini")));
        assert!(links_entry(".gemini", Some(".other")));
    }
}
