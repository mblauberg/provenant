//! Names reserved by Provenant 1.x, consumed by the first 2.0 install and
//! doctor. The table is the only source for cut-over cleanup names.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactDisposition {
    /// The current release writes the same declared name.
    Replace,
    /// The name belonged only to 1.x and is removed during cut-over.
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyArtifact {
    StableLink {
        name: &'static str,
        disposition: ArtifactDisposition,
    },
    HarnessSelector {
        selector: &'static str,
        disposition: ArtifactDisposition,
    },
    LaunchAgent {
        selector: &'static str,
        label: &'static str,
        plist: &'static str,
        disposition: ArtifactDisposition,
    },
}

pub const STABLE_BINARY_NAME: &str = "provenant";

/// Declared 1.x names. A name is Provenant-owned only when it exactly matches
/// an entry here or the release-instruction path shape in `managed`.
pub const LEGACY_1X_ARTIFACTS: &[LegacyArtifact] = &[
    LegacyArtifact::StableLink {
        name: STABLE_BINARY_NAME,
        disposition: ArtifactDisposition::Replace,
    },
    LegacyArtifact::HarnessSelector {
        selector: "toml:mcp_servers.fabric",
        disposition: ArtifactDisposition::Remove,
    },
    LegacyArtifact::HarnessSelector {
        selector: "json:/mcp/fabric",
        disposition: ArtifactDisposition::Remove,
    },
    LegacyArtifact::HarnessSelector {
        selector: "json:/mcpServers/fabric",
        disposition: ArtifactDisposition::Remove,
    },
    LegacyArtifact::LaunchAgent {
        selector: "peer-agent",
        label: "dev.provenant.peer",
        plist: "Library/LaunchAgents/dev.provenant.peer.plist",
        disposition: ArtifactDisposition::Replace,
    },
    LegacyArtifact::LaunchAgent {
        selector: "tick-agent",
        label: "dev.provenant.tick",
        plist: "Library/LaunchAgents/dev.provenant.tick.plist",
        disposition: ArtifactDisposition::Replace,
    },
    LegacyArtifact::LaunchAgent {
        selector: "gc-agent",
        label: "dev.provenant.gc",
        plist: "Library/LaunchAgents/dev.provenant.gc.plist",
        disposition: ArtifactDisposition::Remove,
    },
    LegacyArtifact::LaunchAgent {
        selector: "disk-guard-agent",
        label: "dev.provenant.disk-guard",
        plist: "Library/LaunchAgents/dev.provenant.disk-guard.plist",
        disposition: ArtifactDisposition::Remove,
    },
];

/// Resolve a declared LaunchAgent plist's home-relative path by its selector.
pub fn legacy_launch_agent_path(
    home: &std::path::Path,
    selector: &str,
) -> Option<std::path::PathBuf> {
    LEGACY_1X_ARTIFACTS
        .iter()
        .find_map(|artifact| match artifact {
            LegacyArtifact::LaunchAgent {
                selector: declared,
                plist,
                ..
            } if *declared == selector => Some(home.join(plist)),
            _ => None,
        })
}

pub fn legacy_launch_agent_label(selector: &str) -> Option<&'static str> {
    LEGACY_1X_ARTIFACTS
        .iter()
        .find_map(|artifact| match artifact {
            LegacyArtifact::LaunchAgent {
                selector: declared,
                label,
                ..
            } if *declared == selector => Some(*label),
            _ => None,
        })
}
