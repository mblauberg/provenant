use super::*;
fn shell(code: &str) -> Vec<OsString> {
    vec!["/bin/sh".into(), "-c".into(), code.into()]
}
#[test]
fn streams_are_independent_byte_exact_and_forwarded_after_the_capture_cap() {
    let args = shell("printf 'a\\000b\\377'; printf 'err\\r\\n' >&2; exit 7");
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut options = ExecOptions::new(&args, Path::new("."), Duration::from_secs(3));
    options.stdout_limit = 3;
    options.stderr_limit = 2;
    options.stdout_sink = Some(&mut out);
    options.stderr_sink = Some(&mut err);
    let actual = SystemExecutor.execute(options).unwrap();
    assert_eq!(actual.termination, Termination::Exit(7));
    assert_eq!(actual.stdout.bytes, b"a\0b");
    assert_eq!(actual.stdout.total_bytes, 4);
    assert_eq!(actual.stderr.bytes, b"er");
    assert_eq!(actual.stderr.total_bytes, 5);
    assert!(actual.stdout.truncated && actual.stderr.truncated);
    assert_eq!(out, b"a\0b\xff");
    assert_eq!(err, b"err\r\n");
    assert!(!actual.timed_out);
}
#[test]
fn pumps_large_stdin_and_both_outputs_without_pipe_deadlock() {
    let args = shell("/bin/cat; printf done >&2");
    let bytes = vec![0x9a; 256 * 1024];
    let mut options = ExecOptions::new(&args, Path::new("."), Duration::from_secs(5));
    options.stdin = Some(&bytes);
    options.stdout_limit = bytes.len();
    let result = run(options).unwrap();
    assert_eq!(result.stdout.bytes, bytes);
    assert_eq!(result.stderr.bytes, b"done");
    assert_eq!(result.termination, Termination::Exit(0));
    assert!(!result.timed_out);
}
#[test]
fn env_cwd_empty_argv_spawn_and_strict_utf8_errors() {
    let root = tempfile::tempdir().unwrap();
    let args = shell("printf '%s' \"$KEPT\"; test -f fixture");
    std::fs::write(root.path().join("fixture"), b"").unwrap();
    let env = [("KEPT".into(), "value".into())];
    let mut options = ExecOptions::new(&args, root.path(), Duration::from_secs(3));
    options.env = Some(&env);
    let result = run_text(&SystemExecutor, options).unwrap();
    assert_eq!(result.stdout, "value");
    assert_eq!(result.termination, Termination::Exit(0));
    assert!(matches!(
        run(ExecOptions::new(&[], root.path(), Duration::ZERO)),
        Err(ExecError::EmptyArgv)
    ));
    assert!(matches!(
        run(ExecOptions::new(
            &["/no/such/program".into()],
            root.path(),
            Duration::ZERO
        )),
        Err(ExecError::Io {
            operation: "spawn",
            ..
        })
    ));
    for (code, stream) in [
        ("printf '\\377'", "stdout"),
        ("printf '\\377' >&2", "stderr"),
    ] {
        assert!(
            matches!(run_text(&SystemExecutor, ExecOptions::new(&shell(code), root.path(), Duration::from_secs(3))), Err(TextError::Utf8 { stream: actual, .. }) if actual == stream)
        );
    }
}
#[test]
fn timeout_escalates_term_ignoring_direct_child_and_returns_signal() {
    let args = shell("trap '' TERM; printf ready; while :; do :; done");
    let mut options = ExecOptions::new(&args, Path::new("."), Duration::from_secs(1));
    options.terminate_grace = Duration::from_millis(20);
    let result = run(options).unwrap();
    assert!(result.timed_out);
    assert_eq!(result.stdout.bytes, b"ready");
    assert_eq!(result.termination, Termination::Signal(9));
}
#[test]
fn stdout_sink_failure_is_typed_and_child_is_cleaned() {
    struct Fail;
    impl Write for Fail {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let args = shell("printf data; while :; do :; done");
    let mut fail = Fail;
    let mut options = ExecOptions::new(&args, Path::new("."), Duration::from_secs(2));
    options.stdout_sink = Some(&mut fail);
    assert!(matches!(
        run(options),
        Err(ExecError::Io {
            operation: "forward",
            ..
        })
    ));
}

#[test]
fn differential_raw_bytes_match_python_success_failure_and_stdin() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let script = r#"
import json, subprocess
cases = [("printf 'a\\000b\\377'; printf 'err\\r\\n' >&2; exit 7", None),
         ("/bin/cat", bytes(range(256))*1024),
         ("printf before; kill -TERM $$", None)]
results=[]
for code, data in cases:
    p=subprocess.run(['/bin/sh','-c',code],input=data,stdout=subprocess.PIPE,stderr=subprocess.PIPE,env={'KEPT':'x'},timeout=5)
    results.append({'code':p.returncode,'stdout':list(p.stdout[:32]),'stderr':list(p.stderr[:32]),'out_bytes':len(p.stdout),'err_bytes':len(p.stderr)})
print(json.dumps(results))
"#;
    let Some(expected) = crate::test_support::python(&root, script) else {
        return;
    };
    let input: Vec<_> = (0..256 * 1024).map(|n| (n % 256) as u8).collect();
    let env = [("KEPT".into(), "x".into())];
    let actual: Vec<_> = [
        (
            "printf 'a\\000b\\377'; printf 'err\\r\\n' >&2; exit 7",
            None,
        ),
        ("/bin/cat", Some(input.as_slice())),
        ("printf before; kill -TERM $$", None),
    ]
    .into_iter()
    .map(|(code, input)| {
        let args = shell(code);
        let mut options = ExecOptions::new(&args, &root, Duration::from_secs(5));
        options.env = Some(&env);
        options.stdin = input;
        options.stdout_limit = 32;
        options.stderr_limit = 32;
        let result = run(options).unwrap();
        let code = match result.termination {
            Termination::Exit(code) => code,
            Termination::Signal(signal) => -signal,
        };
        serde_json::json!({"code":code,"stdout":result.stdout.bytes,"stderr":result.stderr.bytes,
                "out_bytes":result.stdout.total_bytes,"err_bytes":result.stderr.total_bytes})
    })
    .collect();
    assert_eq!(serde_json::json!(actual), expected);
}
#[test]
fn discard_stream_does_not_trigger_output_limit_stop_or_capture_bytes() {
    let args = shell("printf retained; /usr/bin/head -c 1048577 /dev/zero >&2; exit 7");
    let mut options = ExecOptions::new(&args, Path::new("."), Duration::from_secs(5));
    options.discard_stderr = true;
    options.stderr_limit = 0;
    options.stop_on_output_limit = true;
    let output = run(options).unwrap();
    assert_eq!(output.termination, Termination::Exit(7));
    assert_eq!(output.stdout.bytes, b"retained");
    assert_eq!(output.stderr, CapturedStream::new());
}

#[test]
fn completed_child_cannot_bypass_an_already_expired_deadline() {
    let args = shell("exit 0");
    let mut options = ExecOptions::new(&args, Path::new("."), Duration::ZERO);
    options.discard_stdout = true;
    options.discard_stderr = true;
    assert!(run(options).unwrap().timed_out);
}
