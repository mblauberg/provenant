//! Help, usage and per-command text. The text lives in `src/help/*.txt` so
//! it can be diffed against the Python dispatcher byte for byte.

pub const USAGE: &str = include_str!("help/usage.txt");
pub const GENERAL: &str = include_str!("help/general.txt");

const DETAILS: &[(&str, &str)] = &[
    ("digest", include_str!("help/digest.txt")),
    ("classify", include_str!("help/classify.txt")),
    ("extract", include_str!("help/extract.txt")),
    ("root", include_str!("help/root.txt")),
    ("route", include_str!("help/route.txt")),
    ("routes", include_str!("help/routes.txt")),
    ("capabilities", include_str!("help/capabilities.txt")),
    ("worktree", include_str!("help/worktree.txt")),
    ("clean", include_str!("help/clean.txt")),
    ("reap", include_str!("help/reap.txt")),
    ("pr", include_str!("help/pr.txt")),
    ("lint", include_str!("help/lint.txt")),
    ("check", include_str!("help/check.txt")),
    ("gate", include_str!("help/gate.txt")),
    ("fabric", include_str!("help/fabric.txt")),
    ("lease", include_str!("help/lease.txt")),
    ("hosts", include_str!("help/hosts.txt")),
    ("peer", include_str!("help/peer.txt")),
    ("lanes", include_str!("help/lanes.txt")),
    ("events", include_str!("help/events.txt")),
    ("graph", include_str!("help/graph.txt")),
    ("telemetry", include_str!("help/telemetry.txt")),
    ("stats", include_str!("help/stats.txt")),
    ("retro", include_str!("help/retro.txt")),
    ("quota", include_str!("help/quota.txt")),
    ("profile", include_str!("help/profile.txt")),
    ("dispatch", include_str!("help/dispatch.txt")),
    ("batch", include_str!("help/batch.txt")),
    ("run", include_str!("help/run.txt")),
    ("help", include_str!("help/help.txt")),
];

/// Detail text for `topic`, newline terminated.
pub fn detail(topic: &str) -> Option<&'static str> {
    DETAILS
        .iter()
        .find(|(name, _)| *name == topic)
        .map(|(_, text)| *text)
}

/// First line of a detail (newline included), used for narrow usage errors.
pub fn first_line(text: &'static str) -> &'static str {
    text.find('\n').map_or(text, |end| &text[..=end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn general_help_fits_the_byte_budget() {
        assert!(GENERAL.len() <= 1100, "{} bytes", GENERAL.len());
        assert!(GENERAL.contains("  fabric ...     runtime/fabric/bin/fabric ...\n"));
        assert!(GENERAL.contains("  batch ...      skills/orchestrate/scripts/batch_run.py ...\n"));
        assert!(!GENERAL.contains("project ..."));
    }

    #[test]
    fn every_detail_is_newline_terminated_and_starts_with_usage() {
        for (name, text) in DETAILS {
            assert!(text.starts_with("usage: provenant "), "{name}");
            assert!(text.ends_with('\n') && !text.ends_with("\n\n"), "{name}");
        }
    }

    #[test]
    fn usage_names_every_verb_once() {
        let line = USAGE.trim_end();
        for verb in ["lint", "lease", "lanes", "events", "peer", "hosts"] {
            assert_eq!(line.matches(verb).count(), 1, "{verb}");
        }
    }

    #[test]
    fn lease_first_line_is_the_usage_line() {
        let text = detail("lease").unwrap();
        assert_eq!(
            first_line(text),
            "usage: provenant lease <acquire|release|verify|status|push> ...\n"
        );
    }
}
