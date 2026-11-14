//! The released spellings this format no longer accepts.
//!
//! CLI command and option refusals and configuration-key warnings share this
//! table so each retired spelling has one current replacement.

#[derive(Clone, Copy, Debug)]
pub struct Removed {
    pub words: &'static [&'static str],
    pub flag: Option<&'static str>,
    pub replacement: &'static str,
    pub config_key: Option<&'static str>,
    /// Exact diagnostic code emitted for a removed configuration contract.
    pub diagnostic_code: Option<&'static str>,
}

pub const fn verb(words: &'static [&'static str], replacement: &'static str) -> Removed {
    Removed {
        words,
        flag: None,
        replacement,
        config_key: None,
        diagnostic_code: None,
    }
}

pub const fn flag(
    words: &'static [&'static str],
    flag: &'static str,
    replacement: &'static str,
) -> Removed {
    Removed {
        words,
        flag: Some(flag),
        replacement,
        config_key: None,
        diagnostic_code: None,
    }
}

pub const fn config_key(key: &'static str, replacement: &'static str) -> Removed {
    Removed {
        words: &[],
        flag: None,
        replacement,
        config_key: Some(key),
        diagnostic_code: None,
    }
}

/// Describe a removed configuration diagnostic code and its repair.
pub const fn diagnostic_code(code: &'static str, replacement: &'static str) -> Removed {
    Removed {
        words: &[],
        flag: None,
        replacement,
        config_key: None,
        diagnostic_code: Some(code),
    }
}

/// Every removed CLI spelling and config key. A CLI spelling exits 2 with one
/// line naming its replacement; there are no aliases.
pub const REMOVED: &[Removed] = &[
    flag(
        &["dispatch"],
        "prompt-file",
        "provenant run TEMPLATE --brief-file PATH",
    ),
    flag(
        &["dispatch"],
        "prompt-stdin",
        "provenant run TEMPLATE --brief-file -",
    ),
    flag(&["resume"], "no-fallback", "provenant retry NAME/NODE#N"),
    flag(
        &["run"],
        "prompt-file",
        "provenant run TEMPLATE --brief-file PATH",
    ),
    flag(
        &["run"],
        "prompt-stdin",
        "provenant run TEMPLATE --brief-file -",
    ),
    flag(&["retry"], "no-fallback", "provenant retry NAME/NODE#N"),
    flag(
        &["retry"],
        "prompt-file",
        "provenant retry NAME/NODE --brief TEXT",
    ),
    flag(
        &["retry"],
        "prompt-stdin",
        "provenant retry NAME/NODE --brief TEXT",
    ),
    flag(
        &["retry"],
        "operation-id",
        "no direct equivalent; use `provenant retry NAME/NODE#N`",
    ),
    flag(
        &["output"],
        "host",
        "no direct equivalent; use `provenant output NAME`",
    ),
    verb(&["dispatch"], "provenant run TEMPLATE BRIEF"),
    verb(&["lanes"], "provenant status"),
    verb(&["resume"], "provenant retry NAME/NODE"),
    verb(&["cancel"], "provenant stop NAME"),
    verb(&["continue"], "provenant retry NAME/NODE#N"),
    verb(&["redo"], "provenant retry NAME/NODE"),
    verb(&["preset"], "provenant templates"),
    verb(&["review"], "provenant run review BRIEF"),
    verb(&["stats"], "provenant telemetry cost --by node"),
    verb(&["pr", "land"], "provenant land NUMBER --wait"),
    verb(&["fabric", "graph"], "provenant status NAME"),
    verb(&["fabric_graph"], "provenant run TEMPLATE BRIEF"),
    verb(&["quota", "usage"], "provenant telemetry cost --by account"),
    verb(&["config", "migrate"], "provenant config check"),
    verb(
        &["telemetry", "backfill"],
        "nothing; this binary reads no format-1 store",
    ),
    flag(&["run"], "use", "provenant run TEMPLATE BRIEF"),
    flag(
        &["run"],
        "tier",
        "provenant run TEMPLATE BRIEF --route ROUTE",
    ),
    flag(
        &["run"],
        "operation-id",
        "provenant run TEMPLATE BRIEF --name NAME",
    ),
    flag(&["run"], "wait", "provenant wait NAME"),
    flag(&["status"], "wait", "provenant wait NAME"),
    flag(
        &["status"],
        "target",
        "provenant status NAME or provenant wait --pr NUMBER",
    ),
    flag(&["status"], "until", "provenant wait NAME --any"),
    flag(&["status"], "follow", "provenant wait NAME --follow"),
    flag(
        &["status"],
        "stats",
        "provenant telemetry cost NAME --by node",
    ),
    flag(&["status"], "by", "provenant telemetry cost NAME --by node"),
    flag(&["status"], "id", "provenant status NAME"),
    flag(&["stop"], "operation-id", "provenant stop NAME"),
    flag(&["stop"], "wait", "provenant wait NAME"),
    flag(&["retry"], "node", "provenant retry NAME/NODE"),
    flag(&["retry"], "wait", "provenant wait NAME"),
    flag(&["output"], "trace", "provenant trace NAME"),
    flag(&["reconcile"], "id", "provenant reconcile NAME"),
    flag(
        &["telemetry", "stats"],
        "run-id",
        "provenant telemetry cost NAME",
    ),
    flag(
        &["telemetry", "stats"],
        "by",
        "provenant telemetry cost --by route",
    ),
    flag(
        &["telemetry", "stats"],
        "detail",
        "provenant telemetry cost NAME",
    ),
    flag(
        &["telemetry", "trace"],
        "run",
        "provenant telemetry trace NAME",
    ),
    flag(&["pr", "status"], "wait", "provenant pr watch NUMBER"),
    config_key(
        "knobs.domains.routing.capacity_cooldown_seconds",
        "set `capacity_cooldown_seconds` in `[domains.routing]`",
    ),
    config_key(
        "knobs.domains.routing.settlement_retries",
        "set `settlement_retries` in `[domains.routing]`",
    ),
    config_key(
        "knobs.domains.routing.capacity_retry_seconds",
        "set `capacity_retry_seconds` in `[domains.routing]`",
    ),
    config_key(
        "knobs.domains.routing.capacity_retry_max_seconds",
        "set `capacity_retry_max_seconds` in `[domains.routing]`",
    ),
    config_key(
        "knobs.domains.routing.retry_delay_ceiling_seconds",
        "set `retry_delay_ceiling_seconds` in `[domains.routing]`",
    ),
    config_key(
        "lane.max_continuations",
        "no direct equivalent; use graph `limits.continuations` to bound restarts",
    ),
    config_key(
        "lane.resume_continuations",
        "no direct equivalent; automatic restarts use fresh sessions; choose the node `on_stop` policy",
    ),
    config_key(
        "domains.fabric.pipelines",
        "no direct equivalent; delete this stale pipeline table",
    ),
    diagnostic_code(
        "moved_to_schedule.starts",
        "replace `starts_unix_seconds` with `schedule.starts`",
    ),
    diagnostic_code(
        "moved_to_schedule.expires",
        "replace `expires_unix_seconds` with `schedule.expires`",
    ),
    diagnostic_code(
        "moved_to_schedule.windows",
        "replace `windows` with `schedule.windows`",
    ),
    diagnostic_code(
        "retired.bundle_bytes",
        "no direct equivalent; delete this setting; Git transport controls bundle sizing",
    ),
];

impl Removed {
    /// The one line a removed command spelling prints.
    pub fn message(&self) -> String {
        if let Some(key) = self.config_key {
            return format!("`{key}` is removed; {}", self.replacement);
        }
        if let Some(code) = self.diagnostic_code {
            return format!("`{code}` is refused; {}", self.replacement);
        }
        let path = self.words.join(" ");
        let message = if self.replacement.starts_with("no direct equivalent;") {
            self.replacement.to_owned()
        } else {
            format!("use `{}`", self.replacement)
        };
        match self.flag {
            Some(flag) => format!("`{path} --{flag}` is removed; {message}"),
            None => format!("`{path}` is removed; {message}"),
        }
    }
}

pub fn config_key_replacement(key: &str) -> Option<&'static str> {
    REMOVED
        .iter()
        .find(|removed| {
            removed.config_key.is_some_and(|retired| {
                key == retired
                    || key
                        .strip_prefix(retired)
                        .is_some_and(|suffix| suffix.starts_with('.'))
                    || key
                        .strip_suffix(retired)
                        .is_some_and(|prefix| prefix.ends_with('.'))
            })
        })
        .map(|removed| removed.replacement)
}

/// Return the remedy attached to an exact domain diagnostic code.
pub fn diagnostic_code_replacement(code: &str) -> Option<&'static str> {
    REMOVED
        .iter()
        .find(|removed| removed.diagnostic_code == Some(code))
        .map(|removed| removed.replacement)
}
