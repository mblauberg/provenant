use super::*;
use serde_json::Value;

#[test]
fn shared_layout_vectors_keep_one_worktree_root() {
    let bytes = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../../tests/fixtures/fabric-v1/layout-cases.json"
    ));
    let vectors: Value = serde_json::from_slice(bytes).unwrap();
    for case in vectors.as_array().unwrap() {
        let cwd = Path::new(
            case["resolved_cwd"]
                .as_str()
                .unwrap_or(case["cwd"].as_str().unwrap()),
        );
        let layout = RunLayout::from_resolved(cwd, case["git_common_dir"].as_str().map(Path::new));
        assert_eq!(
            layout.workspace_root(),
            Path::new(case["run_root"].as_str().unwrap())
        );
        assert_eq!(
            layout.agent_run(),
            Path::new(case["agent_run_dir"].as_str().unwrap())
        );
    }
}
#[test]
fn nested_non_git_cwd_accepts_only_explicit_workspace_runs() {
    let cwd = Path::new("/workspace/src/nested");
    let layout = RunLayout::from_resolved(cwd, None);
    let run = Path::new("/workspace/.agent-run/runs/one");
    assert_eq!(layout.workspace_root(), cwd);
    assert_eq!(
        layout.workspace_for_run(run, cwd, false),
        Path::new("/workspace")
    );
    assert!(layout.contains_run(run, cwd, false));
    assert!(!layout.contains_run(
        Path::new("/workspace-other/.agent-run/runs/one"),
        cwd,
        false
    ));
    assert!(!layout.contains_run(run, cwd, true));
    assert!(
        layout
            .run_path(
                "2026-10-23T00:00:00Z".parse().unwrap(),
                RunKind::Dispatch,
                "test",
                [0; 3]
            )
            .starts_with(cwd)
    );
}
#[test]
fn names_use_explicit_utc_time_sanitized_slug_and_entropy() {
    let layout = RunLayout::from_resolved(Path::new("/workspace"), None);
    let at: Timestamp = "2026-10-23T10:42:59.123456+10:00".parse().unwrap();
    assert_eq!(
        layout.run_path(
            at,
            RunKind::Dispatch,
            "..-héllo  lane/💻-..",
            [0, 0xab, 0xff]
        ),
        Path::new("/workspace/.agent-run/runs/20261023-0042-dispatch-h-llo-lane-00abff")
    );
    assert_eq!(sanitize_slug("-..💻.-"), "task");
    assert_eq!(sanitize_slug("_"), "_");
    assert_eq!(sanitize_slug(&"a".repeat(80)), "a".repeat(64));
    assert_eq!(
        sanitize_slug(&("a".repeat(63) + "-z")),
        "a".repeat(63) + "-"
    );
}
#[test]
fn invalid_kind_is_refused() {
    assert_eq!(
        RunKind::try_from("other").unwrap_err().to_string(),
        "invalid run kind"
    );
    for name in [
        "dispatch", "batch", "orch", "delivery", "mission", "review", "wf",
    ] {
        assert_eq!(RunKind::try_from(name).unwrap().as_str(), name);
    }
}
#[test]
fn differential_python_slug_and_non_git_workspace() {
    let root = tempfile::Builder::new()
        .prefix("records-layout-")
        .tempdir_in("/tmp")
        .unwrap();
    let cwd = root.path().join("src/nested");
    std::fs::create_dir_all(&cwd).unwrap();
    let explicit = root.path().join(".agent-run/runs/one");
    let slugs = ["test lane", "é😀中文", "-..x__y..-", ".", &"a".repeat(80)];
    let oracle =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../skills/orchestrate/scripts");
    let mut command = match crate::oracle::python() {
        Some(command) => command,
        None => return,
    };
    command.args(["-c",r#"
import json,re,sys
sys.path.insert(0,sys.argv[1]);import layout
slugs=json.loads(sys.argv[4])
print(json.dumps({'slugs':[re.sub(r'[^a-zA-Z0-9._-]+','-',s).strip('-.')[:64] or 'task' for s in slugs],'workspace':str(layout.run_workspace(sys.argv[3],sys.argv[2])),'contains':layout.contains_run(sys.argv[3],sys.argv[2])}))
"#]).arg(oracle).arg(&cwd).arg(&explicit).arg(serde_json::to_string(&slugs).unwrap());
    let output = crate::oracle::run(&mut command)
        .unwrap_or_else(|error| panic!("cannot run Python oracle: {error}"));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let old: Value = serde_json::from_slice(&output.stdout).unwrap();
    let cwd = crate::pending::resolve(&cwd).unwrap();
    let explicit = crate::pending::resolve(&explicit).unwrap();
    let layout = RunLayout::from_resolved(&cwd, None);
    assert_eq!(serde_json::json!(slugs.map(sanitize_slug)), old["slugs"]);
    assert_eq!(
        layout.workspace_for_run(&explicit, &cwd, false),
        Path::new(old["workspace"].as_str().unwrap())
    );
    assert_eq!(
        layout.contains_run(&explicit, &cwd, false),
        old["contains"].as_bool().unwrap()
    );
}
