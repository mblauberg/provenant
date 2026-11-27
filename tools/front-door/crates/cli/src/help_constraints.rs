use crate::limits;

pub(crate) const WAIT_JSON_CONTRACT: &str = "With --json, wait writes JSONL: zero or more event objects followed by one terminal command-result envelope. Events have an `event` field; the terminal envelope has `command`, `status` and `data`. Settlement events are emitted without --follow; --follow also emits each observed transition.";

pub(crate) fn constraints(path: &str) -> Vec<String> {
    if path == "provenant output" {
        return vec![
            "target is NAME[/NODE[#N]]; a node reads its newest settled attempt".into(),
            format!(
                "--tail N bounds the last N bytes; --tail-lines N bounds the last N lines and clamps above {0}",
                limits::TAIL_LINES
            ),
            format!(
                "a result page shows whole up to {} KiB, then a short head and tail; --final --to FILE exports the complete sealed result; at most {} changed paths of {} bytes are named; missing evidence is null",
                (limits::FINAL_HEAD_BYTES + limits::FINAL_TAIL_BYTES) / 1024,
                limits::OUTPUT_PATHS,
                limits::PATH_BYTES
            ),
            "peer tail scans share a 30-second deadline and reject changing artifacts".into(),
        ];
    }
    let values: &[&str] = match path {
        "provenant run" => &[
            "run needs a template; `provenant templates` lists them",
            "--set binds a string parameter as written and any other type as TOML; quote nothing for a string",
            "--plan sets plan=true for implementation templates",
            "--file reads a graph document; a plain text brief goes in BRIEF, --brief or --brief-file",
            "inside a lane, work joins the calling node's graph as a child",
        ],
        "provenant route resolve" => &[
            "exactly one of NAME, model or route",
            "fallback has at most two distinct models",
        ],
        "provenant telemetry stats" => &[
            "dates mean midnight UTC; timestamps need an offset",
            "since must precede until",
        ],
        "provenant retry" => &[
            "target is NAME[/NODE[#N]]: a graph recovers, a node starts its next attempt, an attempt continues",
            "a settled graph of one node infers that node; a wider graph needs /NODE",
        ],
        "provenant chair log append" => &[
            "operation-id and issued-at are generated when omitted; supply both for planned retries",
        ],
        "provenant land" => &[
            "checks once by default; wait without a value means one hour; use --wait=30m for a duration",
            "required checks must register and pass on the same head; head changes refuse merging",
            "already-merged PRs retry safe cleanup without CI or another merge",
        ],
        "provenant status" => &[
            "every unsettled graph is shown; settled graphs from the last 24 hours fill up to 20 rows, with an omission count",
            "--all pages every graph with --limit and --cursor",
        ],
        "provenant wait" => &[
            "every target resolves before the wait starts; no target waits for this project's running graphs",
            "`--pr --until complete` defaults to domains.wait.pr_complete_timeout_seconds (60 minutes); an explicit --timeout still wins",
            "exit 0 success, 1 settled without success, 2 invalid or not found, 5 needs input, 124 timeout, 130 cancelled",
            WAIT_JSON_CONTRACT,
        ],
        "provenant hook" | "provenant internal" => &["json transport is invalid for execution"],
        _ => &[],
    };
    values.iter().map(|value| (*value).into()).collect()
}
