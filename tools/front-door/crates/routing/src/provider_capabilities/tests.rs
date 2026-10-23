use super::*;
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};

const OBSERVED: &str = "2026-10-23T12:00:00Z";

fn ordered(value: Value) -> OrderedValue {
    serde_json::from_value(value).expect("test JSON is an OrderedValue")
}

fn parsed(raw: &str) -> OrderedValue {
    load_json(raw).expect("valid test JSON")
}

fn context() -> DiscoveryContext<'static> {
    DiscoveryContext {
        observed_at: OBSERVED,
        cwd: Path::new("/workspace"),
    }
}

#[derive(Default)]
struct FakeHost {
    requests: Vec<ProcessRequest>,
    results: VecDeque<Result<ProcessResult, String>>,
}

impl FakeHost {
    fn with_results(results: impl IntoIterator<Item = ProcessResult>) -> Self {
        Self {
            requests: Vec::new(),
            results: results.into_iter().map(Ok).collect(),
        }
    }
}

impl CapabilityProcessHost for FakeHost {
    fn run_bounded(&mut self, request: &ProcessRequest) -> Result<ProcessResult, String> {
        self.requests.push(request.clone());
        self.results
            .pop_front()
            .unwrap_or_else(|| Err("unexpected process call".into()))
    }
}

#[derive(Default)]
struct FakeWriter {
    stdout: String,
    stderr: String,
    files: Vec<(PathBuf, String, bool)>,
}

impl CapabilityOutputWriter for FakeWriter {
    fn stdout(&mut self, text: &str) -> Result<(), String> {
        self.stdout.push_str(text);
        Ok(())
    }

    fn stderr(&mut self, text: &str) -> Result<(), String> {
        self.stderr.push_str(text);
        Ok(())
    }

    fn write_file(&mut self, path: &Path, text: &str, owner_only: bool) -> Result<(), String> {
        self.files
            .push((path.to_path_buf(), text.to_owned(), owner_only));
        Ok(())
    }
}

fn process_result(returncode: i32, stdout: &str, stderr: &str, timed_out: bool) -> ProcessResult {
    ProcessResult {
        returncode,
        stdout: stdout.into(),
        stderr: stderr.into(),
        timed_out,
    }
}

#[test]
fn provider_table_and_normalizers_keep_the_documented_shapes() {
    assert_eq!(
        PROVIDERS
            .iter()
            .map(|provider| provider.spec().name)
            .collect::<Vec<_>>(),
        ["agy", "claude", "codex"]
    );

    let codex = normalise_codex(
        &parsed(
            r#"{"models":[{"slug":"GPT-6","supported_reasoning_levels":[{"effort":"HIGH"},{"effort":"Ultra"}]},{"slug":"gpt-6-luna","supported_reasoning_levels":[{"effort":"max"}]}]}"#,
        ),
        OBSERVED,
    )
    .unwrap();
    assert_eq!(
        codex
            .get("models")
            .unwrap()
            .get("gpt-6")
            .unwrap()
            .get("supported_efforts"),
        Some(&ordered(json!(["high", "ultra"])))
    );
}

#[test]
fn codex_catalogue_is_model_specific_and_rejects_the_whole_malformed_input() {
    let value = normalise_codex(
        &parsed(
            r#"{"models":[{"slug":"gpt-5.6-sol","supported_reasoning_levels":[{"effort":"ULTRA"},{"effort":"max"}]},{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{"effort":"max"}]}]}"#,
        ),
        OBSERVED,
    )
    .unwrap();
    assert_eq!(
        value
            .get("models")
            .unwrap()
            .get("gpt-5.6-sol")
            .unwrap()
            .get("supported_efforts"),
        Some(&ordered(json!(["ultra", "max"])))
    );
    assert_eq!(
        value
            .get("models")
            .unwrap()
            .get("gpt-5.6-luna")
            .unwrap()
            .get("supported_efforts"),
        Some(&ordered(json!(["max"])))
    );

    for malformed in [
        json!("not an object"),
        json!({"slug": 7, "supported_reasoning_levels": []}),
        json!({"slug": "", "supported_reasoning_levels": []}),
        json!({"slug": "missing-levels"}),
        json!({"slug": "bad-levels", "supported_reasoning_levels": {}}),
        json!({"slug": "bad-level", "supported_reasoning_levels": ["high"]}),
        json!({"slug": "bad-effort", "supported_reasoning_levels": [{"effort": 7}]}),
        json!({"slug": "empty-effort", "supported_reasoning_levels": [{"effort": "  "}]}),
    ] {
        let raw = ordered(json!({"models": [
            {"slug": "valid", "supported_reasoning_levels": [{"effort": "high"}]},
            malformed
        ]}));
        assert!(normalise_codex(&raw, OBSERVED).is_err());
    }
    assert!(normalise_codex(&ordered(json!({"models": []})), OBSERVED).is_err());
    assert!(normalise_codex(
        &ordered(json!({"models": [
            {"slug": "Straße", "supported_reasoning_levels": [{"effort": "high"}]},
            {"slug": "STRASSE", "supported_reasoning_levels": [{"effort": "max"}]}
        ]})),
        OBSERVED
    )
    .is_err());
}

#[test]
fn agy_groups_effort_suffixes_and_keeps_effortless_and_foreign_models_visible() {
    let value = normalise_agy(
        "gemini-3.7-flash-high\tGemini 3.7 Flash (High)\n\
         gemini-3.7-flash-medium\tGemini 3.7 Flash (Medium)\n\
         gemini-3.7-flash-low\tGemini 3.7 Flash (Low)\n\
         claude-opus-4-6-thinking\tClaude Opus 4.6 (Thinking)\n\
         MISTRAL-agent-7\n",
        OBSERVED,
    )
    .unwrap();
    assert_eq!(
        value
            .get("models")
            .unwrap()
            .get("gemini-3.7-flash")
            .unwrap()
            .to_value(),
        json!({
            "resolved_model": "gemini-3.7-flash",
            "supported_efforts": ["low", "medium", "high"],
            "dispatchable_ids": [
                "gemini-3.7-flash-high",
                "gemini-3.7-flash-low",
                "gemini-3.7-flash-medium"
            ]
        })
    );
    assert_eq!(
        value.get("effortless_models"),
        Some(&ordered(json!([
            "MISTRAL-agent-7",
            "claude-opus-4-6-thinking"
        ])))
    );
    assert_eq!(
        value.get("google_models"),
        Some(&ordered(json!(["gemini-3.7-flash"])))
    );
    assert_eq!(
        value.get("foreign_models"),
        Some(&ordered(json!([
            "MISTRAL-agent-7",
            "claude-opus-4-6-thinking"
        ])))
    );
    assert!(normalise_agy("\n  \t\n", OBSERVED).is_err());
    assert!(normalise_agy("claude-opus-4-6\n", OBSERVED).is_err());
}

#[test]
fn claude_canary_requires_subscription_auth_and_one_matching_runtime_model() {
    let auth = ordered(json!({
        "loggedIn": true,
        "authMethod": "claude.ai",
        "subscriptionType": "pro",
        "email": "private@example.test",
        "orgId": "private-org"
    }));
    let result = ordered(json!({
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "result": "OK",
        "modelUsage": {
            "claude-haiku-4-5-20251001": {"inputTokens": 1},
            "claude-opus-4-8": {"inputTokens": 1}
        }
    }));
    let snapshot = normalise_claude(&auth, &result, "Opus", "medium", OBSERVED).unwrap();
    assert_eq!(
        snapshot
            .get("models")
            .unwrap()
            .get("opus")
            .unwrap()
            .get("resolved_model"),
        Some(&OrderedValue::String("claude-opus-4-8".into()))
    );
    let encoded = python_json_dumps(&snapshot);
    assert!(encoded.contains("subscription_runtime_canary"));
    assert!(!encoded.contains("private@example.test"));
    assert!(!encoded.contains("private-org"));

    let exact_model = ordered(json!({
        "type": "result", "subtype": "success", "is_error": false, "result": "OK",
        "modelUsage": {"claude-fable-5-1": {}}
    }));
    assert!(normalise_claude(&auth, &exact_model, "claude-fable-5-1", "medium", OBSERVED).is_ok());
    let wrong_version = ordered(json!({
        "type": "result", "subtype": "success", "is_error": false, "result": "OK",
        "modelUsage": {"claude-fable-5-0": {}}
    }));
    assert!(normalise_claude(
        &auth,
        &wrong_version,
        "claude-fable-5-1",
        "medium",
        OBSERVED
    )
    .is_err());
    let mut bad_auth = auth.clone();
    if let OrderedValue::Object(ref mut object) = bad_auth {
        object.insert("loggedIn".into(), OrderedValue::Number(1.into()));
    }
    assert!(normalise_claude(&bad_auth, &result, "opus", "medium", OBSERVED).is_err());
    let ambiguous = ordered(json!({
        "type": "result", "subtype": "success", "is_error": false, "result": "OK",
        "modelUsage": {"claude-opus-4-8": {}, "claude-opus-4-9": {}}
    }));
    assert!(normalise_claude(&auth, &ambiguous, "opus", "medium", OBSERVED).is_err());
    let unsuccessful = ordered(json!({
        "type": "result", "subtype": "success", "is_error": true, "result": "OK",
        "modelUsage": {"claude-opus-4-8": {}}
    }));
    assert!(normalise_claude(&auth, &unsuccessful, "opus", "medium", OBSERVED).is_err());
}

#[test]
fn cli_validation_happens_before_a_process_can_be_started() {
    let cases = [
        vec![
            "claude".into(),
            "--alias".into(),
            "opus".into(),
            "--effort".into(),
            "medium".into(),
        ],
        vec![
            "claude".into(),
            "--out".into(),
            "out.json".into(),
            "--alias".into(),
            " ".into(),
            "--effort".into(),
            "medium".into(),
        ],
        vec![
            "claude".into(),
            "--out".into(),
            "out.json".into(),
            "--alias".into(),
            "opus".into(),
        ],
        vec!["codex".into(), "--alias".into(), "opus".into()],
        vec![
            "claude".into(),
            "--out".into(),
            "out.json".into(),
            "--alias".into(),
            "opus".into(),
            "--effort".into(),
            "ultra".into(),
        ],
    ];
    for argv in cases {
        let mut host = FakeHost::default();
        let mut writer = FakeWriter::default();
        assert_ne!(run(&argv, &mut host, &mut writer, &context()).unwrap(), 0);
        assert!(host.requests.is_empty());
    }
}

#[test]
fn cli_parser_rejects_unknown_and_ambiguous_options_and_choices() {
    let options = ["--alias", "--all"];
    assert_eq!(
        resolve_option_from("--al", &options).unwrap_err(),
        ["--alias", "--all"]
    );
    assert_eq!(resolve_option_from("--unknown", &options).unwrap(), None);
    assert_eq!(
        parse_args(&["codex".into(), "--unknown".into()])
            .unwrap_err()
            .to_string(),
        "unrecognized arguments: --unknown"
    );
    assert_eq!(
        parse_args(&["cla".into()]).unwrap_err().to_string(),
        "argument provider: invalid choice: 'cla' (choose from 'agy', 'claude', 'codex')"
    );
    let args = parse_args(&["codex".into(), "--ou=capabilities.json".into()])
        .unwrap()
        .unwrap();
    assert_eq!(args.out, Some(PathBuf::from("capabilities.json")));
    assert!(
        parse_args(&["codex".into(), "--bin".into(), "--unknown".into()])
            .unwrap_err()
            .to_string()
            .contains("expected one argument")
    );
    assert_eq!(
        parse_args(&["codex".into(), "--bin".into(), "-5".into()])
            .unwrap()
            .unwrap()
            .binary
            .as_deref(),
        Some("-5")
    );
    assert_eq!(
        parse_args(&["codex".into(), "--bin".into(), "-.5".into()])
            .unwrap()
            .unwrap()
            .binary
            .as_deref(),
        Some("-.5")
    );
}

#[test]
fn help_output_matches_the_python_entry_point() {
    let mut host = FakeHost::default();
    let mut writer = FakeWriter::default();
    assert_eq!(
        run(&["--help".into()], &mut host, &mut writer, &context()).unwrap(),
        0
    );
    assert_eq!(writer.stdout, help_text());
    assert!(host.requests.is_empty());
}

#[test]
fn cli_validation_messages_and_help_match_python_argparse() {
    let cases: Vec<Vec<String>> = [
        vec![],
        vec!["codex", "--unknown"],
        vec!["cla", "--out", "caps.json"],
        vec!["codex", "--bin"],
        vec!["codex", "--effort", "ultra"],
        vec!["codex", "--alias", "opus"],
        vec!["claude", "--alias", "opus", "--effort", "medium"],
        vec![
            "claude",
            "--out",
            "caps.json",
            "--alias",
            " ",
            "--effort",
            "medium",
        ],
        vec!["claude", "--out", "caps.json", "--alias", "opus"],
        vec!["--help"],
    ]
    .into_iter()
    .map(|argv| argv.into_iter().map(str::to_owned).collect())
    .collect();
    let Some(expected): Option<Vec<Value>> = crate::oracle::python(
        r#"
import contextlib,importlib,json,pathlib,sys
from io import StringIO
sys.path.insert(0,str(pathlib.Path('skills/orchestrate/scripts').resolve()))
import capabilities as m
sys.argv[0]='capabilities.py'
answers=[]
for argv in json.load(sys.stdin):
 out,err=StringIO(),StringIO()
 try:
  with contextlib.redirect_stdout(out),contextlib.redirect_stderr(err):
   code=m.main(argv)
 except SystemExit as e:code=e.code
 answers.append([code,out.getvalue(),err.getvalue()])
print(json.dumps(answers))
"#,
        &json!(cases),
    )
    .expect("Python oracle should be available") else {
        return;
    };
    for (argv, expected) in cases.iter().zip(expected) {
        let mut host = FakeHost::default();
        let mut writer = FakeWriter::default();
        let code = run(argv, &mut host, &mut writer, &context()).unwrap();
        assert!(host.requests.is_empty());
        let actual = json!([code, writer.stdout, writer.stderr]);
        assert_eq!(actual, expected, "argv={argv:?}");
    }
}

#[test]
fn catalogue_providers_use_bounded_requests_and_sorted_pretty_output() {
    let catalogue =
        r#"{"models":[{"slug":"gpt-5.6-sol","supported_reasoning_levels":[{"effort":"high"}]}]}"#;
    let mut host = FakeHost::with_results([process_result(0, catalogue, " warning \n", false)]);
    let mut writer = FakeWriter::default();
    let code = run(
        &["codex".into(), "--bin".into(), "/fake/codex".into()],
        &mut host,
        &mut writer,
        &context(),
    )
    .unwrap();
    assert_eq!(code, 0);
    assert_eq!(
        host.requests,
        [ProcessRequest {
            command: vec!["/fake/codex".into(), "debug".into(), "models".into()],
            cwd: PathBuf::from("/workspace"),
            timeout: Duration::from_secs(10),
            output_limit_bytes: OUTPUT_LIMIT_BYTES,
            merge_stderr: false,
        }]
    );
    assert!(writer
        .stderr
        .contains("codex debug models warned: warning\n"));
    assert!(writer.stdout.starts_with("{\n  \"models\": {"));
    assert!(writer.stdout.ends_with("\n}\n"));
    assert!(writer.stdout.contains("\"schema_version\": 1"));
}

#[test]
fn agy_provider_uses_the_models_command_and_warns_without_rejecting_stdout() {
    let mut host = FakeHost::with_results([process_result(
        0,
        "gemini-3.8-flash-high\ngemini-3.8-flash-low\n",
        "provider notice\n",
        false,
    )]);
    let mut writer = FakeWriter::default();
    assert_eq!(
        run(&["agy".into()], &mut host, &mut writer, &context()).unwrap(),
        0
    );
    assert_eq!(host.requests.len(), 1);
    assert_eq!(host.requests[0].command, ["agy", "models"]);
    assert_eq!(host.requests[0].timeout, Duration::from_secs(60));
    assert_eq!(writer.stderr, "agy models warned: provider notice\n");
    assert!(writer.stdout.contains("\"source\": \"agy models\""));
    assert!(writer
        .stdout
        .contains("\"google_models\": [\n    \"gemini-3.8-flash\"\n  ]"));
    assert!(writer.files.is_empty());
}

#[test]
fn claude_canary_runs_both_commands_and_marks_file_output_private() {
    let auth = r#"{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"pro","email":"secret@example.com","orgId":"secret-org"}"#;
    let result = r#"{"type":"result","subtype":"success","is_error":false,"result":"OK","modelUsage":{"claude-opus-4-8":{"inputTokens":1}}}"#;
    let mut host = FakeHost::with_results([
        process_result(0, auth, "", false),
        process_result(0, result, "", false),
    ]);
    let mut writer = FakeWriter::default();
    let argv = [
        "claude".into(),
        "--out".into(),
        "snapshot.json".into(),
        "--bin".into(),
        "/fake/claude".into(),
        "--alias".into(),
        "opus".into(),
        "--effort".into(),
        "medium".into(),
    ];
    assert_eq!(run(&argv, &mut host, &mut writer, &context()).unwrap(), 0);
    assert_eq!(host.requests.len(), 2);
    assert_eq!(host.requests[0].command, ["/fake/claude", "auth", "status"]);
    assert_eq!(host.requests[0].timeout, Duration::from_secs(5));
    assert_eq!(host.requests[1].timeout, Duration::from_secs(30));
    assert_eq!(
        host.requests[1].command,
        [
            "/fake/claude",
            "-p",
            "--safe-mode",
            "--no-session-persistence",
            "--permission-mode",
            "plan",
            "--tools",
            "",
            "--model",
            "opus",
            "--effort",
            "medium",
            "--output-format",
            "json",
            "Reply exactly OK."
        ]
    );
    assert_eq!(writer.files.len(), 1);
    assert!(writer.files[0].2);
    assert!(!writer.files[0].1.contains("secret@example.com"));
    assert!(!writer.files[0].1.contains("secret-org"));
}

#[test]
fn claude_invalid_auth_stops_before_running_the_canary() {
    let mut host = FakeHost::with_results([process_result(
        0,
        r#"{"loggedIn":true,"authMethod":"apiKey","subscriptionType":"pro"}"#,
        "",
        false,
    )]);
    let mut writer = FakeWriter::default();
    let argv = [
        "claude".into(),
        "--out".into(),
        "snapshot.json".into(),
        "--alias".into(),
        "opus".into(),
        "--effort".into(),
        "medium".into(),
    ];

    assert_eq!(run(&argv, &mut host, &mut writer, &context()).unwrap(), 1);
    assert_eq!(host.requests.len(), 1);
    assert!(writer
        .stderr
        .contains("Claude subscription authentication is unavailable"));
}

#[test]
fn process_failures_preserve_timeouts_and_scrub_private_policy_details() {
    let mut host = FakeHost::with_results([process_result(
        1,
        "Your organization has disabled Claude subscription access; contact private@example.test in private-org",
        "",
        false,
    )]);
    let mut writer = FakeWriter::default();
    assert_eq!(
        run(
            &[
                "claude".into(),
                "--out".into(),
                "out".into(),
                "--alias".into(),
                "opus".into(),
                "--effort".into(),
                "medium".into()
            ],
            &mut host,
            &mut writer,
            &context(),
        )
        .unwrap(),
        1
    );
    assert!(writer
        .stderr
        .contains("provider access denied; subscription access disabled by organisation policy"));
    assert!(!writer.stderr.contains("private@example.test"));
    assert!(!writer.stderr.contains("private-org"));
    assert!(writer.files.is_empty());

    let mut host = FakeHost::with_results([process_result(0, "partial", " timeout detail ", true)]);
    let mut writer = FakeWriter::default();
    assert_eq!(
        run(&["agy".into()], &mut host, &mut writer, &context()).unwrap(),
        1
    );
    assert!(writer
        .stderr
        .contains("agy models timed out after 60 seconds: timeout detail"));
}

#[test]
fn scrubbed_failure_details_keep_precedence_and_http_status_filtering() {
    assert_eq!(
        scrubbed_failure_detail("provider returned HTTP 403", ""),
        "provider returned HTTP 403"
    );
    assert_eq!(
        scrubbed_failure_detail("404 then 429 and later 401", ""),
        "provider returned HTTP 429"
    );
    assert_eq!(scrubbed_failure_detail("status 404", ""), "");
    assert_eq!(
        scrubbed_failure_detail("HTTP 403", "  raw provider detail  "),
        "raw provider detail"
    );
}

#[test]
fn claude_effort_rejection_warning_is_a_typed_discovery_failure() {
    let mut host = FakeHost::with_results([process_result(
        0,
        r#"{"ok":true}"#,
        "Warning: Unknown --effort value 'medium'",
        false,
    )]);
    let error = claude_json(
        &mut host,
        vec!["claude".into()],
        Duration::from_secs(2),
        Path::new("/workspace"),
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Claude CLI rejected the requested effort"
    );
}

#[test]
fn duplicate_json_is_rejected_and_successful_stderr_is_only_a_warning() {
    let mut host = FakeHost::with_results([process_result(
        0,
        r#"{"models":[{"slug":"gpt-a","slug":"gpt-b","supported_reasoning_levels":[{"effort":"high"}]}]}"#,
        " harmless warning \n",
        false,
    )]);
    let mut writer = FakeWriter::default();
    assert_eq!(
        run(
            &["codex".into(), "--out".into(), "out.json".into()],
            &mut host,
            &mut writer,
            &context()
        )
        .unwrap(),
        1
    );
    assert!(writer
        .stderr
        .contains("capability discovery failed: duplicate JSON member: slug"));
    assert!(!writer.stderr.contains("harmless warning"));
    assert!(writer.files.is_empty());
}

#[test]
fn provider_json_keeps_checked_number_range_errors_typed() {
    let error = load_provider_json(r#"{"ignored":18446744073709551616}"#).unwrap_err();
    assert!(matches!(
        error,
        CapabilityError::Codec(CheckedJsonError::NumberOutOfRange(_))
    ));
}

#[test]
fn malformed_provider_json_diagnostic_difference_is_explicit() {
    let raw = r#"{"models": [}"#;
    let Some(python_error): Option<String> = crate::oracle::python(
        r#"
import json,pathlib,sys
sys.path.insert(0,str(pathlib.Path('skills/orchestrate/scripts').resolve()))
import capabilities as m
try:
 error=m.load_json(json.load(sys.stdin))
except Exception as e:
 print(json.dumps(f'{type(e).__name__}: {e}'))
else:
 print(json.dumps(repr(error)))
"#,
        &json!(raw),
    ) else {
        return;
    };
    let rust_error = load_provider_json(raw).unwrap_err().to_string();

    // records::checked_json currently exposes serde_json syntax wording rather
    // than CPython JSONDecodeError wording. Keep this source incompatibility
    // visible until the shared owner offers a faithful diagnostic formatter.
    assert_ne!(rust_error, python_error);
    assert!(python_error.starts_with("JSONDecodeError:"));
    assert!(!rust_error.contains("JSONDecodeError"));
}

#[test]
fn unused_nan_codex_field_exposes_python_json_extended_constant_gap() {
    let raw = r#"{"unused":NaN,"models":[{"slug":"gpt-6-sol","supported_reasoning_levels":[{"effort":"high"}]}]}"#;
    let Some(python_result): Option<Value> = crate::oracle::python(
        r#"
import json,pathlib,sys
sys.path.insert(0,str(pathlib.Path('skills/orchestrate/scripts').resolve()))
import capabilities as m
m.observed_at=lambda:'2026-10-23T12:00:00Z'
raw=json.load(sys.stdin)
try:
 print(json.dumps(['ok',m.normalise_codex(m.load_json(raw))]))
except Exception as e:
 print(json.dumps(['error',type(e).__name__,str(e)]))
"#,
        &json!(raw),
    ) else {
        return;
    };
    let rust_result = match load_provider_json(raw) {
        Ok(value) => match normalise_codex(&value, OBSERVED) {
            Ok(snapshot) => json!(["ok", snapshot]),
            Err(error) => json!(["error", error.to_string()]),
        },
        Err(CapabilityError::Codec(error)) => json!(["error", error.to_string()]),
        Err(error) => json!(["error", error.to_string()]),
    };

    // Python's json.loads accepts NaN by default, even in an unused field;
    // checked_json intentionally accepts standards-compliant JSON only.
    assert_eq!(python_result[0], "ok");
    assert_eq!(rust_result[0], "error");
    assert_ne!(rust_result, python_result);
}

#[test]
fn sorted_indented_json_matches_python_ascii_escaping_and_key_order() {
    let value = ordered(json!({"z": "😀", "a": {"é": "x\n\"", "b": true}}));
    assert_eq!(
        python_json_dumps(&value),
        "{\n  \"a\": {\n    \"b\": true,\n    \"\\u00e9\": \"x\\n\\\"\"\n  },\n  \"z\": \"\\ud83d\\ude00\"\n}\n"
    );
}

#[test]
fn normalizers_match_python_policy_for_success_and_rejection_cases() {
    let cases = json!({
        "observed_at": OBSERVED,
        "codex": [
            {"models":[{"slug":"gpt-6-sol","supported_reasoning_levels":[{"effort":"HIGH"},{"effort":"max"}]},{"slug":"gpt-6-luna","supported_reasoning_levels":[{"effort":"max"}]}]},
            {"models":[{"slug":"GPT-Duplicate","supported_reasoning_levels":[{"effort":"high"}]},{"slug":"gpt-duplicate","supported_reasoning_levels":[{"effort":"max"}]}]},
            {"models":[{"slug":"valid","supported_reasoning_levels":[{"effort":"high"}]}, "malformed"]},
            {},
            {"models":[]}
        ],
        "agy": [
            "gemini-3.8-flash-high\ngemini-3.8-flash-medium\ngemini-3.8-flash-low\nclaude-opus-4-6-thinking\n",
            "claude-opus-4-6\n",
            " \t\n",
            "gemini-3.7-flash-high\tGemini 3.7 Flash (High)\u{0085}gemini-3.7-flash-low\u{2028}"
        ],
        "claude": [
            {"auth":{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"pro","secret":"omit"},"result":{"type":"result","subtype":"success","is_error":false,"result":"OK","modelUsage":{"claude-haiku-4-5-20251001":{},"claude-opus-4-8":{}}},"alias":"opus","effort":"medium"},
            {"auth":{"loggedIn":1,"authMethod":"claude.ai","subscriptionType":"pro"},"result":{"type":"result","subtype":"success","is_error":false,"result":"OK","modelUsage":{"claude-opus-4-8":{}}},"alias":"opus","effort":"medium"},
            {"auth":{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"pro"},"result":{"type":"result","subtype":"success","is_error":true,"result":"OK","modelUsage":{"claude-opus-4-8":{}}},"alias":"opus","effort":"medium"},
            {"auth":{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"pro"},"result":{"type":"result","subtype":"success","is_error":false,"result":"OK","modelUsage":{"claude-opus-4-8":{},"claude-opus-4-9":{}}},"alias":"opus","effort":"medium"},
            {"auth":{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"pro"},"result":{"type":"result","subtype":"success","is_error":false,"result":"OK","modelUsage":{"claude-fable-5-0":{}}},"alias":"claude-fable-5-1","effort":"medium"}
        ]
    });
    let Some(expected): Option<Value> = crate::oracle::python(
        r#"
import json,pathlib,sys
from types import SimpleNamespace
sys.path.insert(0,str(pathlib.Path('skills/orchestrate/scripts').resolve()))
import capabilities as m
d=json.load(sys.stdin)
m.observed_at=lambda:d['observed_at']
def answer(f):
 try:return ['ok',f()]
 except ValueError as e:return ['error',str(e)]
out={'codex':[],'agy':[],'claude':[]}
for raw in d['codex']:out['codex'].append(answer(lambda raw=raw:m.normalise_codex(raw)))
for raw in d['agy']:out['agy'].append(answer(lambda raw=raw:m.normalise_agy(raw)))
for c in d['claude']:
 responses=[m.Probe(json.dumps(c['auth']),''),m.Probe(json.dumps(c['result']), '')]
 m.run_cli=lambda *args,**kwargs:responses.pop(0)
 out['claude'].append(answer(lambda c=c:m.discover_claude('claude',SimpleNamespace(alias=c['alias'],effort=c['effort']))[0]))
print(json.dumps(out))
"#,
        &cases,
    )
    .expect("oracle should be available for the differential test") else {
        return;
    };

    let codex = cases["codex"]
        .as_array()
        .unwrap()
        .iter()
        .map(
            |raw| match normalise_codex(&ordered(raw.clone()), OBSERVED) {
                Ok(value) => json!(["ok", value]),
                Err(CapabilityError::Discovery(message)) => json!(["error", message]),
                Err(error) => json!(["error", error.to_string()]),
            },
        )
        .collect::<Vec<_>>();
    let agy = cases["agy"]
        .as_array()
        .unwrap()
        .iter()
        .map(|raw| match normalise_agy(raw.as_str().unwrap(), OBSERVED) {
            Ok(value) => json!(["ok", value]),
            Err(CapabilityError::Discovery(message)) => json!(["error", message]),
            Err(error) => json!(["error", error.to_string()]),
        })
        .collect::<Vec<_>>();
    let claude = cases["claude"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            match normalise_claude(
                &ordered(c["auth"].clone()),
                &ordered(c["result"].clone()),
                c["alias"].as_str().unwrap(),
                c["effort"].as_str().unwrap(),
                OBSERVED,
            ) {
                Ok(value) => json!(["ok", value]),
                Err(CapabilityError::Discovery(message)) => json!(["error", message]),
                Err(error) => json!(["error", error.to_string()]),
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(json!({"codex":codex,"agy":agy,"claude":claude}), expected);
}
