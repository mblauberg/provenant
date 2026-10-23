use super::*;
use serde_json::json;
use std::io::Write;
use std::process::{Command, Stdio};

fn fixture(run: &Path) -> (Value, BTreeMap<String, Vec<u8>>, Value) {
    let digest = digest_bytes(b"result\n");
    let adapter = json!({
        "tool": "codex", "adapter": "codex", "execution_intent": "ordinary",
        "resolved_model": "fixture-model", "provider_family": "openai",
        "model_family": "fixture-model", "endpoint_provider": "openai",
        "identity_source": "fixture", "output_path": run.join("result.md"),
        "output_digest": digest, "read_only_guarantee": "none", "status": "ok",
        "exit": 0, "cross_family": false, "certification_eligible": false,
    });
    let record = json!({
        "status": "ok", "process": {"observed_exit": true, "exit_code": 0},
        "result": {"path": "result.md", "digest": digest},
        "requested_route": {"adapter": "codex", "intent": "ordinary"},
        "route": {"adapter": "codex", "execution_intent": "ordinary",
                  "provider_family": "openai", "resolved_model": "fixture-model"},
    });
    let payloads = BTreeMap::from([
        ("result".into(), b"result\n".to_vec()),
        (
            "adapter_receipt".into(),
            serde_json::to_vec(&adapter).unwrap(),
        ),
    ]);
    (record, payloads, adapter)
}

const CASES: &[(&str, Option<&str>)] = &[
    ("negative_zero_adapter_exit", None),
    ("nonfinite_unknown", None),
    ("overflow_unknown", None),
    ("python_blank_tail", None),
    ("valid", None),
    ("legacy_success", None),
    ("unknown_fields", None),
    ("leading_lines", None),
    ("unicode_lines", None),
    ("absolute_result", None),
    (
        "invalid_status",
        Some("successful attempt status is invalid"),
    ),
    (
        "missing_process",
        Some("successful attempt does not prove process exit"),
    ),
    (
        "unobserved_exit",
        Some("successful attempt does not prove process exit"),
    ),
    (
        "numeric_observation",
        Some("successful attempt does not prove process exit"),
    ),
    (
        "false_exit",
        Some("successful attempt does not prove exit 0"),
    ),
    (
        "float_exit",
        Some("successful attempt does not prove exit 0"),
    ),
    (
        "nonzero_exit",
        Some("successful attempt does not prove exit 0"),
    ),
    (
        "missing_result",
        Some("successful attempt has no result evidence"),
    ),
    (
        "missing_result_bytes",
        Some("successful attempt has no result evidence"),
    ),
    ("empty_result", Some("successful attempt result is empty")),
    (
        "bad_result_digest",
        Some("successful attempt result digest is invalid"),
    ),
    (
        "bad_result_path",
        Some("successful attempt result digest is invalid"),
    ),
    (
        "missing_adapter_receipt",
        Some("successful attempt has no adapter receipt"),
    ),
    (
        "invalid_utf8",
        Some("successful adapter receipt is not valid JSON"),
    ),
    (
        "invalid_json",
        Some("successful adapter receipt is not valid JSON"),
    ),
    (
        "trailing_invalid_json",
        Some("successful adapter receipt is not valid JSON"),
    ),
    (
        "empty_adapter",
        Some("successful adapter receipt is invalid"),
    ),
    (
        "array_adapter",
        Some("successful adapter receipt is invalid"),
    ),
    (
        "failed_adapter",
        Some("successful adapter receipt is invalid"),
    ),
    ("missing_route", Some("successful attempt route is invalid")),
    (
        "missing_requested_route",
        Some("successful attempt route is invalid"),
    ),
    (
        "missing_adapter_field",
        Some("successful adapter receipt is missing route identity"),
    ),
    (
        "bad_adapter",
        Some("successful adapter receipt does not match the requested adapter"),
    ),
    (
        "bad_tool",
        Some("successful adapter receipt does not match the requested adapter"),
    ),
    (
        "bad_intent",
        Some("successful adapter receipt does not match the requested intent"),
    ),
    (
        "bad_output_path",
        Some("successful adapter receipt does not match the retained output"),
    ),
    (
        "nul_output_path",
        Some("successful adapter receipt does not match the retained output"),
    ),
    (
        "bad_output_digest",
        Some("successful adapter receipt does not match the retained output"),
    ),
    (
        "false_adapter_exit",
        Some("successful adapter receipt must record exit 0"),
    ),
    (
        "float_adapter_exit",
        Some("successful adapter receipt must record exit 0"),
    ),
    (
        "nonzero_adapter_exit",
        Some("successful adapter receipt must record exit 0"),
    ),
    (
        "missing_assurance_flag",
        Some("successful adapter receipt is missing assurance flags"),
    ),
    (
        "numeric_assurance_flag",
        Some("successful adapter receipt is missing assurance flags"),
    ),
    (
        "ordinary_certification",
        Some("ordinary execution cannot be certification eligible"),
    ),
    (
        "route_disagreement",
        Some("successful attempt route disagrees with adapter receipt: resolved_model"),
    ),
];

fn mutate(
    name: &str,
    run: &Path,
    record: &mut Value,
    payloads: &mut BTreeMap<String, Vec<u8>>,
    adapter: &mut Value,
) {
    match name {
        "negative_zero_adapter_exit"
        | "nonfinite_unknown"
        | "overflow_unknown"
        | "python_blank_tail" => {}
        "valid" => {}
        "legacy_success" => record["status"] = json!("succeeded"),
        "unknown_fields" => {
            record["future"] = json!({"any": [null, true]});
            adapter["future"] = json!(42);
        }
        "leading_lines"
        | "unicode_lines"
        | "invalid_utf8"
        | "invalid_json"
        | "trailing_invalid_json"
        | "empty_adapter"
        | "array_adapter" => {}
        "absolute_result" => record["result"]["path"] = json!(run.join("result.md")),
        "invalid_status" => record["status"] = json!("partial"),
        "missing_process" => {
            record.as_object_mut().unwrap().remove("process");
        }
        "unobserved_exit" => record["process"]["observed_exit"] = json!(false),
        "numeric_observation" => record["process"]["observed_exit"] = json!(1),
        "false_exit" => record["process"]["exit_code"] = json!(false),
        "float_exit" => record["process"]["exit_code"] = json!(0.0),
        "nonzero_exit" => record["process"]["exit_code"] = json!(1),
        "missing_result" => {
            record.as_object_mut().unwrap().remove("result");
        }
        "missing_result_bytes" => {
            payloads.remove("result");
        }
        "empty_result" => {
            payloads.insert("result".into(), vec![]);
        }
        "bad_result_digest" => record["result"]["digest"] = json!("sha256:wrong"),
        "bad_result_path" => record["result"]["path"] = json!(null),
        "missing_adapter_receipt" => {}
        "failed_adapter" => adapter["status"] = json!("failed"),
        "missing_route" => {
            record.as_object_mut().unwrap().remove("route");
        }
        "missing_requested_route" => {
            record.as_object_mut().unwrap().remove("requested_route");
        }
        "missing_adapter_field" => {
            adapter.as_object_mut().unwrap().remove("provider_family");
        }
        "bad_adapter" => adapter["adapter"] = json!("other"),
        "bad_tool" => adapter["tool"] = json!("other"),
        "bad_intent" => adapter["execution_intent"] = json!("assurance"),
        "bad_output_path" => adapter["output_path"] = json!(run.join("other.md")),
        "nul_output_path" => adapter["output_path"] = json!("\u{0000}"),
        "bad_output_digest" => adapter["output_digest"] = json!("sha256:wrong"),
        "false_adapter_exit" => adapter["exit"] = json!(false),
        "float_adapter_exit" => adapter["exit"] = json!(0.0),
        "nonzero_adapter_exit" => adapter["exit"] = json!(1),
        "missing_assurance_flag" => {
            adapter.as_object_mut().unwrap().remove("cross_family");
        }
        "numeric_assurance_flag" => adapter["certification_eligible"] = json!(0),
        "ordinary_certification" => adapter["certification_eligible"] = json!(true),
        "route_disagreement" => record["route"]["resolved_model"] = json!("other"),
        _ => panic!("unknown fixture {name}"),
    }
    let line = serde_json::to_vec(&adapter).unwrap();
    let bytes = match name {
        "negative_zero_adapter_exit" => String::from_utf8(line)
            .unwrap()
            .replace("\"exit\":0", "\"exit\":-0")
            .into_bytes(),
        "nonfinite_unknown" | "overflow_unknown" => {
            let mut line = line;
            line.pop();
            line.extend_from_slice(if name == "nonfinite_unknown" {
                b", \"future\": [NaN, Infinity, -Infinity]}"
            } else {
                b", \"future\": 1e400}"
            });
            line
        }
        "python_blank_tail" => [line.as_slice(), b"\n\x1f"].concat(),
        "invalid_utf8" => vec![255],
        "invalid_json" => b"{".to_vec(),
        "trailing_invalid_json" => [line.as_slice(), b"\ninvalid"].concat(),
        "empty_adapter" => b" \n\t\r\n".to_vec(),
        "array_adapter" => b"[]".to_vec(),
        "leading_lines" => [b"progress\n{}\n", line.as_slice(), b"\n \n"].concat(),
        "unicode_lines" => [
            b"progress\x0b",
            line.as_slice(),
            "\u{2028}\u{0085}".as_bytes(),
        ]
        .concat(),
        _ => line,
    };
    if name == "missing_adapter_receipt" {
        payloads.remove("adapter_receipt");
    } else {
        payloads.insert("adapter_receipt".into(), bytes);
    }
}

#[test]
fn malformed_success_policy_matrix() {
    let run = tempfile::tempdir().unwrap();
    for (case, expected) in CASES {
        let (mut record, mut payloads, mut adapter) = fixture(run.path());
        mutate(case, run.path(), &mut record, &mut payloads, &mut adapter);
        let actual = validate_successful_attempt(run.path(), &record, &payloads)
            .err()
            .map(|error| error.to_string());
        assert_eq!(actual.as_deref(), *expected, "{case}");
    }
}

#[test]
fn all_route_identity_fields_are_nonempty_strings() {
    let run = tempfile::tempdir().unwrap();
    let (_, _, adapter) = fixture(run.path());
    for field in [
        "tool",
        "adapter",
        "execution_intent",
        "resolved_model",
        "provider_family",
        "model_family",
        "endpoint_provider",
        "identity_source",
        "output_path",
        "output_digest",
        "read_only_guarantee",
    ] {
        for value in [Value::Null, json!(false), json!(0), json!([]), json!("")] {
            let mut bad = adapter.clone();
            bad[field] = value;
            assert_eq!(
                successful_adapter_error(
                    &bad,
                    &json!("codex"),
                    &json!("ordinary"),
                    &run.path().join("result.md"),
                    &digest_bytes(b"result\n")
                ),
                Some(AttemptEvidenceError::RouteIdentity)
            );
        }
    }
}

#[test]
fn assurance_execution_can_be_certification_eligible() {
    let run = tempfile::tempdir().unwrap();
    let (mut record, mut payloads, mut adapter) = fixture(run.path());
    adapter["execution_intent"] = json!("assurance");
    adapter["certification_eligible"] = json!(true);
    record["requested_route"]["intent"] = json!("assurance");
    record["route"]["execution_intent"] = json!("assurance");
    payloads.insert(
        "adapter_receipt".into(),
        serde_json::to_vec(&adapter).unwrap(),
    );
    assert_eq!(
        validate_successful_attempt(run.path(), &record, &payloads),
        Ok(())
    );
}

#[test]
fn each_route_field_must_agree() {
    let run = tempfile::tempdir().unwrap();
    for field in [
        "adapter",
        "execution_intent",
        "provider_family",
        "resolved_model",
    ] {
        let (mut record, payloads, _) = fixture(run.path());
        record["route"][field] = json!("other");
        assert_eq!(
            validate_successful_attempt(run.path(), &record, &payloads),
            Err(AttemptEvidenceError::RouteDisagreement(field))
        );
    }
}

#[test]
fn status_alias_and_digest_contract() {
    for status in [
        Value::Null,
        json!(false),
        json!(0),
        json!({}),
        json!("partial"),
        json!("ok"),
    ] {
        assert_eq!(canonical_success_status(&status), status);
    }
    assert_eq!(canonical_success_status(&json!("succeeded")), json!("ok"));
    assert_eq!(
        digest_bytes(b""),
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_ne!(digest_bytes(b"result"), digest_bytes(b"result\n"));
}

#[cfg(unix)]
#[test]
fn output_paths_resolve_links_and_missing_components() {
    use std::os::unix::fs::symlink;
    let run = tempfile::tempdir().unwrap();
    std::fs::create_dir(run.path().join("real")).unwrap();
    symlink("real", run.path().join("link")).unwrap();
    let (_, _, mut adapter) = fixture(run.path());
    adapter["output_path"] = json!(run.path().join("missing/../link/result.md"));
    assert_eq!(
        successful_adapter_error(
            &adapter,
            &json!("codex"),
            &json!("ordinary"),
            &run.path().join("real/result.md"),
            &digest_bytes(b"result\n")
        ),
        None
    );
}

#[test]
fn differential_python_success_matrix() {
    let mut child = match Command::new("python3").args(["-c", r#"
import importlib.util,json,sys
from pathlib import Path
spec=importlib.util.spec_from_file_location('attempt_evidence_oracle',sys.argv[1])
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
answers=[]
for case in json.load(sys.stdin):
 try:
  m.validate_successful_attempt(Path(case['run']),case['record'],{k:bytes(v) for k,v in case['payloads'].items()})
  answers.append(None)
 except m.AttemptEvidenceError as error: answers.append(str(error))
print(json.dumps(answers))
"#]).arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../skills/orchestrate/scripts/attempt_evidence.py"))
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {eprintln!("skipped: python3 is absent");return;},
        Err(error) => panic!("cannot start Python oracle: {error}"),
    };
    let run = tempfile::tempdir().unwrap();
    let mut cases = Vec::new();
    let mut answers = Vec::new();
    for (name, _) in CASES {
        let (mut record, mut payloads, mut adapter) = fixture(run.path());
        mutate(name, run.path(), &mut record, &mut payloads, &mut adapter);
        answers.push(
            validate_successful_attempt(run.path(), &record, &payloads)
                .err()
                .map(|error| error.to_string()),
        );
        cases.push(json!({"run": run.path(), "record": record, "payloads": payloads}));
    }
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&cases).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let old: Vec<Option<String>> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(old, answers);
}

#[cfg(unix)]
#[test]
fn differential_python_non_strict_symlink_resolution() {
    use std::os::unix::fs::symlink;
    let run = tempfile::tempdir().unwrap();
    symlink("loop", run.path().join("loop")).unwrap();
    std::fs::create_dir(run.path().join("real")).unwrap();
    for index in 0..50 {
        symlink(
            if index == 49 {
                "real".to_owned()
            } else {
                format!("link{}", index + 1)
            },
            run.path().join(format!("link{index}")),
        )
        .unwrap();
    }
    let paths = [
        run.path().join("loop/result.md"),
        run.path().join("link0/missing/../result.md"),
    ];
    let rust: Vec<_> = paths
        .iter()
        .map(|path| crate::pending::resolve(path).unwrap())
        .collect();
    assert_eq!(rust[0], run.path().join("loop/result.md"));
    assert_eq!(rust[1], run.path().join("real/result.md"));
    let output = match Command::new("python3")
        .args([
            "-c",
            r#"
import json,sys
from pathlib import Path
if sys.version_info<(3,13): print('null')
else: print(json.dumps([str(Path(p).resolve()) for p in sys.argv[1:]]))
"#,
        ])
        .args(paths)
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipped: python3 is absent");
            return;
        }
        Err(error) => panic!("cannot run Python oracle: {error}"),
    };
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let old: Option<Vec<std::path::PathBuf>> = serde_json::from_slice(&output.stdout).unwrap();
    if let Some(old) = old {
        assert_eq!(rust, old);
    } else {
        eprintln!("skipped: non-strict loop semantics require Python >=3.13");
    }
}
