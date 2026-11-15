//! Removed CLI command and option spellings with their replacements.

#[derive(Clone, Copy, Debug)]
pub struct Removed {
    pub words: &'static [&'static str],
    pub flag: Option<&'static str>,
    pub replacement: &'static str,
}

pub const fn verb(words: &'static [&'static str], replacement: &'static str) -> Removed {
    Removed {
        words,
        flag: None,
        replacement,
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
    }
}

/// Every removed CLI spelling exits 2 with one line naming its replacement;
/// there are no aliases.
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
    verb(
        &["fabric", "claim"],
        "provenant adopt KIND:ID or provenant release KIND:ID",
    ),
    flag(&["land"], "batch", "provenant land NUMBER [NUMBER ...]"),
    verb(&["fabric", "graph"], "provenant status NAME"),
    verb(&["fabric", "mcp"], "provenant fabric"),
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
    flag(
        &["steer"],
        "text-stdin",
        "provenant steer TARGET --brief-file -",
    ),
];

impl Removed {
    /// The one line a removed command spelling prints.
    pub fn message(&self) -> String {
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
