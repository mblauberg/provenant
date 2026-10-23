use crate::delivery_run_shape::*;
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;

fn root() -> ReceiptValue {
    ReceiptValue::from_json_str(
        r#"{
      "schema_version":1,"contract":"delivery-run","run_id":"r",
      "profile":"p","risk_tier":"routine","initial_risk_tier":"routine",
      "chair_family":"openai","risk_assessment":{},"risk_override":{},
      "high_stakes":false,"intent":{},"authority":{},"artifacts":[],"design":{},
      "evidence":[],"measures":{},"assurance":{},"reviews":[],"security":{},
      "high_stakes_controls":null,"human_gates":{},"observation":{},"incident":null,
      "retrospective":null,"repair_cycles":0,"escaped_defect":false,
      "human_corrections":[],"checkpoint":{},"degradation":null
    }"#,
    )
    .unwrap()
}

#[test]
fn field_order_required_optional_and_type_refusals() {
    let mut run = root();
    assert!(shape_errors(&run).is_empty());
    if let ReceiptValue::Object(fields) = &mut run {
        let (_, value) = fields
            .iter_mut()
            .find(|(name, _)| name == "repair_cycles")
            .unwrap();
        *value = ReceiptValue::Bool(true);
        fields.push(("zeta".into(), ReceiptValue::Null));
        fields.push(("alpha".into(), ReceiptValue::Null));
    }
    assert_eq!(
        shape_errors(&run),
        [
            "repair_cycles must be an integer, got bool",
            "RUN.json declares unknown fields: alpha, zeta",
        ]
    );
    assert_eq!(
        check_shape(&run).unwrap_err().0,
        "repair_cycles must be an integer, got bool"
    );
}

#[test]
fn non_object_roots_are_rejected_without_panicking() {
    for input in ["null", "false", "1", "\"receipt\"", "[]"] {
        let run = ReceiptValue::from_json_str(input).unwrap();
        assert_eq!(shape_errors(&run), ["RUN root must be an object"]);
    }
}

#[test]
fn receipt_decoder_uses_the_shared_checked_json_integer_boundary() {
    let error = ReceiptValue::from_json_str(r#"{"extension":18446744073709551616}"#).unwrap_err();
    assert!(matches!(
        error,
        crate::checked_json::DecodeError::NumberOutOfRange(_)
    ));
    for token in ["-9223372036854775808", "18446744073709551615"] {
        let run = ReceiptValue::from_json_str(&format!(r#"{{"repair_cycles":{token}}}"#)).unwrap();
        assert!(!shape_errors(&run)
            .iter()
            .any(|error| error == "repair_cycles must be an integer, got float"));
    }
    let negative_zero =
        ReceiptValue::from_json_str(r#"{"schema_version":-0,"repair_cycles":-0}"#).unwrap();
    let errors = shape_errors(&negative_zero);
    assert_eq!(
        errors.first().map(String::as_str),
        Some("delivery receipt must use contract delivery-run schema_version 1")
    );
    assert!(!errors
        .iter()
        .any(|error| error == "repair_cycles must be an integer, got float"));
}

#[test]
fn timestamp_walk_preserves_json_object_order_and_duplicates_replace_in_place() {
    let run = ReceiptValue::from_json_str(
        r#"{
      "evidence":[{"finished_at":"first","recorded_at":"second","finished_at":"replacement"}],
      "human_corrections":{"at":"third"},
      "observation":{"started_at":"fourth","ended_at":"fifth"}
    }"#,
    )
    .unwrap();
    assert_eq!(
        recorded_timestamps(&run),
        ["replacement", "second", "third", "fourth", "fifth"]
    );
}

#[test]
fn gate_predicates_match_delivery_receipt_rules() {
    let pending = ReceiptValue::from_json_str(
        r#"{"intent":{"approval":{"status":"approved"}},"delivery_acceptance":null}"#,
    )
    .unwrap();
    assert!(intent_approved(&pending));
    assert!(acceptance_approved(&pending));
    assert!(!run_closed(&pending).unwrap());
    let closed = ReceiptValue::from_json_str(r#"{"human_gates":{"release":{"status":"approved"}},"observation":{"status":"not_applicable"}}"#).unwrap();
    assert!(release_approved(&closed));
    assert!(run_closed(&closed).unwrap());
    assert!(!gate_approved(&ReceiptValue::Null, &["anything"]));
    assert_eq!(
        run_closed(&ReceiptValue::from_json_str(r#"{"observation":{"status":[]}}"#).unwrap()),
        Err(GateError::UnhashableStatus("list"))
    );
    assert_eq!(
        run_closed(&ReceiptValue::from_json_str(r#"{"observation":{"status":{}}}"#).unwrap()),
        Err(GateError::UnhashableStatus("dict"))
    );
}

#[test]
fn python_differential_covers_errors_predicates_and_timestamp_order() {
    let cases = [
        r#"{}"#,
        r#"{"schema_version":true,"contract":"delivery-run","repair_cycles":false,"mystery":1}"#,
        r#"{"schema_version":1.0,"contract":"delivery-run","writer_model":null,"incident":null}"#,
        r#"{"schema_version":-0,"contract":"delivery-run","repair_cycles":-0}"#,
        r#"{"schema_version":1,"contract":"other","human_gates":{"acceptance":{"status":"approved"}},"observation":{"status":"pass"},"evidence":[{"at":{"finished_at":"nested"},"finished_at":"b"}],"human_corrections":[{"recorded_at":"c"}]}"#,
        r#"{"schema_version":1,"contract":"delivery-run","human_gates":{"release":{"status":"approved"}},"observation":{"status":"not_applicable"},"delivery_acceptance":null}"#,
        r#"{"observation":{"status":[]}}"#,
        r#"{"observation":{"status":{}}}"#,
    ];
    let mut rust_results = Vec::new();
    for input in cases {
        let run = ReceiptValue::from_json_str(input).unwrap();
        let closed = match run_closed(&run) {
            Ok(value) => json!(value),
            Err(GateError::UnhashableStatus(kind)) => json!(format!("error:{kind}")),
        };
        rust_results.push(json!({
            "errors": shape_errors(&run),
            "timestamps": recorded_timestamps(&run),
            "intent": intent_approved(&run),
            "acceptance": acceptance_approved(&run),
            "release": release_approved(&run),
            "closed": closed,
        }));
    }

    let Some(mut python) = crate::oracle::python() else {
        return;
    };
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .join("skills/deliver/scripts/delivery_run_shape.py");
    let script = r#"
import importlib.util, json, sys
spec = importlib.util.spec_from_file_location('delivery_run_shape_oracle', sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
cases = json.load(sys.stdin)
out = []
for run in cases:
    try:
        closed = module.run_closed(run)
    except TypeError as error:
        closed = 'error:list' if "unhashable type: 'list'" in str(error) else 'error:dict'
    out.append({
        'errors': module.shape_errors(run),
        'timestamps': module.recorded_timestamps(run),
        'intent': module.intent_approved(run),
        'acceptance': module.acceptance_approved(run),
        'release': module.release_approved(run),
        'closed': closed,
    })
json.dump(out, sys.stdout, separators=(',', ':'))
"#;
    python.args(["-c", script]).arg(source);
    python
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = crate::oracle::spawn(&mut python).unwrap();
    let serialized = format!("[{}]", cases.join(","));
    let write_result = child
        .child_mut()
        .stdin
        .take()
        .expect("Python oracle stdin was not piped")
        .write_all(serialized.as_bytes());
    if let Err(error) = write_result {
        let _ = child.wait();
        panic!("cannot send cases to Python oracle: {error}");
    }
    let output = child.wait().unwrap();
    assert!(
        output.status.success(),
        "python oracle: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(serde_json::to_value(rust_results).unwrap(), expected);
}
