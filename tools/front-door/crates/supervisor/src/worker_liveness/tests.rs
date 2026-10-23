use super::*;
use std::path::Path;

#[test]
fn terminal_reports_require_an_observed_owned_exit() {
    for classification in TERMINAL_CLASSIFICATIONS {
        let report = terminal_report(
            classification,
            81,
            false,
            Some(WorkerExit {
                pid: 81,
                exit_status: 17,
            }),
        )
        .unwrap();
        assert_eq!(report.exit_status, 17);
        assert!(report.observed_exit);
        assert_eq!(report.classification, classification);
    }
    assert_eq!(
        terminal_report("complete", 81, false, None),
        Err(LivenessError::MissingExit)
    );
    assert_eq!(
        terminal_report(
            "complete",
            81,
            false,
            Some(WorkerExit {
                pid: 99,
                exit_status: 0
            })
        ),
        Err(LivenessError::MissingExit)
    );
    assert!(matches!(
        terminal_report("complete", 81, true, None),
        Err(LivenessError::LiveWriter { .. })
    ));
    assert!(matches!(
        terminal_report("other", 81, true, None),
        Err(LivenessError::Classification(_))
    ));
}
#[test]
fn silence_never_stops_a_live_worker_and_deadlines_rearm() {
    assert_eq!(liveness_signal(true, 100, 101), "progress");
    assert_eq!(liveness_signal(true, 100, 100), "live-no-growth");
    assert_eq!(liveness_signal(true, 100, 99), "live-no-growth");
    assert_eq!(liveness_signal(false, 0, 1), "exited");
    assert_eq!(rearm_poll_deadline(60.0, 20.0, true).unwrap(), Some(80.0));
    assert_eq!(rearm_poll_deadline(80.0, 20.0, true).unwrap(), Some(100.0));
    assert_eq!(rearm_poll_deadline(60.0, 20.0, false).unwrap(), None);
    assert_eq!(
        rearm_poll_deadline(60.0, -0.0, false),
        Err(LivenessError::PollBudget)
    );
    assert!(rearm_poll_deadline(60.0, f64::NAN, true)
        .unwrap()
        .unwrap()
        .is_nan());
    assert_eq!(
        rearm_poll_deadline(60.0, f64::INFINITY, true).unwrap(),
        Some(f64::INFINITY)
    );
}
#[test]
fn inspections_require_a_complete_observed_pid_set() {
    assert_eq!(
        require_inspection_quiescent(Some(84), None),
        Err(LivenessError::MissingPidSet)
    );
    assert!(matches!(
        require_inspection_quiescent(Some(84), Some(&BTreeSet::from([84]))),
        Err(LivenessError::LiveWriter { .. })
    ));
    assert!(require_inspection_quiescent(Some(84), Some(&BTreeSet::new())).is_ok());
    assert!(require_inspection_quiescent(None, None).is_ok());
    for action in ["inspection", "reuse"] {
        assert_eq!(
            require_worker_quiescent(84, true, action)
                .unwrap_err()
                .to_string(),
            format!("cannot {action} while owned worker PID 84 remains live")
        );
    }
}
#[test]
fn wrapper_collapse_preserves_census_order_and_fences_cycles() {
    let process = |pid, ppid, command: &str| Process {
        pid,
        ppid,
        elapsed: "32:35".into(),
        cpu: "0:00.01".into(),
        command: command.into(),
    };
    let rows = [
        process(11, 1, "zsh -lc codex exec foo"),
        process(30, 1, "unrelated"),
        process(9, 11, "codex exec foo"),
        process(22, 30, "codex exec bar"),
        process(99, 1, "worker_liveness.py codex exec"),
    ];
    assert_eq!(
        live_processes(&rows)
            .iter()
            .map(|p| p.pid)
            .collect::<Vec<_>>(),
        [9, 22]
    );
    assert!(
        live_processes(&[process(1, 2, "codex exec a"), process(2, 1, "codex exec b")]).is_empty()
    );
}
#[test]
fn differential_duration_command_and_terminal_errors_match_python() {
    let durations = [
        "1:02:03",
        "2:03",
        "3",
        "",
        "invalid",
        "1:2:3:4",
        "1_0:٢.٥",
        "\u{1c}1:02\u{1f}",
        "1:\u{1c}2",
        "1e10",
        "-0",
        "inf",
        "-Infinity",
        "NaN",
        "-nan",
        "1__0",
        "1e_2",
        "0x10",
        "1.2.3",
        "\u{2003}2\u{2003}",
    ];
    let elapsed = [
        "2-01:02:03",
        "1--2",
        "bad-01",
        "-1",
        "1:02",
        "2_0-2:03",
        "1e400-0",
    ];
    let commands = [
        "codex exec --cd '/repo/a b' task",
        "codex exec --cd=/repo/a=b task",
        "codex exec -Crelative task",
        "codex exec -C ''",
        "codex exec --cd",
        "codex exec --cd='broken",
        "codex exec --cd a\\ b",
        "codex exec --cd \"a\\$b\"",
        "codex exec --cd \"a\\\"b\"",
        "codex exec --cd=",
        "codex exec --cd a\\\nb",
        "codex exec --cd a#b",
        "codex exec --cd a\u{2003}b",
    ];
    let script = format!(
        r#"
import importlib.util, json, struct, sys
from dataclasses import asdict
from pathlib import Path
p=Path('skills/orchestrate/scripts/worker_liveness.py')
s=importlib.util.spec_from_file_location('worker_liveness', p)
m=importlib.util.module_from_spec(s); sys.modules[s.name]=m; s.loader.exec_module(m)
def bits(x): return struct.pack('>d', x).hex()
def elapsed(x):
    try: return {{'ok': bits(m.parse_elapsed(x))}}
    except ValueError as e: return {{'error': str(e)}}
def report(c,p,live,ep):
    try: return {{'ok': asdict(m.terminal_report(c,pid=p,process_live=live,exit_observation=m.WorkerExit(ep,17) if ep is not None else None))}}
    except (m.TerminalReportError,m.LiveWriterError) as e: return {{'error': str(e)}}
print(json.dumps({{'duration':[bits(m.parse_duration(x)) for x in {durations}], 'elapsed':[elapsed(x) for x in {elapsed}], 'cwd':[m.command_cwd(x) for x in {commands}], 'report':[report(*x) for x in [['complete',81,False,81],['failed',81,True,81],['blocked',81,False,None],['complete',81,False,99],['other',81,True,None]]]}}))
"#,
        durations = serde_json::to_string(&durations).unwrap(),
        elapsed = serde_json::to_string(&elapsed).unwrap(),
        commands = serde_json::to_string(&commands).unwrap()
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let Some(expected) = crate::test_support::python(&root, &script) else {
        return;
    };
    let reports: Vec<_> = [
        ("complete", false, Some(81)),
        ("failed", true, Some(81)),
        ("blocked", false, None),
        ("complete", false, Some(99)),
        ("other", true, None),
    ]
    .into_iter()
    .map(|(c, live, pid)| {
        match terminal_report(
            c,
            81,
            live,
            pid.map(|pid| WorkerExit {
                pid,
                exit_status: 17,
            }),
        ) {
            Ok(r) => serde_json::json!({"ok":r}),
            Err(e) => serde_json::json!({"error":e.to_string()}),
        }
    })
    .collect();
    let elapsed: Vec<_> = elapsed
        .into_iter()
        .map(|v| match parse_elapsed(v) {
            Ok(f) => serde_json::json!({"ok":format!("{:016x}",f.to_bits())}),
            Err(e) => serde_json::json!({"error":e.to_string()}),
        })
        .collect();
    assert_eq!(
        serde_json::json!({"duration":durations.map(|v|format!("{:016x}",parse_duration(v).to_bits())),"elapsed":elapsed,"cwd":commands.map(command_cwd),"report":reports}),
        expected
    );
}
