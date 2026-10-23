//! Optional oracle runner. Fixtures contain no provider state or credentials.
use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};

pub fn oracle(script: &str, input: &Value) -> Option<Value> {
    let python = std::env::var_os("PROVENANT_PORT_PYTHON").unwrap_or_else(|| "python3".into());
    let required = std::env::var("PROVENANT_PORT_DIFF_REQUIRED").is_ok_and(|v| v == "1");
    let unavailable = |reason: &str| {
        if required {
            panic!("PROVENANT_PORT_DIFF_REQUIRED=1 but Python oracle is unavailable: {reason}");
        }
        eprintln!("skip: Python oracle {reason}");
    };
    let version = Command::new(&python)
        .args([
            "-c",
            "import sys; raise SystemExit(0 if sys.version_info >= (3, 12) else 1)",
        ])
        .output();
    match version {
        Ok(output) if output.status.success() => (),
        Ok(_) => {
            unavailable("requires Python >= 3.12");
            return None;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            unavailable("interpreter not found");
            return None;
        }
        Err(error) => {
            unavailable(&format!("could not start: {error}"));
            return None;
        }
    }
    let scripts = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../skills/orchestrate/scripts");
    let mut child = match Command::new(&python)
        .args(["-c", &format!("import sys, json\nsys.path.insert(0, sys.argv[1])\nfixture=json.load(sys.stdin)\n{script}")])
        .arg(scripts)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
        Ok(child) => child,
        Err(e) => panic!("cannot start oracle: {e}"),
    };
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(input).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

#[test]
fn interfaces_match_python_policy_and_wire_values() {
    use crate::{
        agy_home::HomePlan, instruction_quarantine::quarantinable, lane_protection::is_full_sha,
    };
    let plans: Vec<_> = ["agy", "codex", "claude"].into_iter().flat_map(|adapter|
        ["full", "read_only", "workspace_write"].into_iter().flat_map(move |sandbox|
            [None, Some("sandbox-exec")].into_iter().map(move |confinement|
                serde_json::json!({"adapter": adapter, "applied": {"sandbox": sandbox, "confinement": confinement}})))).collect();
    let changes = vec![
        vec![],
        vec![".agents/x"],
        vec![".AGENTS/x"],
        vec![".agents/x (unresolved conflict)"],
        vec!["src/x"],
    ];
    let shas = vec![
        "a".repeat(40),
        "0".repeat(64),
        "A".repeat(40),
        "a".repeat(41),
    ];
    let fixture = serde_json::json!({"plans": plans, "changes": changes, "shas": shas});
    let Some(actual) = oracle(
        r#"
import agy_home, instruction_quarantine, lane_protection
print(json.dumps({
    'home': [p['adapter'] == 'agy' and p['applied'].get('confinement') == 'sandbox-exec' for p in fixture['plans']],
    'shim': [agy_home.needs_ps_shim(p) for p in fixture['plans']],
    'quarantine': [instruction_quarantine.quarantinable(c) for c in fixture['changes']],
    'sha': [bool(lane_protection.SHA.fullmatch(s)) for s in fixture['shas']]
}))
"#,
        &fixture,
    ) else {
        return;
    };
    let projections: Vec<_> = plans
        .iter()
        .map(|p| HomePlan {
            adapter: p["adapter"].as_str().unwrap(),
            sandbox: p["applied"]["sandbox"].as_str().unwrap(),
            confinement: p["applied"]["confinement"].as_str(),
        })
        .collect();
    assert_eq!(
        actual,
        serde_json::json!({
            "home": projections.iter().map(|p| p.stages_home()).collect::<Vec<_>>(),
            "shim": projections.iter().map(|p| p.needs_ps_shim(if cfg!(target_os="macos") {"darwin"} else {"linux"})).collect::<Vec<_>>(),
            "quarantine": changes.iter().map(|c| quarantinable(c.iter().copied())).collect::<Vec<_>>(),
            "sha": shas.iter().map(|s| is_full_sha(s)).collect::<Vec<_>>()
        })
    );
}
