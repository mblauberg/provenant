//! Real dispatch composition with isolated, synthetic provider processes.
#![allow(clippy::disallowed_methods)]
mod support;
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    process::ExitCode,
};
use support::{Result, TestRig, code};

const HANDOFF_SECRET: &str = "sk-abcdefghijklmnopqrstuvwx";

#[cfg(target_os = "macos")]
fn agy_lease_fixture() -> Result {
    let args: Vec<_> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--version") {
        io::stdout().write_all(b"agy 1.0.0\n")?;
    } else if args.iter().any(|arg| arg == "/usage") {
        let log = args
            .windows(2)
            .find(|pair| pair[0] == "--log-file")
            .ok_or("log argument")?;
        fs::write(
            &log[1],
            "OAuth: authenticated successfully as fixture@example.test\n",
        )?;
        io::stdout().write_all(b"{\"status\":\"SUCCESS\"}\n")?;
    } else {
        use std::os::unix::fs::MetadataExt;
        let prompt = args
            .windows(2)
            .find(|pair| pair[0] == "--print")
            .ok_or("prompt argument")?;
        let marker = prompt[1]
            .split_once("report lease status ")
            .ok_or("identity marker")?
            .1;
        let values: Vec<u64> = marker
            .split_whitespace()
            .take(2)
            .filter_map(|s| s.parse().ok())
            .collect();
        let [_device, inode] = values.as_slice() else {
            return Err("lock identity missing".into());
        };
        let mut inspected = 0;
        for fd in 3..1024 {
            if let Ok(metadata) = fs::metadata(format!("/dev/fd/{fd}")) {
                inspected += 1;
                assert_ne!(metadata.ino(), *inode, "provider inherited update lock");
            }
        }
        assert!(inspected > 0, "provider descriptors could not be inspected");
        io::stdout().write_all(
            br#"{"event":"init","conversation_id":"fixture-session","init":{"model":"gemini-3.8-flash"}}"#,
        )?;
        io::stdout().write_all(b"\n")?;
        let frame = json!({"event":"result","result":{"conversation_id":"fixture-session","status":"SUCCESS","response":"## Findings\nlease-free provider started\n## Evidence\nNo descriptor matched the update lock.","error":null,"usage":{"input_tokens":0,"output_tokens":0}}});
        io::stdout().write_all(format!("{frame}\n").as_bytes())?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn dispatch_proceeds_when_provider_update_is_busy() -> Result {
    let mut rig = TestRig::new()?;
    let _cleanup = rig.process_cleanup();
    let bin = rig.root.join("provider/bin");
    fs::create_dir_all(&bin)?;
    rig.copy_executable(&std::env::current_exe()?, &bin.join("agy"))?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    let keychain = rig.home.join("Library/Keychains/login.keychain-db");
    fs::create_dir_all(keychain.parent().ok_or("keychain parent")?)?;
    fs::write(keychain, "fixture")?;
    let lock = provenant_os::provider_fence::acquire(
        &rig.state,
        "agy",
        provenant_os::fs::LockMode::Exclusive,
    )?;
    let prompt = rig.root.join("prompt.txt");
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::metadata(rig.state.join("provider-updates/agy.lock"))?;
    fs::write(
        &prompt,
        format!("report lease status {} {}", metadata.dev(), metadata.ino()),
    )?;
    let result = rig.command(&[
        "run",
        "--model",
        "agy/gemini-3.8-flash",
        "--operation-id",
        "busy-provider",
        "--prompt-file",
        prompt.to_str().ok_or("prompt")?,
        "--wait",
        "1s",
        "--json",
    ])?;
    assert_eq!(code(&result), 0, "{result:?}");
    let response: Value = serde_json::from_slice(&result.stdout.bytes)?;
    let run = response["data"]["run_id"].as_str().ok_or("run id")?;
    let lanes = rig.command(&["lanes", "--id", run, "--wait", "5s", "--json"])?;
    assert_eq!(code(&lanes), 0, "{lanes:?}");
    let output = rig.command(&["output", run, "--json"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout.bytes).contains("lease-free provider started"),
        "{output:?}"
    );
    drop(lock);
    Ok(())
}

fn handoff_recovery_provider(prompt: &str, args: &[String], auth: &Value) -> Result {
    let account = auth["tokens"]["account_id"]
        .as_str()
        .ok_or("fixture account")?;
    let session = "01234567-89ab-cdef-0123-456789abcdef";
    writeln!(
        io::stdout(),
        "{}",
        json!({"type":"thread.started","thread_id":session})
    )?;
    if !prompt.contains("Previous attempt handoff") {
        let path = prompt
            .lines()
            .find_map(|line| {
                line.strip_prefix("Progress handoff JSON: ")?
                    .split_once(". Update this file atomically")
                    .map(|(path, _)| PathBuf::from(path))
            })
            .ok_or("supplied progress path")?;
        let progress = json!({
            "schema_version":2,"status":"stopped_early",
            "note":format!("Parser written.\n{HANDOFF_SECRET}\t{HANDOFF_SECRET}"),
            "remaining":[format!("Finish renderer.\n{HANDOFF_SECRET}"),format!("Check output.\t{HANDOFF_SECRET}")],
            "decisions":[format!("Keep parser API.\t{HANDOFF_SECRET}")],
            "verify":[format!("Parser fixture passed.\n{HANDOFF_SECRET}")],
            "future":{"nested":[format!("\n{HANDOFF_SECRET}"),{"token":format!("\t{HANDOFF_SECRET}")}]}
        });
        fs::write("partial-work.txt", "parser implementation survives")?;
        fs::write("first-account.txt", account)?;
        fs::write(
            "first-worktree.txt",
            std::env::current_dir()?.as_os_str().as_encoded_bytes(),
        )?;
        let staging = path.with_extension("next");
        fs::write(&staging, serde_json::to_vec(&progress)?)?;
        fs::rename(staging, path)?;
        writeln!(
            io::stdout(),
            "{}",
            json!({"type":"item.completed","item":{"type":"agent_message","text":"```handoff\n{\"schema_version\":2,"}})
        )?;
        writeln!(
            io::stdout(),
            "{}",
            json!({"type":"turn.failed","error":{"message":"usage limit reached","codex_error_info":"usage_limit_exceeded"}})
        )?;
    } else {
        assert!(!args.iter().any(|arg| arg == "resume"));
        assert_ne!(fs::read_to_string("first-account.txt")?, account);
        assert_eq!(
            fs::read("first-worktree.txt")?,
            std::env::current_dir()?.as_os_str().as_encoded_bytes()
        );
        assert_eq!(
            fs::read_to_string("partial-work.txt")?,
            "parser implementation survives"
        );
        assert!(!prompt.contains(HANDOFF_SECRET));
        for text in [
            "fixture handoff recovery",
            "Finish renderer.",
            "Keep parser API.",
            "Parser fixture passed.",
            "[REDACTED]",
        ] {
            assert!(
                prompt.contains(text),
                "missing continuation context: {text}"
            );
        }
        fs::write("successor-context.txt", prompt)?;
        fs::write("second-account.txt", account)?;
        writeln!(
            io::stdout(),
            "{}",
            json!({"type":"item.completed","item":{"type":"agent_message","text":"Recovered parser; renderer verified."}})
        )?;
        writeln!(
            io::stdout(),
            "{}",
            json!({"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}})
        )?;
    }
    Ok(())
}

/// Fake answers close with a done handoff, as every lane brief asks (#2707).
const DONE_FENCE: &str = "\n```handoff\n{\"schema_version\":2,\"status\":\"done\",\"note\":\"finished\",\"remaining\":[],\"decisions\":[],\"verify\":[]}\n```";

/// An upstream answer without the fake's closing handoff.
fn without_done(text: &str) -> &str {
    text.strip_suffix(DONE_FENCE).unwrap_or(text)
}

fn fixture() -> Result {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|v| v == "--version") {
        let version = if std::path::Path::new(&args[0]).file_name()
            == Some(std::ffi::OsStr::new("opencode"))
        {
            "opencode v2.2.0"
        } else {
            "codex-cli 0.159.0"
        };
        writeln!(io::stdout(), "{version}")?;
        return Ok(());
    }
    if args.get(1).is_some_and(|v| v == "auth") {
        writeln!(
            io::stdout(),
            "{}",
            json!({"loggedIn":true,"email":"fixture@example.test"})
        )?;
        return Ok(());
    }
    if args.get(1).is_some_and(|v| v == "run") {
        let mut prompt = String::new();
        io::stdin().read_to_string(&mut prompt)?;
        if prompt.contains("fixture owner lifetime probe") {
            // Keep the synthetic provider alive while its owner's lifetime is tested.
            #[allow(clippy::disallowed_methods)] // Bound fixture lifetime if cleanup fails.
            std::thread::park_timeout(std::time::Duration::from_secs(120));
        }
        if prompt.contains("stderr refusal then hang") {
            write!(
                io::stderr(),
                "\x1b[31mAI.Error.QuotaExceeded: Go usage limit exceeded retryAfterMs=60000"
            )?;
            io::stderr().flush()?;
            #[allow(clippy::disallowed_methods)] // Bound fixture lifetime if cleanup fails.
            std::thread::park_timeout(std::time::Duration::from_secs(120));
        }
        if prompt.contains("stderr quota mention") {
            writeln!(
                io::stderr(),
                "The documentation mentions quota and usage limit exceeded"
            )?;
            io::stderr().flush()?;
        }
        let frames = if prompt.contains("free limit") {
            include_str!("fixtures/providers/opencode/v2.2.0-free-limit.jsonl")
        } else if prompt.contains("model unavailable") {
            include_str!("fixtures/providers/opencode/model-unavailable.jsonl")
        } else {
            include_str!("fixtures/providers/opencode/v2.2.0-success.jsonl")
        };
        // Restricted routes must supply the sections requested at launch.
        let frames = frames.replace(
            r#""text":"OK""#,
            r###""text":"## Findings\nOK\n## Evidence\nFixture checked.\n```handoff\n{\"schema_version\":2,\"status\":\"done\",\"note\":\"finished\",\"remaining\":[],\"decisions\":[],\"verify\":[]}\n```""###,
        );
        io::stdout().write_all(frames.as_bytes())?;
        io::stdout().flush()?;
        if prompt.contains("free limit then hang") {
            // Keep the process and its pipes live until the guardian stops it.
            #[allow(clippy::disallowed_methods)] // Bound fixture lifetime if cleanup fails.
            std::thread::park_timeout(std::time::Duration::from_secs(120));
        }
        return Ok(());
    }
    if args.iter().skip(1).any(|v| v == "-p") {
        let dir = PathBuf::from(std::env::var_os("CLAUDE_CONFIG_DIR").ok_or("CLAUDE_CONFIG_DIR")?);
        // Shell lanes get a private config directory; others use the account's.
        assert!(matches!(
            dir.file_name().and_then(|s| s.to_str()),
            Some(".claude-pro" | "claude-config")
        ));
        if args.iter().any(|v| v == "provenant-refresh-probe") {
            let expiry = u64::try_from(
                provenant_core::Clock::wall_now(&provenant_os::clock::SystemClock::new())?
                    .unix_millis(),
            )?
            .saturating_add(3_600_000);
            fs::write(
                dir.join(".credentials.json"),
                serde_json::to_vec(
                    &json!({"claudeAiOauth":{"accessToken":"credential-sentinel-2719-violet-fox","expiresAt":expiry}}),
                )?,
            )?;
            fs::write(dir.join("refreshed"), b"yes")?;
            return Ok(());
        }
        assert!(std::env::var_os("CLAUDE_CODE_OAUTH_TOKEN").is_none());
        let fd: i32 = std::env::var("CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR")?.parse()?;
        let token = provenant_os::sys::fixture_read_secret_pipe(fd)?;
        assert_eq!(token, b"credential-sentinel-2719-violet-fox");
        let mut prompt = String::new();
        io::stdin().read_to_string(&mut prompt)?;
        if prompt.contains("fixture host-native") {
            let argument = |flag| {
                args.windows(2)
                    .find(|pair| pair[0] == flag)
                    .map(|pair| &pair[1])
            };
            assert_eq!(argument("--agent").map(String::as_str), Some("ci-fixer"));
            let agents: Value =
                serde_json::from_str(argument("--agents").ok_or("agent definition")?)?;
            assert_eq!(agents["ci-fixer"]["prompt"], "Repair failing CI checks.");
            assert_eq!(argument("--effort").map(String::as_str), Some("medium"));
        }
        assert_eq!(
            std::env::var("CLAUDE_CODE_AUTO_COMPACT_WINDOW")?.as_str(),
            "275000"
        );
        let answer = if prompt.contains("fixture-council-overcap") {
            if prompt.contains("Verify the lens findings for:")
                || prompt.contains("Reconcile independent review findings against evidence for:")
            {
                assert!(prompt.contains("composite findings truncated:"));
                let entries: Vec<_> = prompt
                    .lines()
                    .filter_map(|line| {
                        let source = line
                            .strip_prefix("composite findings truncated: full member result: ")?;
                        let source: Value = serde_json::from_str(source).ok()?;
                        let label = source["label"].as_str()?.strip_prefix("lenses--")?;
                        Some((label.to_owned(), PathBuf::from(source["path"].as_str()?)))
                    })
                    .collect();
                assert!(entries.len() >= 4, "{prompt}");
                let mut count = 0;
                let mut dispositions = String::new();
                let mut structured = Vec::new();
                for (label, path) in entries {
                    let data = fs::read_to_string(path)?;
                    let block = data
                        .split_once("```findings\n")
                        .ok_or("findings block")?
                        .1
                        .split_once("```")
                        .ok_or("findings end")?
                        .0;
                    let value: Value = serde_json::from_str(block)?;
                    for finding in value["findings"].as_array().ok_or("findings array")? {
                        let id = format!(
                            "lenses--{label}:{}",
                            finding["id"].as_str().ok_or("finding id")?
                        );
                        let status = if prompt.contains("Verify the lens findings for:") {
                            "falsified"
                        } else {
                            "rejected"
                        };
                        dispositions.push_str(&format!(
                            "{id}: rejected; evidence refutes it\nDISPOSITION {}\n",
                            json!({"id":id,"disposition":status,"reason":"evidence refutes it"})
                        ));
                        if structured.len() < 32 {
                            let mut finding = json!({"id":id,"severity":"high","file":"a.rs","summary":"fixture finding","status":"open","verification":"falsified","disposition":"rejected","reason":"evidence refutes it"});
                            if prompt.contains("Verify the lens findings for:") {
                                finding["propagates"] = json!(format!("lenses:{id}"));
                            }
                            structured.push(finding);
                        }
                        count += 1;
                    }
                }
                assert!(count > 32, "{count}");
                format!(
                    "read {count} findings from complete lens results\n{dispositions}```findings\n{}\n```\nVERDICT: PASS\n",
                    json!({"schema_version":1,"findings":structured})
                )
            } else if prompt.contains("Lens:") {
                let findings: Vec<_> = (0..10).map(|i| json!({"id":format!("F{i}"),"severity":"high","file":"a.rs","summary":"fixture finding","status":"open"})).collect();
                format!(
                    "```findings\n{}\n```\nVERDICT: PASS\n",
                    json!({"schema_version":1,"findings":findings})
                )
            } else {
                "subject framed\nVERDICT: PASS\n".to_owned()
            }
        } else if prompt.contains("fixture large result") {
            // Far past the 4 KiB excerpt a consuming node's prompt carries.
            let mut text: String = (0..2000)
                .map(|n| format!("finding {n:04} body\n"))
                .collect();
            text.push_str("END-OF-LARGE-RESULT");
            text
        } else if let Some(name) = prompt
            .lines()
            .find_map(|line| line.strip_prefix("fixture case result "))
        {
            format!("case-result-{name}")
        } else if prompt.contains("fixture read upstream file") {
            // The prompt lists each materialised file as `- label: PATH (N bytes)`.
            let path = prompt
                .lines()
                .skip_while(|line| *line != "<UPSTREAM_FILES>")
                .find_map(|line| line.strip_prefix("- producer: "))
                .and_then(|line| line.split_once(" ("))
                .map(|(path, _)| PathBuf::from(path))
                .ok_or("upstream file entry")?;
            let text = fs::read_to_string(&path)?;
            let text = without_done(&text);
            let sibling = path.with_file_name("other.md");
            // Upstreams whose ids differ only by case each keep their own file.
            let mut cases = String::new();
            for label in ["Review", "review"] {
                let entry = format!("- {label}: ");
                let path = prompt
                    .lines()
                    .skip_while(|line| *line != "<UPSTREAM_FILES>")
                    .find_map(|line| line.strip_prefix(entry.as_str()))
                    .and_then(|line| line.split_once(" ("))
                    .map(|(path, _)| PathBuf::from(path))
                    .ok_or("case upstream file entry")?;
                cases.push_str(&format!(
                    "; {label}={}",
                    without_done(&fs::read_to_string(path)?)
                ));
            }
            format!(
                "read {} bytes; tail {}; sibling write denied {}; parent listing denied {}{cases}",
                text.len(),
                text.lines().last().unwrap_or_default(),
                fs::write(&sibling, b"x").is_err(),
                fs::read_dir(path.parent().and_then(|p| p.parent()).ok_or("parent")?).is_err(),
            )
        } else {
            "OK".to_owned()
        };
        let answer = format!("{answer}{DONE_FENCE}");
        for frame in [
            json!({"type":"system","subtype":"init","session_id":"fixture-session"}),
            json!({"type":"assistant","session_id":"fixture-session","parent_tool_use_id":null,"message":{"model":"claude-sonnet-5-5","role":"assistant","content":[{"type":"text","text":answer}],"usage":{"input_tokens":1,"output_tokens":1}}}),
            json!({"type":"result","session_id":"fixture-session","subtype":"success","is_error":false,"result":answer}),
        ] {
            writeln!(io::stdout(), "{frame}")?;
        }
        return Ok(());
    }
    let mut prompt = String::new();
    io::stdin().read_to_string(&mut prompt)?;
    if prompt.contains("fixture slow result") {
        std::thread::park_timeout(std::time::Duration::from_secs(3));
    }
    let home = PathBuf::from(std::env::var_os("CODEX_HOME").ok_or("CODEX_HOME")?);
    let auth: Value = serde_json::from_slice(&fs::read(home.join("auth.json"))?)?;
    if !prompt.contains("fixture handoff recovery") && !prompt.contains("fixture graph redraw") {
        assert_eq!(auth["tokens"]["account_id"], "fixture-one");
    }
    let session = "01234567-89ab-cdef-0123-456789abcdef";
    let sessions = home.join("sessions/2026/10/24");
    fs::create_dir_all(&sessions)?;
    let recorded_session = if prompt.contains("mismatch") {
        "other-thread"
    } else {
        session
    };
    let policy_model = args
        .windows(2)
        .find(|pair| pair[0] == "-m")
        .map(|pair| pair[1].as_str());
    if !prompt.contains("fixture missing rollout") {
        // Real rollouts record tool output. A printed credential must be
        // scrubbed from this transcript at settlement, not only from captures.
        let recorded = if prompt.contains("fixture printed credential") {
            "printed ghp_abcdefghijklmnopqrstuvwxyz0123456789"
        } else {
            "must not be retained"
        };
        fs::write(
            sessions.join(format!("rollout-2026-10-24T00-00-00-{session}.jsonl")),
            format!(
                "{}\n{}\n{}\n",
                json!({"type":"session_meta","payload":{"id":recorded_session}}),
                json!({"type":"turn_context","payload":{"model": if prompt.contains("fixture policy result") || prompt.contains("fixture policy continuation") || prompt.contains("fixture graph redraw") { policy_model.unwrap_or("gpt-6-luna") } else if prompt.contains("fixture pool result") { "gpt-6.1-sol" } else { "gpt-6-luna" },"effort":"low"}}),
                json!({"type":"response_item","payload":{"content":recorded}})
            ),
        )?;
    }
    if prompt.contains("fixture handoff recovery") {
        return handoff_recovery_provider(&prompt, &args, &auth);
    }
    if prompt.contains("fixture graph redraw") && policy_model == Some("gpt-6-luna") {
        writeln!(
            io::stdout(),
            "{}",
            json!({"type":"thread.started","thread_id":session})
        )?;
        writeln!(
            io::stdout(),
            "{}",
            json!({"type":"turn.failed","error":{"message":"usage limit reached","codex_error_info":"usage_limit_exceeded"}})
        )?;
        return Ok(());
    }
    if prompt.contains("fixture capacity") {
        writeln!(
            io::stdout(),
            "{}",
            json!({"type":"thread.started","thread_id":session})
        )?;
        writeln!(
            io::stdout(),
            "{}",
            json!({"type":"turn.failed","error":{"message":"at capacity","codex_error_info":"server_overloaded"}})
        )?;
        return Ok(());
    }
    if prompt.contains("fixture printed credential") {
        let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        write!(io::stderr(), "diagnostic {token}")?;
        for frame in [
            json!({"type":"thread.started","thread_id":session}),
            json!({"type":"item.completed","item":{"type":"agent_message","text":format!("credential {token}{DONE_FENCE}")}}),
            json!({"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}),
        ] {
            let frame = frame.to_string();
            // Escaping hides the marker from the byte-stream filter. The
            // decoded answer must pass through redaction again at settlement.
            let frame = if prompt.contains("escaped") {
                frame.replace("ghp_", r"\u0067hp_")
            } else {
                frame
            };
            writeln!(io::stdout(), "{frame}")?;
        }
        return Ok(());
    }
    let answer = if prompt.contains("fixture council findings") {
        let entries = if prompt.contains("Frame the immutable review subject for:") {
            json!([])
        } else if prompt.contains("Reconcile independent review findings") {
            json!([{"id":"F1","disposition":"rejected","reason":"falsified"}])
        } else if prompt.contains("Verify the lens findings for:") {
            json!([{"id":"lenses--lens-correctness-spec:F1","severity":"high","file":"a.rs","summary":"fixture finding","status":"open","verification":"falsified","propagates":"lenses:lenses--lens-correctness-spec:F1"}])
        } else if prompt.contains("Lens correctness-spec.") {
            json!([{"id":"F1","severity":"high","file":"a.rs","summary":"fixture finding","status":"open"}])
        } else {
            json!([])
        };
        format!(
            "```findings\n{}\n```\nVERDICT: PASS{DONE_FENCE}\n",
            json!({"schema_version":1,"findings":entries})
        )
    } else if prompt.contains("fixture partial stop") {
        format!(
            "```handoff\n{}\n```",
            json!({"schema_version":2,"status":"stopped_early","note":"Fixture stopped partway.","remaining":["rest"],"decisions":[],"verify":[]})
        )
    } else if prompt.contains("fixture policy continuation") {
        let status = if args.iter().any(|arg| arg == "resume") {
            "done"
        } else {
            "stopped_early"
        };
        format!(
            "```handoff\n{}\n```",
            json!({"schema_version":2,"status":status,"note":"Fixture continuation state.","remaining":[],"decisions":[],"verify":[]})
        )
    } else if prompt.contains("fixture policy result") && policy_model == Some("gpt-6-luna") {
        String::new()
    } else if prompt.contains("Review commit ") {
        let lens = prompt
            .lines()
            .find(|line| line.starts_with("Lens: "))
            .unwrap_or("Lens: unknown");
        let denied = fs::write("review-write-sentinel", b"forbidden").is_err();
        if prompt.contains("fixture unknown verdict") {
            format!("No recognised verdict\n{lens}\nwrite denied: {denied}{DONE_FENCE}")
        } else {
            format!("{lens}\nwrite denied: {denied}\nVERDICT: APPROVE{DONE_FENCE}")
        }
    } else {
        format!("OK{DONE_FENCE}")
    };
    for frame in [
        json!({"type":"thread.started","thread_id":session}),
        json!({"type":"item.completed","item":{"type":"agent_message","text":answer}}),
        json!({"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}),
    ] {
        writeln!(io::stdout(), "{frame}")?;
    }
    Ok(())
}
fn policy_codex_fixture() -> Result<TestRig> {
    let mut rig = TestRig::new()?;
    for path in [".cargo/bin", ".cargo/registry", ".rustup/toolchains"] {
        fs::create_dir_all(rig.home.join(path))?;
    }
    fs::write(rig.home.join(".rustup/settings.toml"), "")?;
    let bin = rig.root.join("provider/bin");
    fs::create_dir_all(&bin)?;
    rig.copy_executable(&std::env::current_exe()?, &bin.join("codex"))?;
    fs::write(
        rig.root.join("codex/auth.json"),
        br#"{"tokens":{"account_id":"fixture-one"}}"#,
    )?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    Ok(rig)
}
fn run_refuses_pacing_reserve() -> Result {
    let rig = policy_codex_fixture()?;
    let auth = br#"{"tokens":{"account_id":"fixture-one"}}"#;
    let key = provenant_providers::identity::codex_account(auth).ok_or("account key")?;
    support::register_account(&rig, "codex", "one", &key, &rig.root.join("codex"))?;
    writeln!(
        OpenOptions::new()
            .append(true)
            .open(rig.state.join("config.toml"))?,
        "\n[domains.routing.pacing]\ncodex='pace'"
    )?;
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
    )?;
    let scope = provenant_quota::AccountScope {
        adapter: "codex".into(),
        account_key: key.clone(),
        label: "one".into(),
    };
    let mut record = provenant_quota::AccountRecord::empty(scope.clone());
    record.snapshot = Some(provenant_quota::QuotaSnapshot {
        schema_version: 1,
        account: scope,
        revision: 0,
        status: provenant_quota::QuotaStatus::Available,
        observed_unix_seconds: now,
        source_ref: "fixture".into(),
        authority_ref: None,
        windows: vec![provenant_quota::QuotaWindow {
            pool: "five_hour".into(),
            window_seconds: Some(18_000),
            used_basis_points: 9_000,
            resets_unix_seconds: Some(now + 9_000),
        }],
        details: None,
    });
    let quota = rig.state.join("quota").join(key);
    fs::create_dir_all(&quota)?;
    fs::set_permissions(rig.state.join("quota"), fs::Permissions::from_mode(0o700))?;
    fs::set_permissions(&quota, fs::Permissions::from_mode(0o700))?;
    let snapshot = quota.join("codex.json");
    fs::write(&snapshot, serde_json::to_vec(&record)?)?;
    fs::set_permissions(snapshot, fs::Permissions::from_mode(0o600))?;
    let prompt = rig.root.join("prompt.txt");
    fs::write(&prompt, "Reply OK.")?;
    let output = rig.command(&[
        "run",
        "--model",
        "codex/gpt-6-luna",
        "--account",
        "one",
        "--mode",
        "read-only",
        "--prompt-file",
        prompt.to_str().ok_or("prompt")?,
        "--json",
    ])?;
    assert_ne!(code(&output), 0, "{output:?}");
    let response = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout.bytes),
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    assert!(response.contains("PacingReserve"), "{output:?}");
    Ok(())
}

/// `settled` is the exit `run` gives when the whole retry chain settles
/// inside its own wait; otherwise it returns `waiting` (0) and `lanes` waits on.
/// Which one happens depends on host speed, so both are accepted. A wait that
/// starts while a successor is still pending counts only that successor, so
/// once the chain has settled the run is read again: a wholly settled named
/// selection returns every terminal row.
fn settled_policy_lanes(
    rig: &TestRig,
    output: provenant_os::exec::ExecOutcome,
    settled: i64,
) -> Result<Value> {
    assert!(code(&output) == settled || code(&output) == 0, "{output:?}");
    let output: Value = serde_json::from_slice(&output.stdout.bytes)?;
    let run_id = output["data"]["run_id"].as_str().ok_or("run ID")?;
    // Keep each command below the rig's ten-second watchdog while allowing
    // sequential guardians to settle and launch their successor.
    for _ in 0..6 {
        let lanes = rig.command(&["lanes", "--id", run_id, "--all", "--wait", "5s", "--json"])?;
        if code(&lanes) != 124 {
            assert_eq!(code(&lanes), 0, "{lanes:?}");
            let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
            if lanes["data"]["ignored_settled"].is_null() {
                return Ok(lanes);
            }
            let all = rig.command(&["lanes", "--id", run_id, "--all", "--wait", "5s", "--json"])?;
            assert_eq!(code(&all), 0, "{all:?}");
            let all: Value = serde_json::from_slice(&all.stdout.bytes)?;
            assert!(all["data"]["ignored_settled"].is_null(), "{all}");
            return Ok(all);
        }
    }
    Err("policy successors did not settle within the fixture budget".into())
}

fn handoff_recovered(lanes: &Value) -> bool {
    let Some(rows) = lanes["data"]["lanes"].as_array() else {
        return false;
    };
    rows.iter()
        .any(|row| row["attempt_number"] == 1 && row["state"] == "quota_limited")
        && rows.iter().any(|row| {
            row["attempt_number"] == 2
                && row["continuation"] == "policy_fallback"
                && !matches!(
                    row["state"].as_str(),
                    None | Some("queued" | "starting" | "running" | "settling")
                )
        })
}

fn handoff_recovery_requires_a_settled_successor() -> Result {
    let mut lanes = json!({"data":{"lanes":[{"attempt_number":1,"state":"quota_limited"}]}});
    assert!(!handoff_recovered(&lanes));
    lanes["data"]["lanes"]
        .as_array_mut()
        .ok_or("fixture lanes must be an array")?
        .push(json!({"attempt_number":2,"state":"running","continuation":"policy_fallback"}));
    assert!(!handoff_recovered(&lanes));
    lanes["data"]["lanes"][1]["state"] = json!("uncommitted");
    assert!(handoff_recovered(&lanes));
    Ok(())
}
/// Confined synthetic accounts exercise real launch, holder settlement and
/// fresh retry routing without contacting a live provider service.
fn interrupted_progress_recovers_on_another_account_in_the_same_worktree() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = policy_codex_fixture()?;
    let cleanup = rig.process_cleanup();
    for path in [
        ".cargo/git",
        "Library/pnpm",
        "Library/Caches/pnpm",
        ".cache/node",
        ".codex",
        ".claude/hooks",
        ".kiro/settings",
        ".gemini/config",
        ".cursor/extensions",
        ".npm",
    ] {
        fs::create_dir_all(rig.home.join(path))?;
    }
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.routing]\nsettlement_retries=1\n",
    )?;
    for (label, directory, account) in [
        ("one", rig.root.join("codex"), "fixture-one"),
        ("two", rig.root.join("codex-two"), "fixture-two"),
    ] {
        fs::create_dir_all(&directory)?;
        let auth = serde_json::to_vec(&json!({"tokens":{"account_id":account}}))?;
        fs::write(directory.join("auth.json"), &auth)?;
        let key = provenant_providers::identity::codex_account(&auth).ok_or("account key")?;
        support::register_account(&rig, "codex", label, &key, &directory)?;
    }
    let git = provenant_os::git::git_executable().ok_or("git")?;
    for args in [
        vec!["commit", "--allow-empty", "-qm", "base"],
        vec!["branch", "-M", "main"],
    ] {
        assert_eq!(code(&rig.execute(&git, &args, None)?), 0);
    }
    let prompt = rig.root.join("prompt.txt");
    fs::write(
        &prompt,
        "fixture handoff recovery: finish the parser and renderer",
    )?;
    let timeline = std::time::Instant::now();
    let output = rig.command(&[
        "run",
        "--model",
        "codex/gpt-6-luna",
        "--effort",
        "low",
        "--operation-id",
        "handoff-recovery",
        "--mode",
        "worktree-write",
        "--worktree-branch",
        "fixture/handoff-recovery",
        "--base",
        "main",
        "--deliver",
        "none",
        "--prompt-file",
        prompt.to_str().ok_or("prompt path")?,
        "--wait",
        "3s",
        "--json",
    ])?;
    let launch: Value = serde_json::from_slice(&output.stdout.bytes)?;
    let worktree = PathBuf::from(
        launch["data"]["worktree"]["path"]
            .as_str()
            .ok_or("retained worktree path")?,
    );
    assert!(code(&output) == 1 || code(&output) == 0, "{output:?}");
    let run_id = launch["data"]["run_id"].as_str().ok_or("run ID")?;
    let mut last_rows = None;
    let mut recovery_history = Vec::new();
    let lanes = provenant_test_support::wait_for(
        "quota-limited attempt and settled fallback",
        std::time::Duration::from_secs(120),
        || -> Result<Value> {
            let output = rig.command(&["lanes", "--id", run_id, "--all", "--json"])?;
            if code(&output) != 0 {
                return Err(format!("lanes command failed: {output:?}").into());
            }
            let lanes: Value = serde_json::from_slice(&output.stdout.bytes)?;
            let rows = lanes["data"]["lanes"].as_array().map(|rows| {
                rows.iter()
                    .map(|row| {
                        json!({
                            "attempt_number": row["attempt_number"],
                            "state": row["state"],
                            "continuation": row["continuation"],
                            "account": row["account"],
                        })
                    })
                    .collect::<Vec<_>>()
            });
            if last_rows.as_ref() != Some(&rows) {
                recovery_history.push((timeline.elapsed(), rows.clone()));
                last_rows = Some(rows);
            }
            Ok(lanes)
        },
        |observation| observation.as_ref().is_ok_and(handoff_recovered),
    )
    .map_err(|error| format!("{error}; recovery history: {recovery_history:?}"))??;
    assert!(lanes["data"]["ignored_settled"].is_null(), "{lanes}");
    let rows = lanes["data"]["lanes"].as_array().ok_or("lane rows")?;
    assert_eq!(rows.len(), 2, "{lanes}");
    let first = rows
        .iter()
        .find(|row| row["attempt_number"] == 1)
        .ok_or("first attempt")?;
    let second = rows
        .iter()
        .find(|row| row["attempt_number"] == 2)
        .ok_or("successor")?;
    assert_eq!(first["state"], "quota_limited", "{lanes}");
    assert_eq!(second["state"], "uncommitted", "{lanes}");
    assert_eq!(second["continuation"], "policy_fallback");
    assert_ne!(
        first["applied_route"]["account_key"],
        second["applied_route"]["account_key"]
    );
    for row in [first, second] {
        assert_eq!(row["applied_route"]["model"], "codex/gpt-6-luna");
    }
    let run_id = first["run_id"].as_str().ok_or("run ID")?;
    let attempt = |row: &Value| -> Result<PathBuf> {
        Ok(rig
            .state
            .join("runs")
            .join(run_id)
            .join("attempts")
            .join(row["attempt_id"].as_str().ok_or("attempt ID")?))
    };
    let snapshot = fs::read_to_string(attempt(first)?.join("handoff.json"))?;
    let handoff = provenant_records::handoff::Handoff::from_json(snapshot.as_bytes())?;
    assert_eq!(handoff.note, "Parser written.\n[REDACTED]\t[REDACTED]");
    assert_eq!(
        handoff.extra["future"]["nested"][1]["token"],
        "\t[REDACTED]"
    );
    let successor_prompt = fs::read_to_string(attempt(second)?.join("prompt.txt"))?;
    assert!(successor_prompt.contains(&handoff.fenced()));
    assert!(!snapshot.contains(HANDOFF_SECRET) && !successor_prompt.contains(HANDOFF_SECRET));
    // The provider's evidence proves the checkout survived both launches and
    // that the second process received the sanitised context under confinement.
    assert_eq!(
        fs::read_to_string(worktree.join("partial-work.txt"))?,
        "parser implementation survives"
    );
    assert_ne!(
        fs::read_to_string(worktree.join("first-account.txt"))?,
        fs::read_to_string(worktree.join("second-account.txt"))?
    );
    assert!(
        fs::read_to_string(worktree.join("successor-context.txt"))?.contains(&handoff.fenced())
    );
    cleanup.disarm();
    Ok(())
}

fn policy_empty_result_falls_back_without_a_chair_turn() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = policy_codex_fixture()?;
    let cleanup = rig.process_cleanup();
    fs::create_dir_all(&rig.state)?;
    fs::set_permissions(&rig.state, fs::Permissions::from_mode(0o700))?;
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n[[domains.routing.model_policy.rules]]\nmodel='codex/gpt-6-luna'\n[domains.routing.model_policy.rules.policy.result]\ncheck=true\nfallback='codex/gpt-6.1-terra'\nfallback_effort='low'\n",
    )?;
    let prompt = rig.root.join("prompt.txt");
    fs::write(&prompt, "fixture policy result")?;
    let output = rig.command(&[
        "run",
        "--model",
        "codex/gpt-6-luna",
        "--effort",
        "low",
        "--operation-id",
        "policy-fallback",
        "--prompt-file",
        prompt.to_str().ok_or("prompt path")?,
        "--wait",
        "3s",
        "--json",
    ])?;
    let lanes = settled_policy_lanes(&rig, output, 0)?;
    let rows = lanes["data"]["lanes"].as_array().ok_or("lane rows")?;
    assert_eq!(rows.len(), 2, "{lanes}");
    let first = rows
        .iter()
        .find(|row| row["attempt_number"] == 1)
        .ok_or("first attempt")?;
    let second = rows
        .iter()
        .find(|row| row["attempt_number"] == 2)
        .ok_or("fallback attempt")?;
    assert_eq!(first["state"], "failed", "{lanes}");
    assert_eq!(second["state"], "succeeded", "{lanes}");
    assert_eq!(second["applied_route"]["model"], "codex/gpt-6.1-terra");
    assert_eq!(second["continuation"], "policy_fallback");
    cleanup.disarm();
    Ok(())
}
fn submit_prints_each_brief_read_root_warning_once() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = policy_codex_fixture()?;
    let cleanup = rig.process_cleanup();
    let elsewhere = tempfile::tempdir()?;
    let outside = fs::canonicalize(elsewhere.path())?.join("rubric.md");
    fs::write(&outside, "rubric")?;
    let prompt = rig.root.join("prompt.txt");
    fs::write(
        &prompt,
        format!(
            "fixture policy result; apply {} and {} again",
            outside.display(),
            outside.display()
        ),
    )?;
    let submit = |json: bool, id: &str| {
        let mut args = vec![
            "run",
            "--model",
            "codex/gpt-6-luna",
            "--effort",
            "low",
            "--operation-id",
            id,
            "--prompt-file",
            prompt.to_str().ok_or("prompt path")?,
            "--wait",
            "3s",
        ];
        if json {
            args.push("--json");
        }
        rig.command(&args)
    };
    let text = submit(false, "brief-warning-text")?;
    let human = String::from_utf8_lossy(&text.stdout.bytes).into_owned();
    assert_eq!(
        human
            .lines()
            .filter(|line| line.contains(&outside.display().to_string()))
            .count(),
        1,
        "{human}"
    );
    assert_eq!(
        human.matches("warning: brief references").count(),
        1,
        "{human}"
    );
    let json = submit(true, "brief-warning-json")?;
    let data: Value = serde_json::from_slice(&json.stdout.bytes)?;
    assert_eq!(
        data["data"]["warnings"].as_array().map(Vec::len),
        Some(1),
        "{data}"
    );
    cleanup.disarm();
    Ok(())
}
fn stats_reports_partial_signals_outside_graph_groups() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = policy_codex_fixture()?;
    let cleanup = rig.process_cleanup();
    fs::create_dir_all(rig.project.join("config"))?;
    fs::write(
        rig.project.join("config/telemetry-prices.toml"),
        include_str!("../../../config/telemetry-prices.toml"),
    )?;
    writeln!(
        OpenOptions::new()
            .append(true)
            .open(rig.state.join("config.toml"))?,
        "\n[domains.routing.routes.receipt-fixture]\nmodel='codex/gpt-6-luna'\neffort='low'"
    )?;
    let prompt = rig.root.join("prompt.txt");
    fs::write(&prompt, "fixture partial stop")?;
    let output = rig.command(&[
        "run",
        "--route",
        "receipt-fixture",
        "--operation-id",
        "partial-stop",
        "--prompt-file",
        prompt.to_str().ok_or("prompt path")?,
        "--wait",
        "3s",
        "--json",
    ])?;
    let lanes = settled_policy_lanes(&rig, output, 0)?;
    assert_eq!(lanes["data"]["lanes"][0]["applied_route"]["effort"], "low");
    assert_eq!(
        lanes["data"]["lanes"][0]["resolved_route"]["source"],
        "named_route"
    );
    assert_eq!(
        lanes["data"]["lanes"][0]["partial_signals"],
        json!(["declared_stop"]),
        "{lanes}"
    );
    let route_stats = rig.command(&["telemetry", "stats", "--by", "route", "--json"])?;
    assert_eq!(code(&route_stats), 0, "{route_stats:?}");
    let route_stats: Value = serde_json::from_slice(&route_stats.stdout.bytes)?;
    assert_eq!(
        route_stats["data"]["context_groups"][0]["value"],
        "codex/gpt-6-luna@low"
    );
    assert_eq!(
        route_stats["data"]["context_groups"][0]["totals"]["attempts"],
        1
    );
    let human = rig.command(&["telemetry", "stats", "--by", "route"])?;
    assert_eq!(code(&human), 0, "{human:?}");
    let human = String::from_utf8(human.stdout.bytes)?;
    assert!(
        human.contains("by route: 1 groups\ncodex/gpt-6-luna@low  "),
        "{human}"
    );
    for args in [
        &["telemetry", "stats", "--json"][..],
        &["telemetry", "stats", "--by", "graph", "--json"],
    ] {
        let stats = rig.command(args)?;
        assert_eq!(code(&stats), 0, "{stats:?}");
        let stats: Value = serde_json::from_slice(&stats.stdout.bytes)?;
        assert_eq!(
            stats["data"]["partial_signals"],
            json!({"declared_stop": 1}),
            "{stats}"
        );
    }
    cleanup.disarm();
    Ok(())
}
fn policy_incomplete_result_continues_the_retained_session() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = policy_codex_fixture()?;
    let cleanup = rig.process_cleanup();
    let prompt = rig.root.join("prompt.txt");
    fs::write(&prompt, "fixture policy continuation")?;
    let output = rig.command(&[
        "run",
        "--model",
        "codex/gpt-6.1-terra",
        "--effort",
        "low",
        "--operation-id",
        "policy-continuation",
        "--prompt-file",
        prompt.to_str().ok_or("prompt path")?,
        "--wait",
        "3s",
        "--json",
    ])?;
    let lanes = settled_policy_lanes(&rig, output, 0)?;
    let rows = lanes["data"]["lanes"].as_array().ok_or("lane rows")?;
    assert_eq!(rows.len(), 2, "{lanes}");
    let first = rows
        .iter()
        .find(|row| row["attempt_number"] == 1)
        .ok_or("first attempt")?;
    let second = rows
        .iter()
        .find(|row| row["attempt_number"] == 2)
        .ok_or("continuation attempt")?;
    assert_eq!(first["state"], "failed", "{lanes}");
    assert_eq!(second["state"], "succeeded", "{lanes}");
    assert_eq!(second["continuation"], "policy_continuation");
    assert_eq!(second["resolved_route"]["account_source"], "continuation");
    assert_eq!(second["applied_route"]["model"], "codex/gpt-6.1-terra");
    assert_eq!(
        first["transcript"]["session"]
            .as_str()
            .ok_or("first session")?,
        second["transcript"]["session"]
            .as_str()
            .ok_or("continued session")?
    );
    cleanup.disarm();
    Ok(())
}
fn capacity_settlement_resumes_the_same_session() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = policy_codex_fixture()?;
    let cleanup = rig.process_cleanup();
    fs::create_dir_all(&rig.state)?;
    fs::set_permissions(&rig.state, fs::Permissions::from_mode(0o700))?;
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.routing]\nsettlement_retries=1\ncapacity_retry_seconds=0\n",
    )?;
    let prompt = rig.root.join("prompt.txt");
    fs::write(&prompt, "fixture capacity")?;
    let output = rig.command(&[
        "run",
        "--model",
        "codex/gpt-6-luna",
        "--effort",
        "low",
        "--operation-id",
        "capacity-retry",
        "--prompt-file",
        prompt.to_str().ok_or("prompt path")?,
        // Long enough for both attempts to settle on most hosts, so `run`
        // reports the failed tail (exit 1); a slow host times out instead.
        "--wait",
        "7s",
        "--json",
    ])?;
    let lanes = settled_policy_lanes(&rig, output, 1)?;
    let rows = lanes["data"]["lanes"].as_array().ok_or("lane rows")?;
    assert_eq!(rows.len(), 2, "{lanes}");
    let first = rows
        .iter()
        .find(|row| row["attempt_number"] == 1)
        .ok_or("first attempt")?;
    let second = rows
        .iter()
        .find(|row| row["attempt_number"] == 2)
        .ok_or("retry attempt")?;
    assert_eq!(first["state"], "model_unavailable", "{lanes}");
    assert!(
        first["reason"]
            .as_str()
            .is_some_and(|reason| reason.ends_with("; retrying this session (attempt 2 of 2)")),
        "{lanes}"
    );
    // The retry resumed the same session; its own capacity stop is the last.
    assert_eq!(second["continuation"], "policy_continuation", "{lanes}");
    assert_eq!(second["state"], "model_unavailable", "{lanes}");
    assert_eq!(
        first["transcript"]["session"]
            .as_str()
            .ok_or("first session")?,
        second["transcript"]["session"]
            .as_str()
            .ok_or("resumed session")?
    );
    cleanup.disarm();
    Ok(())
}
fn review_resolves_identity_only_dispositions() -> Result {
    if !graph_fixture_available().map_err(|e| format!("sandbox: {e}"))? {
        return Ok(());
    }
    let mut rig = policy_codex_fixture().map_err(|e| format!("fixture: {e}"))?;
    let cleanup = rig.process_cleanup();
    writeln!(
        OpenOptions::new()
            .append(true)
            .open(rig.state.join("config.toml")).map_err(|e| format!("config open: {e}"))?,
        "\n[domains.routing.pools]\njunior=[{{model='codex/gpt-6-luna',effort='low'}}]\nsenior=[{{model='codex/gpt-6-luna',effort='low'}}]\nprincipal=[{{model='codex/gpt-6-luna',effort='low'}}]"
    ).map_err(|e| format!("config write: {e}"))?;
    let git = std::env::var_os("PROVENANT_GIT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/bin/git"));
    let commit = rig
        .execute(&git, &["commit", "--allow-empty", "-qm", "Initial"], None)
        .map_err(|e| format!("git: {e}"))?;
    assert_eq!(code(&commit), 0);
    let head = rig.execute(&git, &["rev-parse", "HEAD"], None)?;
    assert_eq!(code(&head), 0);
    let head = String::from_utf8(head.stdout.bytes)?.trim().to_owned();
    let worktree = rig.root.join("detached-worktree");
    let detached = rig.execute(
        &git,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            worktree.to_str().ok_or("worktree")?,
            &head,
        ],
        None,
    )?;
    assert_eq!(code(&detached), 0);
    rig.project = worktree;
    assert_ne!(
        code(&rig.execute(&git, &["symbolic-ref", "--quiet", "HEAD"], None)?),
        0
    );
    let brief = rig.root.join("council-brief.txt");
    fs::write(&brief, "fixture council findings").map_err(|e| format!("brief: {e}"))?;
    let started = rig
        .command(&[
            "run",
            "--preset",
            "review",
            "--prompt-file",
            brief.to_str().ok_or("brief")?,
            "--set",
            "parameters.review_shape=\"standard\"",
            "--set",
            &format!("parameters.subject.commit=\"{head}\""),
            "--set",
            &format!("parameters.base=\"{head}\""),
            "--plan",
            "off",
            "--wait",
            "1s",
            "--json",
        ])
        .map_err(|e| format!("start: {e}"))?;
    let started: Value = serde_json::from_slice(&started.stdout.bytes)?;
    let id = started["data"]["graph"]["checkpoint"]["graph_id"]
        .as_str()
        .ok_or_else(|| format!("missing graph id: {started}"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let output = rig
            .command(&["fabric", "graph", "status", id, "--json"])
            .map_err(|e| format!("status: {e}"))?;
        let status: Value = serde_json::from_slice(&output.stdout.bytes)?;
        if status["data"]["status"] == "complete" {
            let nodes = status["data"]["checkpoint"]["nodes"]
                .as_array()
                .ok_or("nodes")?;
            let lens = nodes
                .iter()
                .find(|node| {
                    node["id"]
                        .as_str()
                        .is_some_and(|id| id.starts_with("lenses--lens-"))
                })
                .ok_or("lens")?;
            assert_eq!(lens["state"], "succeeded", "{status}");
            assert_eq!(lens["source_commit"], head, "{status}");
            assert_eq!(
                lens["outputs"]["verdict"], "VERDICT: INCONCLUSIVE",
                "{status}"
            );
            assert!(
                lens["outputs"]["warnings"]
                    .as_array()
                    .is_some_and(|warnings| warnings.iter().any(|warning| warning
                        .as_str()
                        .is_some_and(
                            |warning| warning.contains(&format!("base {head}, head {head}"))
                        ))),
                "{status}"
            );
            assert!(lens["outputs"]["branch"].is_null(), "{status}");
            assert!(
                lens["attempts"]
                    .as_array()
                    .is_some_and(|attempts| !attempts.is_empty()),
                "{status}"
            );
            let lens_findings: usize = nodes
                .iter()
                .filter(|node| {
                    node["id"]
                        .as_str()
                        .is_some_and(|id| id.starts_with("lenses--lens-"))
                })
                .map(|node| node["outputs"]["findings"].as_array().map_or(0, Vec::len))
                .sum();
            assert_eq!(lens_findings, 1, "{status}");
            let synthesis = nodes
                .iter()
                .find(|node| node["id"] == "synthesis")
                .ok_or("synthesis")?;
            assert_eq!(synthesis["state"], "succeeded", "{status}");
            assert_eq!(
                synthesis["outputs"]["verdict"], "VERDICT: INCONCLUSIVE",
                "{status}"
            );
            assert!(
                synthesis["outputs"]["warnings"]
                    .as_array()
                    .is_some_and(|warnings| warnings.iter().any(|warning| warning
                        .as_str()
                        .is_some_and(
                            |warning| warning.contains(&format!("base {head}, head {head}"))
                        ))),
                "{status}"
            );
            assert_eq!(
                synthesis["outputs"]["findings"][0]["summary"], "fixture finding",
                "{status}"
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "review did not complete: {status}"
        );
        std::thread::park_timeout(std::time::Duration::from_millis(50));
    }
    assert_ne!(
        code(&rig.execute(&git, &["symbolic-ref", "--quiet", "HEAD"], None)?),
        0
    );
    cleanup.disarm();
    Ok(())
}

fn review_fans_out_and_collects_fake_provider() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let mut rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
    for path in [".cargo/bin", ".cargo/registry", ".rustup/toolchains"] {
        fs::create_dir_all(rig.home.join(path))?;
    }
    fs::write(rig.home.join(".rustup/settings.toml"), "")?;
    let bin = rig.root.join("provider/bin");
    fs::create_dir_all(&bin)?;
    rig.copy_executable(&std::env::current_exe()?, &bin.join("codex"))?;
    fs::write(
        rig.root.join("codex/auth.json"),
        br#"{"tokens":{"account_id":"fixture-one"}}"#,
    )?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    writeln!(
        OpenOptions::new()
            .append(true)
            .open(rig.state.join("config.toml"))?,
        // The capacity reviewers below settle without settlement retries:
        // two reviewers of three attempts each can outlast the rig's
        // ten-second watchdog. Retry chains are covered by
        // `capacity_settlement_resumes_the_same_session` and the run store.
        "\n[domains.routing]\nsettlement_retries = 0\n[domains.routing.pools]\nreview-fixture = [{{ model = 'codex/gpt-6-luna', effort = 'low', efforts = ['low'] }}]\n[domains.routing.review.shapes.fixture]\nreviewers = [{{ pool = 'review-fixture', count = 2, lenses = ['correctness', 'tests'], distinct_families = true }}]\n[domains.routing.review.select.by_paths]\n'.github/workflows/**' = 'fixture'"
    )?;
    let git = std::env::var_os("PROVENANT_GIT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/bin/git"));
    let commit = rig.execute(&git, &["commit", "--allow-empty", "-qm", "Initial"], None)?;
    assert_eq!(code(&commit), 0);
    // Review derives its base from origin's default branch.
    let project = rig.project.to_str().ok_or("project path")?;
    for args in [
        &["branch", "-M", "main"][..],
        &["remote", "add", "origin", project][..],
        &["update-ref", "refs/remotes/origin/main", "HEAD"][..],
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ][..],
    ] {
        assert_eq!(code(&rig.execute(&git, args, None)?), 0);
    }
    let brief = rig.root.join("review-brief.txt");
    fs::write(&brief, "Check this change; fixture slow result")?;
    let timed = rig.command(&[
        "review",
        "HEAD",
        "--brief-file",
        brief.to_str().ok_or("brief")?,
        "--shape",
        "fixture",
        "--wait",
        "1s",
        "--json",
    ])?;
    assert_eq!(
        code(&timed),
        124,
        "{}",
        String::from_utf8_lossy(&timed.stdout.bytes)
    );
    let timed: Value = serde_json::from_slice(&timed.stdout.bytes)?;
    let timed_rows = timed["data"]["reviewers"]
        .as_array()
        .ok_or("timed reviewers")?;
    assert_eq!(timed_rows.len(), 2);
    assert!(timed_rows[0]["run_id"].is_string());
    assert!(timed_rows.iter().all(|row| row["state"].is_string()));
    let out = rig.command(&[
        "review",
        "HEAD",
        "--brief-file",
        brief.to_str().ok_or("brief")?,
        "--shape",
        "fixture",
        "--range",
        "missing-ref..HEAD",
        "--wait",
        "60s",
        "--json",
    ])?;
    assert_eq!(
        code(&out),
        0,
        "{} {}",
        String::from_utf8_lossy(&out.stdout.bytes),
        String::from_utf8_lossy(&out.stderr.bytes)
    );
    let result: Value = serde_json::from_slice(&out.stdout.bytes)?;
    assert_eq!(result["data"]["shape"], "fixture");
    assert_eq!(result["data"]["selection_source"], "override");
    let reviewers = result["data"]["reviewers"].as_array().ok_or("reviewers")?;
    assert_eq!(reviewers.len(), 2);
    assert_eq!(result["data"]["verdicts"]["approve"], 2);
    assert_eq!(reviewers[0]["run_id"], timed_rows[0]["run_id"]);
    for (index, lens) in ["correctness", "tests"].iter().enumerate() {
        let row = &reviewers[index];
        assert_eq!(row["state"], "succeeded", "{row}");
        assert_eq!(row["verdict"], "approve", "{row}");
        assert_eq!(row["lens"], *lens);
        assert_eq!(row["substitution"], index == 1);
        assert!(row["run_id"].is_string());
        assert_eq!(row["applied_route"]["model"], "codex/gpt-6-luna");
        let path: provenant_records::StructuredPathWire =
            serde_json::from_value(row["result_path"].clone())?;
        let output = fs::read_to_string(path.to_path_buf()?)?;
        assert!(output.contains(&format!("Lens: {lens}")), "{output}");
        assert!(output.contains("write denied: true"), "{output}");
    }
    assert!(!rig.project.join("review-write-sentinel").exists());
    fs::write(&brief, "fixture unknown verdict")?;
    let unknown = rig.command(&[
        "review",
        "HEAD",
        "--brief-file",
        brief.to_str().ok_or("brief")?,
        "--shape",
        "fixture",
        "--wait",
        "60s",
        "--json",
    ])?;
    assert_eq!(
        code(&unknown),
        1,
        "{}",
        String::from_utf8_lossy(&unknown.stdout.bytes)
    );
    let unknown: Value = serde_json::from_slice(&unknown.stdout.bytes)?;
    assert_eq!(unknown["data"]["verdicts"]["unknown"], 2);
    fs::create_dir_all(rig.project.join(".github/workflows"))?;
    fs::write(rig.project.join(".github/workflows/check.yml"), "fixture")?;
    let added = rig.execute(&git, &["add", ".github/workflows/check.yml"], None)?;
    assert_eq!(code(&added), 0);
    let saved = rig.execute(&git, &["commit", "-qm", "workflow"], None)?;
    assert_eq!(code(&saved), 0);
    fs::create_dir_all(rig.project.join("docs"))?;
    let moved = rig.execute(
        &git,
        &["mv", ".github/workflows/check.yml", "docs/check.yml"],
        None,
    )?;
    assert_eq!(code(&moved), 0);
    let saved = rig.execute(&git, &["commit", "-qm", "move workflow"], None)?;
    assert_eq!(code(&saved), 0);
    fs::write(&brief, "check renamed path")?;
    let renamed = rig.command(&[
        "review",
        "HEAD",
        "--brief-file",
        brief.to_str().ok_or("brief")?,
        "--risk",
        "low",
        "--range",
        "HEAD~1..HEAD",
        "--wait",
        "60s",
        "--json",
    ])?;
    assert_eq!(
        code(&renamed),
        0,
        "{}",
        String::from_utf8_lossy(&renamed.stdout.bytes)
    );
    let renamed: Value = serde_json::from_slice(&renamed.stdout.bytes)?;
    assert_eq!(renamed["data"]["shape"], "fixture");
    assert_eq!(renamed["data"]["selection_source"], "by_paths");
    fs::write(&brief, "fixture capacity")?;
    let failed = rig.command(&[
        "review",
        "HEAD",
        "--brief-file",
        brief.to_str().ok_or("brief")?,
        "--shape",
        "fixture",
        "--wait",
        "60s",
        "--json",
    ])?;
    assert_eq!(
        code(&failed),
        1,
        "{}",
        String::from_utf8_lossy(&failed.stdout.bytes)
    );
    let failed: Value = serde_json::from_slice(&failed.stdout.bytes)?;
    assert!(
        failed["data"]["reviewers"]
            .as_array()
            .ok_or("failed reviewers")?
            .iter()
            .any(|row| row["state"] != "succeeded")
    );
    cleanup.disarm();
    Ok(())
}
/// Medium: injected lifecycle ticks and real durable attempt settlement; no sleeps.
fn opencode_usage_limit_settlement_is_bounded() -> Result {
    use provenant_os::capture::{FrameDecoder, FrameEvent};
    use provenant_supervisor::machine::StopReason;

    // Prose about quota is not a provider refusal and must not stop a turn.
    let decoder = provenant_providers::opencode::OpenCode;
    assert_eq!(
        decoder.decode(br#"{"type":"text","sessionID":"ses_x","part":{"text":"free limit"}}"#)?,
        FrameEvent::Progress
    );
    for stdout in [
        include_bytes!("fixtures/providers/opencode/v2.2.0-free-limit.jsonl").as_slice(),
        include_bytes!("fixtures/providers/opencode/free-limit.jsonl").as_slice(),
        br#"{"type":"provider.quota","message":"Go usage limit exceeded"}"#.as_slice(),
    ] {
        assert!(matches!(
            decoder.decode(stdout)?,
            FrameEvent::Terminal | FrameEvent::ProtocolRejected
        ));
        let (_rig, terminal) = persisted_attempt_with(
            "opencode",
            "opencode/opencode/glm-5.3-flash-free",
            ("zen", "fixture-zen"),
            stdout,
            b"",
            143,
            PersistedAttemptOptions {
                stop_reason: Some(StopReason::PostTerminalCleanup),
                ..Default::default()
            },
        )?;
        assert!(
            matches!(
                terminal.state,
                provenant_run::AttemptState::Terminal {
                    outcome: provenant_run::Outcome::QuotaLimited,
                    ..
                }
            ),
            "{:?}",
            terminal.state
        );
    }
    Ok(())
}

/// Medium: real attempt settlement with a synthetic provider that never exits.
fn opencode_usage_limit_stops_hanging_provider() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let mut rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
    let bin = rig.root.join("provider/bin");
    fs::create_dir_all(&bin)?;
    rig.copy_executable(&std::env::current_exe()?, &bin.join("opencode"))?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    let prompt = rig.root.join("prompt.md");
    fs::write(&prompt, "free limit then hang")?;
    let out = rig.command(&[
        "dispatch",
        "--model",
        "opencode/opencode/glm-5.3-flash-free",
        "--operation-id",
        "opencode-hanging-limit",
        "--prompt-file",
        prompt.to_str().ok_or("prompt")?,
        "--wait",
        "5s",
        "--json",
    ])?;
    assert_eq!(code(&out), 0, "{out:?}");
    let dispatched: Value = serde_json::from_slice(&out.stdout.bytes)?;
    let run = dispatched["data"]["run_id"].as_str().ok_or("run")?;
    let out = rig.command(&["lanes", "--id", run, "--json"])?;
    assert_eq!(code(&out), 0, "{out:?}");
    let lanes: Value = serde_json::from_slice(&out.stdout.bytes)?;
    let lane = &lanes["data"]["lanes"][0];
    assert_eq!(lane["state"], "quota_limited", "{lane}");
    let attempt = rig
        .state
        .join("runs")
        .join(run)
        .join("attempts")
        .join(lane["attempt_id"].as_str().ok_or("attempt")?);
    let exit: Value = serde_json::from_slice(&fs::read(attempt.join("process-exit.json"))?)?;
    // Cleanup, rather than a natural exit or the attempt wall deadline, must
    // have ended the provider. The dispatch wait bounds the settlement above.
    assert_eq!(exit["stop_reason"], "post_terminal_cleanup", "{exit}");
    assert_eq!(exit["terminal_observed"], true, "{exit}");
    cleanup.disarm();
    Ok(())
}

/// Real stderr-only refusal and incidental text through the guardian.
fn opencode_stderr_live_quota() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    for (prompt_text, expected, stop_reason) in [
        (
            "stderr refusal then hang",
            "quota_limited",
            Some("post_terminal_cleanup"),
        ),
        ("stderr quota mention", "succeeded", None),
    ] {
        let mut rig = TestRig::new()?;
        let cleanup = rig.process_cleanup();
        let data = rig.home.join(".local/share/opencode");
        fs::create_dir_all(&data)?;
        let db = rusqlite::Connection::open(data.join("opencode.db"))?;
        db.execute_batch(
            r#"CREATE TABLE session_message (session_id TEXT, type TEXT, data TEXT);
            INSERT INTO session_message VALUES ('ses_x', 'assistant',
            '{"model":{"providerID":"opencode","id":"glm-5.3-flash-free"},"finish":"stop"}');"#,
        )?;
        let bin = rig.root.join("provider/bin");
        fs::create_dir_all(&bin)?;
        rig.copy_executable(&std::env::current_exe()?, &bin.join("opencode"))?;
        rig.env.retain(|(key, _)| key != "PATH");
        rig.env.push((
            "PATH".into(),
            format!("{}:/usr/bin:/bin", bin.display()).into(),
        ));
        let prompt = rig.root.join("prompt.md");
        fs::write(&prompt, prompt_text)?;
        let out = rig.command(&[
            "dispatch",
            "--model",
            "opencode/opencode/glm-5.3-flash-free",
            "--operation-id",
            "stderr-quota",
            "--prompt-file",
            prompt.to_str().ok_or("prompt")?,
            "--wait",
            "5s",
            "--json",
        ])?;
        assert_eq!(code(&out), 0, "{prompt_text}: {out:?}");
        let dispatched: Value = serde_json::from_slice(&out.stdout.bytes)?;
        let run = dispatched["data"]["run_id"].as_str().ok_or("run")?;
        let out = rig.command(&["lanes", "--id", run, "--json"])?;
        assert_eq!(code(&out), 0, "{prompt_text}: {out:?}");
        let lanes: Value = serde_json::from_slice(&out.stdout.bytes)?;
        let lane = &lanes["data"]["lanes"][0];
        assert_eq!(lane["state"], expected, "{prompt_text}: {lane}");
        let attempt = rig
            .state
            .join("runs")
            .join(run)
            .join("attempts")
            .join(lane["attempt_id"].as_str().ok_or("attempt")?);
        let exit: Value = serde_json::from_slice(&fs::read(attempt.join("process-exit.json"))?)?;
        assert_eq!(
            exit["stop_reason"].as_str(),
            stop_reason,
            "{prompt_text}: {exit}"
        );
        assert_eq!(
            exit["terminal_observed"],
            stop_reason.is_some(),
            "{prompt_text}: {exit}"
        );
        cleanup.disarm();
    }
    Ok(())
}

fn opencode_dispatch_observes_v2_wal_session() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    for custom_data_home in [false, true] {
        let mut rig = TestRig::new()?;
        let cleanup = rig.process_cleanup();
        let data_home = if custom_data_home {
            let path = rig.root.join("custom-data");
            rig.env
                .push(("XDG_DATA_HOME".into(), path.clone().into_os_string()));
            path
        } else {
            rig.home.join(".local/share")
        };
        let data = data_home.join("opencode");
        fs::create_dir_all(&data)?;
        let db = rusqlite::Connection::open(data.join("opencode.db"))?;
        // Keep the writer open and disable checkpoints: the assistant row is
        // committed in the WAL when the guardian's read-only connection opens.
        db.execute_batch(
            r#"PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
            CREATE TABLE session_message (session_id TEXT, type TEXT, data TEXT);
            INSERT INTO session_message VALUES ('ses_x', 'assistant',
            '{"model":{"providerID":"opencode-go","id":"beam-free"},"finish":"stop"}');"#,
        )?;
        assert!(fs::metadata(data.join("opencode.db-wal"))?.len() > 0);
        let bin = rig.root.join("provider/bin");
        fs::create_dir_all(&bin)?;
        rig.copy_executable(&std::env::current_exe()?, &bin.join("opencode"))?;
        rig.env.retain(|(key, _)| key != "PATH");
        rig.env.push((
            "PATH".into(),
            format!("{}:/usr/bin:/bin", bin.display()).into(),
        ));
        let prompt = rig.root.join("prompt.md");
        for (operation, text, expected) in [
            ("opencode-success", "reply OK", "succeeded"),
            ("opencode-zen", "reply OK", "succeeded"),
            (
                "opencode-unavailable",
                "model unavailable",
                "model_unavailable",
            ),
            ("opencode-limit", "free limit", "quota_limited"),
        ] {
            fs::write(&prompt, text)?;
            let (model, provider, session_model) = if expected == "model_unavailable" {
                (
                    "opencode/opencode/deepseek-v4.1-flash-free",
                    "opencode",
                    "misspelt-free",
                )
            } else if operation == "opencode-zen" {
                (
                    "opencode/opencode/glm-5.3-flash-free",
                    "opencode",
                    "glm-5.3-flash-free",
                )
            } else {
                (
                    "opencode/opencode-go/glm-5.3-flash",
                    "opencode-go",
                    "glm-5.3-flash",
                )
            };
            db.execute("UPDATE session_message SET data = ?1", [
                serde_json::to_string(&json!({"model": {"providerID": provider, "id": session_model}, "finish": "stop"}))?
            ])?;
            let out = rig.command(&[
                "dispatch",
                "--model",
                model,
                "--operation-id",
                operation,
                "--prompt-file",
                prompt.to_str().ok_or("prompt")?,
                "--wait",
                "5s",
                "--json",
            ])?;
            assert_eq!(code(&out), 0, "{out:?}");
            let dispatched: Value = serde_json::from_slice(&out.stdout.bytes)?;
            let run = dispatched["data"]["run_id"].as_str().ok_or("run")?;
            let lanes = rig.command(&["lanes", "--id", run, "--json"])?;
            assert_eq!(code(&lanes), 0, "{lanes:?}");
            let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
            let lane = &lanes["data"]["lanes"][0];
            assert_eq!(lane["state"], expected, "{lane}");
            let account = if provider == "opencode-go" {
                "go"
            } else {
                "zen"
            };
            assert_eq!(lane["account"], account, "{lane}");
            assert_eq!(lane["applied_route"]["account_label"], account, "{lane}");
            assert_eq!(lane["applied_route"]["account_key"], account, "{lane}");
            if expected == "succeeded" {
                assert_eq!(lane["transcript"]["session"], "ses_x");
                assert_eq!(lane["observed_route"]["model"], model);
                assert_eq!(lane["observed_route"]["source"], "opencode_session");
            } else if expected == "quota_limited" {
                assert!(
                    lane["reason"]
                        .as_str()
                        .is_some_and(|reason| reason.starts_with("Free usage limit reached.")),
                    "{lane}"
                );
            } else {
                assert_eq!(
                    lane["reason"],
                    "provider model unavailable: Model unavailable: opencode/misspelt-free"
                );
            }
        }
        rig.assert_legacy_unchanged()?;
        cleanup.disarm();
    }
    Ok(())
}

fn printed_credentials_leave_no_plaintext_in_run_files() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = policy_codex_fixture()?;
    let cleanup = rig.process_cleanup();
    for mode in ["plain", "escaped"] {
        let prompt = rig.root.join("prompt.txt");
        fs::write(&prompt, format!("fixture printed credential {mode}"))?;
        let out = rig.command(&[
            "run",
            "--model",
            "codex/gpt-6-luna",
            "--effort",
            "low",
            "--operation-id",
            mode,
            "--prompt-file",
            prompt.to_str().ok_or("prompt")?,
            "--wait",
            "5s",
            "--json",
        ])?;
        assert_eq!(code(&out), 0, "{out:?}");
        let response: Value = serde_json::from_slice(&out.stdout.bytes)?;
        let run = response["data"]["run_id"].as_str().ok_or("run")?;
        let lanes = rig.command(&["lanes", "--id", run, "--json"])?;
        assert_eq!(code(&lanes), 0, "{lanes:?}");
        let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
        let lane = &lanes["data"]["lanes"][0];
        assert_eq!(lane["state"], "succeeded", "{lane}");
        let id = lane["attempt_id"].as_str().ok_or("attempt")?;
        let run_dir = rig.state.join("runs").join(run);
        let attempt = run_dir.join("attempts").join(id);
        assert_eq!(
            fs::read_to_string(attempt.join("result.md"))?,
            format!("credential [REDACTED]{DONE_FENCE}")
        );
        assert_eq!(
            fs::read(attempt.join("stderr.log"))?,
            b"diagnostic [REDACTED]"
        );
        let stdout = fs::read_to_string(attempt.join("stdout.log"))?;
        assert!(stdout.contains(if mode == "plain" {
            "[REDACTED]"
        } else {
            r"\u0067hp_"
        }));
        let mut pending = vec![run_dir];
        while let Some(path) = pending.pop() {
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    pending.push(entry.path());
                } else if kind.is_file() {
                    let bytes = fs::read(entry.path())?;
                    assert!(
                        !bytes
                            .windows(40)
                            .any(|window| window == b"ghp_abcdefghijklmnopqrstuvwxyz0123456789"),
                        "plaintext in {}",
                        entry.path().display()
                    );
                }
            }
        }
    }
    cleanup.disarm();
    Ok(())
}

fn codex_dispatch_observes_rollout() -> Result {
    for (index, prompt) in ["reply OK", "fixture missing rollout", "mismatch"]
        .iter()
        .enumerate()
    {
        let mut rig = TestRig::new()?;
        let cleanup = rig.process_cleanup();
        // The production read-only profile anchors the host Rust runtime roots.
        for path in [".cargo/bin", ".cargo/registry", ".rustup/toolchains"] {
            fs::create_dir_all(rig.home.join(path))?;
        }
        fs::write(rig.home.join(".rustup/settings.toml"), "")?;
        let bin = rig.root.join("provider/bin");
        fs::create_dir_all(&bin)?;
        rig.copy_executable(&std::env::current_exe()?, &bin.join("codex"))?;
        let account = rig.root.join("codex");
        fs::write(
            account.join("auth.json"),
            br#"{"tokens":{"account_id":"fixture-one"}}"#,
        )?;
        rig.env.retain(|(k, _)| k != "PATH");
        rig.env.push((
            "PATH".into(),
            format!("{}:/usr/bin:/bin", bin.display()).into(),
        ));
        let prompt_file = rig.root.join("prompt.txt");
        fs::write(&prompt_file, prompt)?;
        let op = format!("{:032x}", index + 1);
        let out = rig.command(&[
            if index == 0 { "run" } else { "dispatch" },
            if index == 0 { "--route" } else { "--model" },
            if index == 0 {
                "utility"
            } else {
                "codex/gpt-6-luna"
            },
            "--effort",
            "low",
            "--operation-id",
            &op,
            "--prompt-file",
            prompt_file.to_str().ok_or("prompt")?,
            "--wait",
            "5",
            "--json",
        ])?;
        assert_eq!(
            code(&out),
            if index == 0 && !cfg!(target_os = "macos") {
                1
            } else {
                0
            },
            "{} {}",
            String::from_utf8_lossy(&out.stdout.bytes),
            String::from_utf8_lossy(&out.stderr.bytes)
        );
        let lanes = rig.command(&["lanes", "--all", "--json"])?;
        let value: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
        let rows = value["data"]["lanes"]
            .as_array()
            .ok_or_else(|| format!("lanes: {value}"))?;
        let lane = rows.first().ok_or_else(|| format!("lane: {value}"))?;
        if !cfg!(target_os = "macos") {
            assert_eq!(lane["state"], "rejected", "{lane}");
            assert!(
                lane["reason"]
                    .as_str()
                    .is_some_and(|r| r.contains("UnsupportedPlatform")),
                "{lane}"
            );
            rig.assert_legacy_unchanged()?;
            cleanup.disarm();
            return Ok(());
        }
        if index == 0 {
            assert_eq!(lane["state"], "succeeded", "{lane}");
            assert_eq!(lane["observed_route"]["model"], "codex/gpt-6-luna");
            assert_eq!(lane["observed_route"]["source"], "codex_rollout");
            assert!(lane["applied_route"]["account_label"].as_str().is_some());
            let result: Value = serde_json::from_slice(&out.stdout.bytes)?;
            let id = lane["attempt_id"].as_str().ok_or("attempt")?;
            assert_eq!(
                result["data"]["result_tails"][id]["bytes"]["data"],
                format!("OK{DONE_FENCE}")
            );
            fs::write(&prompt_file, "fixture slow result")?;
            let slow = rig.command(&[
                "run",
                "--model",
                "codex/gpt-6-luna",
                "--effort",
                "low",
                "--prompt-file",
                prompt_file.to_str().ok_or("prompt")?,
                "--wait",
                "1s",
            ])?;
            assert_eq!(code(&slow), 0, "{slow:?}");
            let text = String::from_utf8(slow.stdout.bytes)?;
            let id = text
                .lines()
                .find_map(|line| {
                    line.strip_prefix("next: provenant lanes --id ")
                        .and_then(|rest| rest.split_whitespace().next())
                })
                .ok_or("timeout attempt")?;
            assert!(
                text.contains(&format!("provenant lanes --id {id} --wait 4m")),
                "{text}"
            );
            let settled = rig.command(&["lanes", "--id", id, "--wait", "10s", "--json"])?;
            assert_eq!(code(&settled), 0, "{settled:?}");
        } else {
            assert_eq!(lane["state"], "failed", "{lane}");
            assert!(
                lane["reason"]
                    .as_str()
                    .is_some_and(|r| r.starts_with("provider identity was not observed: ")),
                "{lane}"
            );
        }
        rig.assert_legacy_unchanged()?;
        cleanup.disarm();
    }
    Ok(())
}

fn capacity_holder_persists_and_explicit_and_resume_ignore_cooldown() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let mut rig = TestRig::new()?;
    let cleanup = rig.process_cleanup();
    for path in [".cargo/bin", ".cargo/registry", ".rustup/toolchains"] {
        fs::create_dir_all(rig.home.join(path))?;
    }
    fs::write(rig.home.join(".rustup/settings.toml"), "")?;
    let bin = rig.root.join("provider/bin");
    fs::create_dir_all(&bin)?;
    rig.copy_executable(&std::env::current_exe()?, &bin.join("codex"))?;
    fs::write(
        rig.root.join("codex/auth.json"),
        br#"{"tokens":{"account_id":"fixture-one"}}"#,
    )?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    // This test pins the settled capacity state; the retry has its own test.
    fs::create_dir_all(&rig.state)?;
    fs::set_permissions(&rig.state, fs::Permissions::from_mode(0o700))?;
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.routing]\nsettlement_retries=0\n",
    )?;
    let prompt = rig.root.join("prompt.txt");
    fs::write(&prompt, "fixture capacity")?;
    let out = rig.command(&[
        "dispatch",
        "--model",
        "codex/gpt-6-luna",
        "--effort",
        "low",
        "--operation-id",
        "capacity-first",
        "--prompt-file",
        prompt.to_str().ok_or("prompt")?,
        "--wait",
        "5s",
        "--json",
    ])?;
    assert_eq!(code(&out), 0, "{out:?}");
    let data: Value = serde_json::from_slice(&out.stdout.bytes)?;
    let run = data["data"]["run_id"].as_str().ok_or("run")?;
    let lanes = rig.command(&["lanes", "--id", run, "--json"])?;
    let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
    let id = lanes["data"]["lanes"][0]["attempt_id"]
        .as_str()
        .ok_or("attempt")?;
    assert_eq!(
        lanes["data"]["lanes"][0]["state"], "model_unavailable",
        "{lanes}"
    );
    assert_eq!(
        lanes["data"]["capacity_cooldowns"][0]["model"], "gpt-6-luna",
        "{lanes}"
    );
    fs::write(&prompt, "reply OK")?;
    let explicit = rig.command(&[
        "dispatch",
        "--model",
        "codex/gpt-6-luna",
        "--effort",
        "low",
        "--operation-id",
        "capacity-explicit",
        "--prompt-file",
        prompt.to_str().ok_or("prompt")?,
        "--wait",
        "5s",
        "--json",
    ])?;
    assert_eq!(code(&explicit), 0, "{explicit:?}");
    let explicit: Value = serde_json::from_slice(&explicit.stdout.bytes)?;
    let explicit_run = explicit["data"]["run_id"].as_str().ok_or("explicit run")?;
    let explicit = rig.command(&["lanes", "--id", explicit_run, "--json"])?;
    let explicit: Value = serde_json::from_slice(&explicit.stdout.bytes)?;
    assert_eq!(
        explicit["data"]["lanes"][0]["state"], "succeeded",
        "{explicit}"
    );
    // Exercise the public senior and junior routes through the full CLI and
    // host preparation path while the same account has a live cooldown.
    let config_path = rig.state.join("config.toml");
    let config = fs::read_to_string(&config_path)?;
    fs::write(
        &config_path,
        format!(
            "{config}\n[domains.routing.pools]\n\
            junior = [{{ model = \"codex/gpt-6-luna\", weight = 100, effort = \"low\", efforts = [\"low\"] }}, {{ model = \"codex/gpt-6.1-sol\", weight = 1, effort = \"low\", efforts = [\"low\"] }}]\n\
            senior = [{{ model = \"codex/gpt-6-luna\", weight = 100, effort = \"low\", efforts = [\"low\"] }}, {{ model = \"codex/gpt-6.1-sol\", weight = 1, effort = \"low\", efforts = [\"low\"] }}]\n"
        ),
    )?;
    fs::write(&prompt, "fixture pool result")?;
    for pool in ["senior", "junior"] {
        let operation = format!("capacity-pool-{pool}");
        let draw = rig.command(&[
            "dispatch",
            "--route",
            pool,
            "--operation-id",
            &operation,
            "--prompt-file",
            prompt.to_str().ok_or("prompt")?,
            "--wait",
            "5s",
            "--json",
        ])?;
        assert_eq!(code(&draw), 0, "{draw:?}");
        let draw: Value = serde_json::from_slice(&draw.stdout.bytes)?;
        let run = draw["data"]["run_id"].as_str().ok_or("pool run")?;
        let lanes = rig.command(&["lanes", "--id", run, "--json"])?;
        let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
        let lane = &lanes["data"]["lanes"][0];
        assert_eq!(lane["state"], "succeeded", "{lane}");
        assert_eq!(
            lane["applied_route"]["model"], "codex/gpt-6.1-sol",
            "{lane}"
        );
        let receipt = &lane["applied_route"]["pool_selection"];
        assert_eq!(receipt["pool"], pool, "{receipt}");
        let excluded = receipt["candidates"]
            .as_array()
            .ok_or("pool candidates")?
            .iter()
            .find(|candidate| candidate["model"] == "codex/gpt-6-luna")
            .ok_or("cooling candidate")?;
        assert_eq!(excluded["exclusion"], "capacity cooldown", "{receipt}");
    }
    fs::write(&prompt, "reply OK")?;
    let resumed = rig.command(&[
        "resume",
        id,
        "--no-fallback",
        "--operation-id",
        "capacity-resume",
        "--prompt-file",
        prompt.to_str().ok_or("prompt")?,
        "--wait",
        "5s",
        "--json",
    ])?;
    assert_eq!(code(&resumed), 0, "{resumed:?}");
    let resumed: Value = serde_json::from_slice(&resumed.stdout.bytes)?;
    assert_eq!(resumed["data"]["continuation"], "provider", "{resumed}");
    let resumed_run = resumed["data"]["run_id"].as_str().ok_or("resumed run")?;
    let resumed = rig.command(&["lanes", "--id", resumed_run, "--json"])?;
    let resumed: Value = serde_json::from_slice(&resumed.stdout.bytes)?;
    assert_eq!(
        resumed["data"]["lanes"][0]["state"], "succeeded",
        "{resumed}"
    );
    assert_eq!(
        resumed["data"]["lanes"][0]["applied_route"]["model"], "codex/gpt-6-luna",
        "{resumed}"
    );
    cleanup.disarm();
    Ok(())
}
fn claude_fixture() -> Result<TestRig> {
    let mut rig = TestRig::new()?;
    let bin = rig.root.join("provider/bin");
    fs::create_dir_all(&bin)?;
    rig.copy_executable(&std::env::current_exe()?, &bin.join("claude"))?;
    let account = rig.home.join(".claude-pro");
    fs::create_dir(&account)?;
    fs::write(
        account.join(".credentials.json"),
        br#"{"claudeAiOauth":{"accessToken":"fixture-expired-token","expiresAt":1}}"#,
    )?;
    rig.env.retain(|(k, _)| k != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    let registered = rig.command(&[
        "accounts",
        "register",
        "pro",
        "--adapter",
        "claude",
        "--directory",
        account.to_str().ok_or("account path")?,
        "--json",
    ])?;
    assert_eq!(
        code(&registered),
        0,
        "{}",
        String::from_utf8_lossy(&registered.stdout.bytes)
    );
    let listed = rig.command(&["accounts", "list", "--json"])?;
    let listed: Value = serde_json::from_slice(&listed.stdout.bytes)?;
    assert_eq!(
        listed["data"]["entries"]["claude-pro"]["label"],
        "pro",
        "{} config={}",
        listed,
        fs::read_to_string(rig.state.join("config.toml"))?
    );
    Ok(rig)
}

fn opus_writer_dispatch_advises_a_cheaper_route() -> Result {
    {
        let rig = claude_fixture()?;
        let cleanup = rig.process_cleanup();
        let mut config = OpenOptions::new()
            .append(true)
            .open(rig.state.join("config.toml"))?;
        writeln!(
            config,
            "\n[domains.routing.pacing.accounts.pro]\ndeadline={{reset_of='removed'}}"
        )?;
        let prompt = rig.root.join("prompt.md");
        fs::write(&prompt, "Reply OK.")?;
        let out = rig.command(&[
            "dispatch",
            "--operation-id",
            "stale-profile",
            "--model",
            "claude/claude-sonnet-5-5",
            "--account",
            "pro",
            "--prompt-file",
            prompt.to_str().ok_or("prompt")?,
            "--wait",
            "10s",
            "--json",
        ])?;
        assert_eq!(code(&out), 0, "{out:?}");
        let result: Value = serde_json::from_slice(&out.stdout.bytes)?;
        let run = result["data"]["run_id"].as_str().ok_or("run")?;
        let lanes = rig.command(&["lanes", "--id", run, "--json"])?;
        assert_eq!(code(&lanes), 0, "{lanes:?}");
        let result: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
        let lane = &result["data"]["lanes"][0];
        assert_eq!(lane["state"], "rejected", "{lane}");
        assert!(
            lane["reason"].as_str().is_some_and(|message| message
                .contains("pacing.accounts.pro.deadline.reset_of")
                && message.contains("removed")),
            "{lane}"
        );
        cleanup.disarm();
    }
    for (model, writer, suppress) in [
        ("claude/claude-opus-5-5", true, false),
        ("claude/claude-opus-5-5", true, true),
        ("claude/claude-opus-5-5", false, false),
        ("claude/claude-sonnet-5-5", true, false),
        ("claude/claude-haiku-5-5", false, false),
    ] {
        let rig = claude_fixture()?;
        let cleanup = rig.process_cleanup();
        if suppress {
            let mut config = OpenOptions::new()
                .append(true)
                .open(rig.state.join("config.toml"))?;
            writeln!(
                config,
                "\n[domains.routing]\nsuppress_opus_writer_warning = true"
            )?;
        }
        let git = provenant_os::git::git_executable().ok_or("git")?;
        for args in [
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.invalid"],
            vec!["commit", "--allow-empty", "-qm", "base"],
            vec!["branch", "-M", "main"],
        ] {
            assert_eq!(code(&rig.execute(&git, &args, None)?), 0);
        }
        let prompt = rig.root.join("prompt.md");
        fs::write(&prompt, "Reply OK without changing files.")?;
        let mut args = vec![
            "dispatch",
            "--operation-id",
            "opus-writer",
            "--model",
            model,
            "--account",
            "pro",
            "--prompt-file",
            prompt.to_str().ok_or("prompt")?,
            "--json",
        ];
        if writer {
            args.extend([
                "--mode",
                "worktree-write",
                "--worktree-branch",
                "fixture/opus",
                "--base",
                "main",
                "--deliver",
                "none",
            ]);
        }
        let out = rig.command(&args)?;
        assert_eq!(
            code(&out),
            0,
            "{} {}",
            String::from_utf8_lossy(&out.stdout.bytes),
            String::from_utf8_lossy(&out.stderr.bytes)
        );
        let stderr = String::from_utf8_lossy(&out.stderr.bytes);
        assert!(!stderr.contains("not in the catalogue"), "{stderr}");
        let reply: Value = serde_json::from_slice(&out.stdout.bytes)?;
        assert_eq!(reply["status"], "ok", "{reply}");
        if !writer || suppress || model.contains("sonnet") {
            assert!(!stderr.contains("advisory"), "{stderr}");
        } else {
            let advisory = stderr
                .lines()
                .find(|line| line.contains("advisory"))
                .ok_or_else(|| format!("missing Opus writer advisory: {stderr}"))?;
            let routes: provenant_routing::WriterAdvisory = toml_edit::de::from_str(include_str!(
                "../crates/routing/assets/writer-advisory.toml"
            ))?;
            assert!(advisory.contains(model), "{advisory}");
            assert!(
                advisory.contains(routes.label.as_deref().ok_or("advisory label")?),
                "{advisory}"
            );
            assert!(
                routes
                    .cheaper_models
                    .iter()
                    .any(|route| advisory.contains(route.as_str())),
                "{advisory}"
            );
            assert!(
                routes
                    .pool_priority
                    .iter()
                    .any(|pool| advisory.contains(pool)),
                "{advisory}"
            );
            assert_eq!(
                stderr
                    .lines()
                    .filter(|line| line.contains("advisory"))
                    .count(),
                1
            );
        }
        rig.assert_legacy_unchanged()?;
        cleanup.disarm();
    }
    Ok(())
}

fn claude_dispatch_refreshes_registered_account() -> Result {
    claude_dispatch(false, false)
}

fn host_native_dispatch_uses_the_declared_agent_and_refuses_resume() -> Result {
    if !graph_fixture_available()? {
        writeln!(
            io::stderr(),
            "host_native_dispatch_uses_the_declared_agent_and_refuses_resume skipped: graph fixture unavailable"
        )?;
        return Ok(());
    }
    claude_dispatch(true, false)
}

fn unconfigured_host_native_repair_launches_alias_fallback() -> Result {
    if !graph_fixture_available()? {
        writeln!(
            io::stderr(),
            "unconfigured_host_native_repair_launches_alias_fallback skipped: graph fixture unavailable"
        )?;
        return Ok(());
    }
    let rig = claude_fixture()?;
    let cleanup = rig.process_cleanup();
    writeln!(
        OpenOptions::new()
            .append(true)
            .open(rig.state.join("config.toml"))?,
        "\n[domains.routing.pools]\ngraph-test = [{{ model = 'claude/claude-sonnet-5-5' }}]"
    )?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    assert_eq!(
        code(&rig.execute(&git, &["commit", "--allow-empty", "-qm", "Initial",], None,)?),
        0
    );
    let created = rig.command(&[
        "worktree",
        "create",
        "graph-repair",
        "--base",
        "HEAD",
        "--json",
    ])?;
    assert_eq!(code(&created), 0, "{created:?}");
    for path in [
        ".cargo/bin",
        ".cargo/registry",
        ".cargo/git",
        "Library/pnpm",
        "Library/Caches/pnpm",
        ".cache/node",
        ".rustup/toolchains",
        ".codex",
        ".claude/hooks",
        ".kiro/settings",
        ".gemini/config",
        ".cursor/extensions",
        ".npm",
    ] {
        fs::create_dir_all(rig.home.join(path))?;
    }
    fs::write(rig.home.join(".rustup/settings.toml"), "")?;
    fs::write(
        rig.project.join("graph.json"),
        serde_json::to_vec(
            &json!({"schema_version":1,"graph_id":"native-fallback","nodes":[
                {"id":"repair","prompt":"reply OK","access":"worktree_write",
                 "worktree":".worktrees/graph-repair","delivery":"none","routes":[
                    {"adapter":"host-native","name":"absent"},
                    {"alias":"graph-test","account":"pro"}
                 ]}
            ]}),
        )?,
    )?;
    let started = rig.command(&[
        "fabric",
        "graph",
        "run",
        "--document",
        "graph.json",
        "--json",
    ])?;
    assert_eq!(code(&started), 0, "{started:?}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let status = rig.command(&["fabric", "graph", "status", "native-fallback", "--json"])?;
        assert_eq!(code(&status), 0, "{status:?}");
        let status: Value = serde_json::from_slice(&status.stdout.bytes)?;
        let repair = &status["data"]["checkpoint"]["nodes"][0];
        if status["data"]["status"] == "complete" {
            let attempts = repair["attempts"].as_array().ok_or("attempts")?;
            assert_eq!(attempts.len(), 2, "{status}");
            assert_eq!(attempts[0]["outcome"], "adapter_unavailable", "{status}");
            assert_eq!(attempts[0]["launched"], false, "{status}");
            assert_eq!(attempts[1]["launched"], true, "{status}");
            assert_eq!(repair["state"], "succeeded", "{status}");
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{status}");
        std::thread::park_timeout(std::time::Duration::from_millis(50));
    }
    let lanes = rig.command(&["lanes", "--all", "--json"])?;
    assert_eq!(code(&lanes), 0, "{lanes:?}");
    let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
    let lanes = lanes["data"]["lanes"].as_array().ok_or("lanes")?;
    assert_eq!(lanes.len(), 2);
    let refused = lanes
        .iter()
        .find(|lane| lane["requested_route"]["provider"] == "host-native")
        .ok_or("host-native attempt")?;
    assert_eq!(refused["state"], "rejected", "{refused}");
    assert_eq!(
        refused["reason"],
        "host-native absent is not configured in domains.fabric.host_native"
    );
    let launched = lanes
        .iter()
        .find(|lane| lane["state"] == "succeeded")
        .ok_or("fallback lane did not succeed")?;
    assert_eq!(launched["applied_route"]["provider"], "claude");
    assert_eq!(
        launched["applied_route"]["model"],
        "claude/claude-sonnet-5-5"
    );
    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
}

fn host_native_preparation_uses_the_provider_contract() -> Result {
    use provenant_core::{AttemptId, Digest, OperationId, RunId, TaskId};
    let rig = claude_fixture()?;
    writeln!(
        OpenOptions::new()
            .append(true)
            .open(rig.state.join("config.toml"))?,
        "\n[domains.fabric.host_native.ci-fixer]\nkind = 'claude-agent'\nmodel = 'claude/claude-sonnet-5-5'\neffort = 'medium'\ndescription = 'Repair CI'\nprompt = 'Repair failing CI checks.'"
    )?;
    fs::write(
        rig.home.join(".claude-pro/.credentials.json"),
        serde_json::to_vec(
            &json!({"claudeAiOauth":{"accessToken":"credential-sentinel-2719-violet-fox","expiresAt":provenant_core::Clock::wall_now(&provenant_os::clock::SystemClock::new())?.unix_millis() + 3_600_000}}),
        )?,
    )?;
    let mut inputs = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let binding = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(rig.project.join(".git"))?,
    )?;
    let assets = rig.root.join("assets");
    fs::create_dir(&assets)?;
    let mut host = provenant_run::HostLaunchContext::from_environment(
        &rig.state,
        &rig.project,
        provenant_os::git::git_executable()
            .ok_or("git")?
            .canonicalize()?,
    );
    host.executables
        .insert("claude".into(), std::env::current_exe()?);
    // Provider resolution must honour the host pin, even when captured PATH
    // does not contain Claude. Command entries instead retain literal lookup.
    host.executable_search_path = vec![rig.root.join("missing-bin")];
    let registry = provenant_cli::run::Registry {
        roots: Some(&roots),
        home: Some(&rig.home),
        cwd: Some(&rig.project),
        assets: Some(&assets),
        state_root: Some(&rig.state),
        project_id: Some(binding.project_id()),
        host: Some(&host),
        ..Default::default()
    };
    let attempt_id = AttemptId::from_bytes([1; 16]);
    let run_id = RunId::from_bytes([2; 16]);
    let attempt = rig
        .state
        .join(format!("runs/{run_id}/attempts/{attempt_id}"));
    let paths = provenant_run::LaunchPaths {
        attempt_id,
        run_id,
        operation_id: OperationId::parse("host-preparation")?,
        task_index: 0,
        lane_depth: 1,
        deadline_unix_ms: u64::MAX,
        scratch: attempt.join("scratch"),
        temporary: attempt.join("tmp"),
        temporary_alias: None,
        provider_state: attempt.join("provider-state"),
    };
    for path in [&paths.scratch, &paths.temporary, &paths.provider_state] {
        fs::create_dir_all(path)?;
    }
    let task: provenant_run::TaskRequest = serde_json::from_value(json!({
        "task_id":TaskId::from_bytes([3;16]), "prompt_digest":provenant_records::CanonicalV1::file_digest(b"fixture host-native"),
        "route":{"kind":"adapter","provider":"host-native","name":"ci-fixer"}, "account":"pro", "fallback":[],
        "authority_digest":Digest::from_bytes([4;32]), "provider_timeout_seconds":20, "delivery_id":null
    }))?;
    let mut absent = task.clone();
    absent.route = provenant_run::RequestedRoute::Adapter {
        extra: Default::default(),
        provider: "host-native".into(),
        name: "absent".into(),
    };
    let error =
        provenant_run::Providers::prepare(&registry, &absent, "fixture host-native", &paths, None)
            .err()
            .ok_or("unconfigured host-native entry was accepted")?;
    assert_eq!(
        error.to_string(),
        "host-native absent is not configured in domains.fabric.host_native"
    );
    assert_eq!(
        error.fallback_outcome,
        Some(provenant_records::run::PreparationFailure::AdapterUnavailable)
    );
    assert_eq!(fs::read_dir(&paths.temporary)?.count(), 0);
    let mut unsupported = task.clone();
    unsupported.route = provenant_run::RequestedRoute::Adapter {
        extra: Default::default(),
        provider: "cursor".into(),
        name: "fixture".into(),
    };
    let error = provenant_run::Providers::prepare(&registry, &unsupported, "fixture", &paths, None)
        .err()
        .ok_or("unsupported named entry was accepted")?;
    assert_eq!(error.to_string(), "cursor has no named model-less entries");
    assert_eq!(fs::read_dir(&paths.temporary)?.count(), 0);
    unsupported.route = provenant_run::RequestedRoute::Adapter {
        extra: Default::default(),
        provider: "command".into(),
        name: "fixture".into(),
    };
    let error = provenant_run::Providers::prepare(&registry, &unsupported, "fixture", &paths, None)
        .err()
        .ok_or("unconfigured command was accepted")?;
    assert_eq!(
        error.to_string(),
        "command fixture is not configured in domains.fabric.commands"
    );
    assert_eq!(fs::read_dir(&paths.temporary)?.count(), 0);
    writeln!(
        OpenOptions::new()
            .append(true)
            .open(rig.state.join("config.toml"))?,
        "\n[domains.routing.routes.receipt-fixture]\nmodel='claude/claude-sonnet-5-5'\neffort='medium'"
    )?;
    let mut named = task.clone();
    named.route = provenant_run::RequestedRoute::Named {
        extra: Default::default(),
        name: "receipt-fixture".into(),
        effort: None,
    };
    let named_launch = provenant_run::Providers::prepare(
        &registry,
        &named,
        "synthetic-prompt-secret-2719",
        &paths,
        None,
    )?;
    assert_eq!(
        named_launch.route.model.as_deref(),
        Some("claude/claude-sonnet-5-5")
    );
    assert_eq!(named_launch.route.effort.as_deref(), Some("medium"));
    assert_eq!(named_launch.route.source.as_deref(), Some("named_route"));
    assert_eq!(named_launch.route.extra["account_source"], "pin");
    let receipt = serde_json::to_string(&named_launch.route)?;
    assert!(!receipt.contains("synthetic-prompt-secret-2719"));
    assert!(!receipt.contains("credential-sentinel-2719-violet-fox"));
    writeln!(
        OpenOptions::new()
            .append(true)
            .open(rig.state.join("config.toml"))?,
        "\n[domains.routing.pools]\nsecret-fixture = [{{ model = 'claude/claude-sonnet-5-5', effort = 'medium' }}]"
    )?;
    let mut pooled = task.clone();
    pooled.route = provenant_run::RequestedRoute::Review {
        extra: Default::default(),
        name: "secret-fixture".into(),
        effort: None,
        excluded_families: vec![],
        preferred_families: vec![],
        distinct_families: false,
        require_distinct: false,
    };
    let pool_launch = provenant_run::Providers::prepare(
        &registry,
        &pooled,
        "prompt-sentinel-2719-indigo-owl",
        &paths,
        None,
    )?;
    assert_eq!(pool_launch.route.source.as_deref(), Some("pool"));
    assert_eq!(
        serde_json::to_value(&pool_launch.route)?["pool_selection"]["pool"],
        "secret-fixture"
    );
    assert!(
        String::from_utf8_lossy(&pool_launch.stdin).contains("prompt-sentinel-2719-indigo-owl")
    );
    assert!(pool_launch.secret_input.is_some());
    let receipt = serde_json::to_string(&pool_launch.route)?;
    assert!(!receipt.contains("prompt-sentinel-2719-indigo-owl"));
    assert!(!receipt.contains("credential-sentinel-2719-violet-fox"));
    named.account = None;
    let automatic = provenant_run::Providers::prepare(
        &registry,
        &named,
        "synthetic-prompt-secret-2719",
        &paths,
        None,
    )?;
    assert_eq!(automatic.route.extra["account_source"], "automatic");
    let launch =
        provenant_run::Providers::prepare(&registry, &task, "fixture host-native", &paths, None)?;
    assert_eq!(launch.route.provider.as_deref(), Some("claude"));
    assert_eq!(
        launch.route.model.as_deref(),
        Some("claude/claude-sonnet-5-5")
    );
    assert_eq!(launch.route.effort.as_deref(), Some("medium"));
    assert_eq!(launch.route.extra["host_native"], "ci-fixer");
    let argument = |flag| {
        launch
            .argv
            .windows(2)
            .find(|pair| pair[0] == flag)
            .map(|pair| &pair[1])
    };
    assert_eq!(
        argument("--agent").and_then(|value| value.to_str()),
        Some("ci-fixer")
    );
    let definition: Value = serde_json::from_str(
        argument("--agents")
            .and_then(|value| value.to_str())
            .ok_or("agents")?,
    )?;
    assert_eq!(
        definition["ci-fixer"]["prompt"],
        "Repair failing CI checks."
    );
    assert!(String::from_utf8(launch.stdin)?.contains("fixture host-native"));
    assert!(launch.secret_input.is_some());
    assert!(launch.confinement.is_some());
    // A nested result-policy fallback must not silently turn the named agent
    // into an ordinary model route with different recovery semantics.
    writeln!(
        OpenOptions::new()
            .append(true)
            .open(rig.state.join("config.toml"))?,
        "\n[[domains.routing.model_policy.rules]]\nadapter='claude'\nmodel='claude/claude-sonnet-5-5'\n[domains.routing.model_policy.rules.policy.result]\ncheck=true\nfallback='claude/claude-opus-5-5'\nfallback_effort='high'"
    )?;
    let mut nested = paths;
    nested.lane_depth = 2;
    nested.temporary = attempt.join("nested-tmp");
    nested.provider_state = attempt.join("nested-state");
    nested.scratch = attempt.join("nested-scratch");
    for path in [&nested.temporary, &nested.provider_state, &nested.scratch] {
        fs::create_dir_all(path)?;
    }
    let fallback = provenant_run::Providers::prepare(
        &registry,
        &named,
        "synthetic-prompt-secret-2719",
        &nested,
        None,
    )?;
    assert_eq!(fallback.route.source.as_deref(), Some("fallback"));
    assert_eq!(fallback.route.effort.as_deref(), Some("high"));
    let error =
        provenant_run::Providers::prepare(&registry, &task, "fixture host-native", &nested, None)
            .err()
            .ok_or("host agent was silently replaced by its provider's policy fallback")?;
    assert_eq!(
        error.to_string(),
        "host-native requires its configured model; use graph routes for non-native fallback"
    );
    Ok(())
}

fn pool_receipt_excludes_prompt_and_credential_from_artifacts() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    claude_dispatch(false, true)
}

fn claude_dispatch(host_native: bool, pool: bool) -> Result {
    let rig = claude_fixture()?;
    // Open-network Claude dispatch must not require a Rust installation.
    assert!(!rig.home.join(".cargo").exists());
    assert!(!rig.home.join(".rustup").exists());
    let cleanup = rig.process_cleanup();
    let account = rig.home.join(".claude-pro");
    let route = if host_native {
        writeln!(
            OpenOptions::new()
                .append(true)
                .open(rig.state.join("config.toml"))?,
            "\n[domains.fabric.host_native.ci-fixer]\nkind = 'claude-agent'\nmodel = 'claude/claude-sonnet-5-5'\neffort = 'medium'\ndescription = 'Repair CI'\nprompt = 'Repair failing CI checks.'"
        )?;
        json!({"kind":"adapter","provider":"host-native","name":"ci-fixer"})
    } else if pool {
        writeln!(
            OpenOptions::new()
                .append(true)
                .open(rig.state.join("config.toml"))?,
            "\n[domains.routing.pools]\nsecret-fixture = [{{ model = 'claude/claude-sonnet-5-5', effort = 'medium' }}]"
        )?;
        json!({"kind":"review","name":"secret-fixture","effort":null,"excluded_families":[],"distinct_families":false})
    } else {
        json!({"kind":"model","provider":"claude","model":"claude-sonnet-5-5","effort":"medium"})
    };
    let manifest = rig.root.join("manifest.json");
    fs::write(
        &manifest,
        serde_json::to_vec(
            &json!({"schema_version":1,"tasks":[{"prompt":if host_native { "fixture host-native" } else if pool { "prompt-sentinel-2719-indigo-owl" } else { "reply OK" },"route":route,"account":"pro"}]}),
        )?,
    )?;
    let out = rig.command(&[
        "dispatch",
        "--manifest",
        manifest.to_str().ok_or("manifest")?,
        "--operation-id",
        "fixture-claude",
        "--wait",
        "5",
        "--json",
    ])?;
    assert_eq!(
        code(&out),
        0,
        "{} {}",
        String::from_utf8_lossy(&out.stdout.bytes),
        String::from_utf8_lossy(&out.stderr.bytes)
    );
    let lanes = rig.command(&["lanes", "--all", "--json"])?;
    let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
    let lane = &lanes["data"]["lanes"][0];
    if !cfg!(target_os = "macos") {
        assert_eq!(lane["state"], "rejected", "{lane}");
        assert!(
            lane["reason"]
                .as_str()
                .is_some_and(|r| r.contains("UnsupportedPlatform")),
            "{lane}"
        );
        assert!(account.join("refreshed").exists());
        rig.assert_legacy_unchanged()?;
        cleanup.disarm();
        return Ok(());
    }
    assert_eq!(lane["state"], "succeeded", "{lane}");
    assert_eq!(lane["account"], "pro");
    if pool {
        assert_eq!(lane["requested_route"]["kind"], "review");
        assert_eq!(
            lane["applied_route"]["pool_selection"]["pool"],
            "secret-fixture"
        );
        let receipt = serde_json::to_string(&lane["resolved_route"])?;
        assert!(!receipt.contains("prompt-sentinel-2719-indigo-owl"));
        assert!(!receipt.contains("credential-sentinel-2719-violet-fox"));
    }
    assert_eq!(lane["observed_route"]["model"], "claude/claude-sonnet-5-5");
    if host_native {
        assert_eq!(lane["requested_route"]["provider"], "host-native");
        assert_eq!(lane["applied_route"]["host_native"], "ci-fixer");
        assert_eq!(
            lane["applied_route"]["source"],
            "domains.fabric.host_native.ci-fixer"
        );
        let resume_prompt = rig.root.join("resume-prompt.md");
        fs::write(&resume_prompt, "continue")?;
        let resumed = rig.command(&[
            "resume",
            lane["attempt_id"].as_str().ok_or("attempt")?,
            "--prompt-file",
            resume_prompt.to_str().ok_or("resume prompt")?,
            "--no-fallback",
            "--json",
        ])?;
        assert_eq!(
            code(&resumed),
            1,
            "{}",
            String::from_utf8_lossy(&resumed.stdout.bytes)
        );
        let refusal: Value = serde_json::from_slice(&resumed.stdout.bytes)?;
        assert_eq!(
            refusal["error"]["code"], "CONTINUATION_UNSUPPORTED",
            "{refusal}"
        );
    }
    assert!(account.join("refreshed").exists());
    let attempt = rig
        .state
        .join("runs")
        .join(lane["run_id"].as_str().ok_or("run")?)
        .join("attempts")
        .join(lane["attempt_id"].as_str().ok_or("attempt")?);
    if pool {
        // The sentinel does reach the attempt, so its absence below is evidence.
        assert!(
            fs::read_to_string(attempt.join("prompt.txt"))?
                .contains("prompt-sentinel-2719-indigo-owl")
        );
        // Every launched attempt writes these, so a missing one fails the
        // test rather than hiding a leak. prompt.txt holds the prompt by design.
        for name in [
            "launch.json",
            "process.json",
            "terminal-commit.json",
            "result.md",
        ] {
            let artifact = fs::read_to_string(attempt.join(name))?;
            assert!(
                !artifact.contains("prompt-sentinel-2719-indigo-owl"),
                "{name}"
            );
            assert!(
                !artifact.contains("credential-sentinel-2719-violet-fox"),
                "{name}"
            );
        }
    }
    let authority: Value =
        serde_json::from_slice(&fs::read(attempt.join("credential-authority.json"))?)?;
    assert_eq!(authority["actor"], lane["run_id"]);
    assert_eq!(
        authority["source"]["path"],
        account.join(".credentials.json").to_str().ok_or("source")?
    );
    assert_eq!(authority["purpose"], "lane dispatch");
    for name in [
        "launch.json",
        "attempt.json",
        "stdout.log",
        "stderr.log",
        "credential-authority.json",
    ] {
        if let Ok(bytes) = fs::read(attempt.join(name)) {
            assert!(
                !String::from_utf8_lossy(&bytes).contains("credential-sentinel-2719-violet-fox")
            );
        }
    }
    rig.assert_legacy_unchanged()?;
    cleanup.disarm();
    Ok(())
}

/// Whether graph fixtures can launch confined providers here.
fn graph_fixture_available() -> Result<bool> {
    if !cfg!(target_os = "macos") {
        provenant_test_support::skip_sandbox_dependent_test(
            "graph fixture skipped: native confinement requires macOS",
        )?;
        return Ok(false);
    }
    let executor = provenant_os::exec::SystemExecutor::new(1)?;
    let cancel = provenant_core::CancelToken::new();
    if let Err(error) = provenant_confinement::permissions::system_backend(
        &provenant_confinement::permissions::BackendContext {
            executor: &executor,
            cancel: &cancel,
            #[cfg(target_os = "macos")]
            env: &[],
        },
    ) {
        provenant_test_support::skip_sandbox_dependent_test(format!(
            "graph fixture skipped: {error}"
        ))?;
        return Ok(false);
    }
    Ok(true)
}

/// The preferred fake vanishes after its version probe. Its pool peer stays
/// available, so a successful writer must actually start that peer.
fn writer_pool_redraw_fixture() -> Result<TestRig> {
    let mut rig = TestRig::new()?;
    let bin = rig.root.join("provider/bin");
    fs::create_dir_all(&bin)?;
    let cursor = bin.join("cursor-agent");
    fs::write(
        &cursor,
        "#!/bin/sh\nif [ \"$1\" = --version ]; then /bin/rm \"$0\"; printf 'cursor fixture 1.0\\n'; exit 0; fi\nexit 1\n",
    )?;
    fs::set_permissions(&cursor, fs::Permissions::from_mode(0o700))?;
    rig.copy_executable(&std::env::current_exe()?, &bin.join("opencode"))?;
    // The fake stream reports ses_x; model identity comes from the session DB.
    let data = rig.home.join(".local/share/opencode");
    fs::create_dir_all(&data)?;
    let db = rusqlite::Connection::open(data.join("opencode.db"))?;
    db.execute_batch(
        r#"CREATE TABLE session_message (session_id TEXT, type TEXT, data TEXT);
        INSERT INTO session_message VALUES ('ses_x', 'assistant',
        '{"model":{"providerID":"opencode-go","id":"deepseek-v4.1-flash"},"finish":"stop"}');"#,
    )?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    writeln!(
        OpenOptions::new()
            .append(true)
            .open(rig.state.join("config.toml"))?,
        "\n[domains.routing]\nsettlement_retries=1\n[domains.routing.pools]\nwriter-redraw=[{{model='cursor/gpt-5.3-codex-low',weight=1}},{{model='opencode/opencode-go/deepseek-v4.1-flash',weight=0}}]"
    )?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let commit = rig.execute(&git, &["commit", "--allow-empty", "-qm", "base"], None)?;
    assert_eq!(code(&commit), 0, "{commit:?}");
    Ok(rig)
}

fn assert_writer_pool_peer_launched(lanes: &Value) -> Result {
    let rows = lanes["data"]["lanes"].as_array().ok_or("lanes")?;
    let lane = rows
        .iter()
        .find(|row| row["applied_route"]["provider"] == "opencode")
        .ok_or_else(|| format!("second pool member did not launch: {lanes}"))?;
    assert_eq!(lane["state"], "succeeded", "{lanes}");
    assert_eq!(
        lane["observed_route"]["model"], "opencode/opencode-go/deepseek-v4.1-flash",
        "{lanes}"
    );
    assert_eq!(
        lane["observed_route"]["source"], "opencode_session",
        "{lanes}"
    );
    let skipped = lane["skipped_routes"].as_array().ok_or("skipped routes")?;
    assert!(
        skipped.iter().any(|row| row["route"]
            .as_str()
            .is_some_and(|route| route.starts_with("cursor/"))),
        "{lanes}"
    );
    Ok(())
}

fn direct_writer_launches_second_eligible_pool_member() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = writer_pool_redraw_fixture()?;
    let cleanup = rig.process_cleanup();
    let prompt = rig.root.join("prompt.txt");
    fs::write(&prompt, "reply OK")?;
    let started = rig.command(&[
        "dispatch",
        "--operation-id",
        "writer-redraw-direct",
        "--route",
        "writer-redraw",
        "--mode",
        "worktree-write",
        "--worktree-branch",
        "fixture/redraw",
        "--base",
        "HEAD",
        "--deliver",
        "none",
        "--prompt-file",
        prompt.to_str().ok_or("prompt")?,
        "--wait",
        "30s",
        "--json",
    ])?;
    assert_eq!(code(&started), 0, "{started:?}");
    let started: Value = serde_json::from_slice(&started.stdout.bytes)?;
    let run = started["data"]["run_id"].as_str().ok_or("run id")?;
    let lanes = rig.command(&["lanes", "--id", run, "--all", "--json"])?;
    assert_eq!(code(&lanes), 0, "{lanes:?}");
    assert_writer_pool_peer_launched(&serde_json::from_slice(&lanes.stdout.bytes)?)?;
    cleanup.disarm();
    Ok(())
}

fn graph_writer_launches_second_eligible_pool_member() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = writer_pool_redraw_fixture()?;
    let cleanup = rig.process_cleanup();
    let created = rig.command(&[
        "worktree",
        "create",
        "graph-redraw",
        "--base",
        "HEAD",
        "--json",
    ])?;
    assert_eq!(code(&created), 0, "{created:?}");
    fs::write(
        rig.project.join("graph.json"),
        serde_json::to_vec(&json!({
            "schema_version":1,"graph_id":"writer-redraw","nodes":[{
                "id":"write","prompt":"reply OK","access":"worktree_write",
                "worktree":".worktrees/graph-redraw","delivery":"none","route":{"alias":"writer-redraw"}
            }]
        }))?,
    )?;
    let started = rig.command(&[
        "fabric",
        "graph",
        "run",
        "--document",
        "graph.json",
        "--json",
    ])?;
    assert_eq!(code(&started), 0, "{started:?}");
    let status = provenant_test_support::wait_for(
        "writer pool redraw settlement",
        std::time::Duration::from_secs(30),
        || -> Result<Value> {
            let out = rig.command(&["fabric", "graph", "status", "writer-redraw", "--json"])?;
            assert_eq!(code(&out), 0, "{out:?}");
            Ok(serde_json::from_slice(&out.stdout.bytes)?)
        },
        |result| {
            result.as_ref().is_ok_and(|status| {
                status["data"]["status"] == "complete" || status["data"]["status"] == "failed"
            })
        },
    )??;
    assert_eq!(status["data"]["status"], "complete", "{status}");
    assert_eq!(
        status["data"]["checkpoint"]["nodes"][0]["attempts"][0]["launched"], true,
        "{status}"
    );
    let lanes = rig.command(&["lanes", "--all", "--json"])?;
    assert_eq!(code(&lanes), 0, "{lanes:?}");
    assert_writer_pool_peer_launched(&serde_json::from_slice(&lanes.stdout.bytes)?)?;
    cleanup.disarm();
    Ok(())
}

fn graph_without_deadline_launches_on_tick() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    use provenant_core::{CancelToken, Clock, RootInputs, WaitBudget};
    use provenant_fabric_runtime::graph::{self, GraphJournal};
    struct Seed;
    impl graph::AttemptOwner for Seed {
        type Error = std::convert::Infallible;
        fn actor(&self) -> String {
            "fixture".into()
        }
        fn observe_or_start(
            &mut self,
            _: &graph::AttemptKey,
            _: graph::NodeInput<'_>,
            _: &graph::GraphRoute,
            _: graph::AccessMode,
            _: Option<&str>,
            _: graph::Replay<'_>,
        ) -> std::result::Result<graph::AttemptObservation, Self::Error> {
            Ok(graph::AttemptObservation::Pending { attempt_ref: None })
        }
    }
    let rig = claude_fixture()?;
    let cleanup = rig.process_cleanup();
    let config_path = rig.state.join("config.toml");
    let config = fs::read_to_string(&config_path)?;
    fs::write(
        &config_path,
        config.replace("[gc]\n", "[gc]\nschedule=false\n"),
    )?;
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let project = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(rig.project.join(".git"))?,
    )?
    .project_id();
    let document: graph::GraphDocument = serde_json::from_value(json!({
        "schema_version":1,"graph_id":"tick-waiter","nodes":[{"id":"one","prompt":"reply OK",
            "access":"read_only","route":{"adapter":"claude","model":"claude/claude-sonnet-5-5","effort":"medium","account":"pro"}}]
    }))?;
    let clock = provenant_os::clock::SystemClock;
    let cancel = CancelToken::new();
    let mut journal = graph::FileJournal::open(
        &roots.state,
        project,
        "tick-waiter",
        WaitBudget::cli(clock.mono_now(), 10)?,
        &cancel,
        &clock,
    )?;
    journal.bind(graph::GraphExecution {
        review_base: None,
        extra: Default::default(),
        integration_base: None,
        directory: rig.project.clone(),
        document: document.clone(),
    })?;
    let mut seeded = graph::advance_with_budget(
        &document,
        &mut Seed,
        &mut journal,
        &Default::default(),
        &Default::default(),
    )?;
    seeded.checkpoint.nodes[0].state = graph::NodeState::Pending;
    seeded.checkpoint.nodes[0].attempts.clear();
    seeded.checkpoint.wake_at = None;
    journal.save(&seeded.checkpoint)?;
    drop(journal);
    let tick = rig.command(&["tick", "--json"])?;
    assert_eq!(
        code(&tick),
        0,
        "{} {}",
        String::from_utf8_lossy(&tick.stdout.bytes),
        String::from_utf8_lossy(&tick.stderr.bytes)
    );
    let tick: Value = serde_json::from_slice(&tick.stdout.bytes)?;
    assert_eq!(tick["data"]["graphs"]["advanced"], 1, "{tick}");
    let checkpoint = graph::FileJournal::read_checkpoint(&roots.state, project, "tick-waiter")?
        .ok_or("checkpoint")?;
    assert!(
        checkpoint.nodes[0]
            .attempts
            .iter()
            .any(|attempt| attempt.launched)
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let checkpoint = graph::FileJournal::read_checkpoint(&roots.state, project, "tick-waiter")?
            .ok_or("checkpoint")?;
        if checkpoint.nodes[0].state == graph::NodeState::Succeeded {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "node did not finish: {:?}",
            checkpoint.nodes[0].state
        );
        std::thread::park_timeout(std::time::Duration::from_millis(50));
    }
    cleanup.disarm();
    Ok(())
}

fn graph_read_only_alias_does_not_redraw_after_launch_quota() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = policy_codex_fixture()?;
    let cleanup = rig.process_cleanup();
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.routing.pools]\nretry-test=[{model='codex/gpt-6-luna',weight=1,effort='low'},{model='codex/gpt-6.1-terra',weight=0,effort='low'}]\n",
    )?;
    for (label, account) in [("one", "fixture-one"), ("two", "fixture-two")] {
        let directory = rig.root.join(format!("codex-{label}"));
        fs::create_dir_all(&directory)?;
        let auth = serde_json::to_vec(&json!({"tokens":{"account_id":account}}))?;
        fs::write(directory.join("auth.json"), &auth)?;
        let key = provenant_providers::identity::codex_account(&auth).ok_or("account key")?;
        support::register_account(&rig, "codex", label, &key, &directory)?;
    }
    fs::write(
        rig.project.join("graph.json"),
        serde_json::to_vec(&json!({
            "schema_version":1,"graph_id":"quota-redraw","nodes":[{
                "id":"read","prompt":"fixture graph redraw","access":"read_only","route":{"alias":"retry-test"}
            }]
        }))?,
    )?;
    let started = rig.command(&[
        "fabric",
        "graph",
        "run",
        "--document",
        "graph.json",
        "--json",
    ])?;
    assert_eq!(code(&started), 0, "{started:?}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let status = loop {
        let status = rig.command(&["fabric", "graph", "status", "quota-redraw", "--json"])?;
        assert_eq!(code(&status), 0, "{status:?}");
        let status: Value = serde_json::from_slice(&status.stdout.bytes)?;
        if status["data"]["checkpoint"]["nodes"][0]["state"] == "failed" {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "graph did not settle: {status}"
        );
        std::thread::park_timeout(std::time::Duration::from_millis(50));
    };
    let attempts = status["data"]["checkpoint"]["nodes"][0]["attempts"]
        .as_array()
        .ok_or("attempts")?;
    assert_eq!(attempts.len(), 1, "{status}");
    let lanes = rig.command(&["lanes", "--all", "--json"])?;
    assert_eq!(code(&lanes), 0, "{lanes:?}");
    let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
    let rows = lanes["data"]["lanes"].as_array().ok_or("lanes")?;
    assert_eq!(rows.len(), 1, "{lanes}");
    assert_eq!(rows[0]["state"], "quota_limited", "{lanes}");
    cleanup.disarm();
    Ok(())
}

fn review_overcap_runs_through_fake_providers() -> Result {
    if !graph_fixture_available().map_err(|e| format!("fixture availability: {e}"))? {
        return Ok(());
    }
    let mut rig = claude_fixture().map_err(|e| format!("claude fixture: {e}"))?;
    let cleanup = rig.process_cleanup();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    rig.env.retain(|(key, _)| key != "PROVENANT_ROOT");
    rig.env
        .push(("PROVENANT_ROOT".into(), root.into_os_string()));
    for path in [
        ".cargo/bin",
        ".cargo/registry",
        ".cargo/git",
        "Library/pnpm",
        "Library/Caches/pnpm",
        ".cache/node",
        ".rustup/toolchains",
        ".codex",
        ".claude/hooks",
        ".kiro/settings",
        ".gemini/config",
        ".cursor/extensions",
        ".npm",
    ] {
        fs::create_dir_all(rig.home.join(path))?;
    }
    fs::write(rig.home.join(".rustup/settings.toml"), "")?;
    writeln!(
        OpenOptions::new()
            .append(true)
            .open(rig.state.join("config.toml"))?,
        "\n[domains.routing.pools]\nsenior = [{{ model = 'claude/claude-sonnet-5-5', effort = 'medium' }}]\nprincipal = [{{ model = 'claude/claude-sonnet-5-5', effort = 'medium' }}]\ngraph-test = [{{ model = 'claude/claude-sonnet-5-5', effort = 'medium' }}]"
    )?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    assert_eq!(
        code(&rig.execute(&git, &["commit", "--allow-empty", "-qm", "Initial"], None)?),
        0
    );
    let prompt = rig.root.join("council-brief.txt");
    fs::write(&prompt, "fixture-council-overcap")?;
    let worktree = format!("parameters.worktree='{}'", rig.project.display());
    let lenses = "parameters.lenses=[{name='correctness-spec',tier='graph-test',when=''},{name='tests',tier='graph-test',when=''},{name='simplicity',tier='graph-test',when=''},{name='reliability',tier='graph-test',when=''}]";
    let out = rig
        .command(&[
            "run",
            "--preset",
            "review",
            "--prompt-file",
            prompt.to_str().ok_or("brief")?,
            "--set",
            &worktree,
            "--set",
            lenses,
            "--set",
            "parameters.verifier_tier='graph-test'",
            "--wait",
            "1s",
            "--json",
        ])
        .map_err(|e| format!("start preset: {e}"))?;
    let value: Value = serde_json::from_slice(&out.stdout.bytes).map_err(|e| {
        format!(
            "parse preset: {e}: {} {}",
            String::from_utf8_lossy(&out.stdout.bytes),
            String::from_utf8_lossy(&out.stderr.bytes)
        )
    })?;
    let id = value["data"]["graph_id"]
        .as_str()
        .ok_or_else(|| format!("graph id: {value}"))?
        .to_owned();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let response = rig.command(&["fabric", "graph", "status", &id, "--json"])?;
        let status: Value = serde_json::from_slice(&response.stdout.bytes)?;
        if status["data"]["status"] == "complete" {
            let nodes = status["data"]["checkpoint"]["nodes"]
                .as_array()
                .ok_or("nodes")?;
            for id in ["lenses", "verify", "synthesis"] {
                let node = nodes.iter().find(|node| node["id"] == id).ok_or("stage")?;
                assert_eq!(node["state"], "succeeded", "{status}");
                if id != "lenses" {
                    assert!(
                        node["outputs"]["result"]["excerpt"]
                            .as_str()
                            .unwrap_or("")
                            .contains("read 40 findings"),
                        "{status}"
                    );
                }
            }
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{status}");
        std::thread::park_timeout(std::time::Duration::from_millis(100));
    }
    cleanup.disarm();
    Ok(())
}

fn graph_settlement_records_a_failed_advance_for_status() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = claude_fixture()?;
    let cleanup = rig.process_cleanup();
    for path in [
        ".cargo/bin",
        ".cargo/registry",
        ".cargo/git",
        "Library/pnpm",
        "Library/Caches/pnpm",
        ".cache/node",
        ".rustup/toolchains",
        ".codex",
        ".claude/hooks",
        ".kiro/settings",
        ".gemini/config",
        ".cursor/extensions",
        ".npm",
    ] {
        fs::create_dir_all(rig.home.join(path))?;
    }
    fs::write(rig.home.join(".rustup/settings.toml"), "")?;
    let route = json!({"adapter":"claude","model":"claude/claude-sonnet-5-5","effort":"medium","account":"pro"});
    // The dependant names a worktree that was never created, so starting it
    // fails when the first node settles and the guardian advances the graph.
    fs::write(
        rig.project.join("graph.json"),
        serde_json::to_vec(&json!({"schema_version":1,"graph_id":"broken","nodes":[
            {"id":"first","prompt":"reply OK","access":"read_only","route":route},
            {"id":"second","prompt":"reply OK","access":"worktree_write","worktree":".worktrees/missing","depends_on":["first"],"route":route}
        ]}))?,
    )?;
    let started = rig.command(&[
        "fabric",
        "graph",
        "run",
        "--document",
        "graph.json",
        "--json",
    ])?;
    assert_eq!(
        code(&started),
        0,
        "{}",
        String::from_utf8_lossy(&started.stdout.bytes)
    );
    // Status only observes: the guardian's advance is the sole writer here.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let error = loop {
        let status = rig.command(&["fabric", "graph", "status", "broken", "--json"])?;
        assert_eq!(code(&status), 0);
        let status: Value = serde_json::from_slice(&status.stdout.bytes)?;
        if let Some(error) = status["data"]["advance_error"].as_str() {
            break error.to_owned();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no advance error recorded: {status}"
        );
        std::thread::park_timeout(std::time::Duration::from_millis(50));
    };
    assert!(!error.is_empty());
    let human = rig.command(&["fabric", "graph", "status", "broken"])?;
    assert!(
        String::from_utf8_lossy(&human.stdout.bytes).contains(&error),
        "{}",
        String::from_utf8_lossy(&human.stdout.bytes)
    );
    cleanup.disarm();
    Ok(())
}

/// A node whose upstream result is far past the excerpt budget reads the whole
/// of it from the file the host materialised in its own inputs directory, and
/// nothing else beside it.
fn graph_node_reads_full_upstream_result_from_its_inputs_directory() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = claude_fixture()?;
    let cleanup = rig.process_cleanup();
    for path in [
        ".cargo/bin",
        ".cargo/registry",
        ".cargo/git",
        "Library/pnpm",
        "Library/Caches/pnpm",
        ".cache/node",
        ".rustup/toolchains",
        ".codex",
        ".claude/hooks",
        ".kiro/settings",
        ".gemini/config",
        ".cursor/extensions",
        ".npm",
    ] {
        fs::create_dir_all(rig.home.join(path))?;
    }
    fs::write(rig.home.join(".rustup/settings.toml"), "")?;
    let route = json!({"adapter":"claude","model":"claude/claude-sonnet-5-5","effort":"medium","account":"pro"});
    fs::write(
        rig.project.join("graph.json"),
        serde_json::to_vec(
            &json!({"schema_version":1,"graph_id":"full-inputs","nodes":[
                {"id":"producer","prompt":"fixture large result","access":"read_only","route":route},
                {"id":"Review","prompt":"fixture case result Review","access":"read_only","route":route},
                {"id":"review","prompt":"fixture case result review","access":"read_only","route":route},
                {"id":"consumer","prompt":"fixture read upstream file","access":"read_only",
                 "depends_on":["producer","Review","review"],
                 "inputs":[{"node":"producer","outputs":["result"]},
                           {"node":"Review","outputs":["result"]},
                           {"node":"review","outputs":["result"]}],"route":route}
            ]}),
        )?,
    )?;
    let started = rig.command(&[
        "fabric",
        "graph",
        "run",
        "--document",
        "graph.json",
        "--json",
    ])?;
    assert_eq!(
        code(&started),
        0,
        "{}",
        String::from_utf8_lossy(&started.stdout.bytes)
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let status = loop {
        let status = rig.command(&["fabric", "graph", "status", "full-inputs", "--json"])?;
        assert_eq!(code(&status), 0, "{status:?}");
        let status: Value = serde_json::from_slice(&status.stdout.bytes)?;
        if status["data"]["status"] == "complete" {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "graph did not complete: {status}"
        );
        std::thread::park_timeout(std::time::Duration::from_millis(50));
    };
    let nodes = status["data"]["checkpoint"]["nodes"]
        .as_array()
        .ok_or("nodes")?;
    let producer = &nodes[0]["outputs"]["result"];
    assert_eq!(producer["truncated"], true, "{status}");
    assert!(producer["excerpt"].as_str().ok_or("excerpt")?.len() <= 4096);
    let consumer = nodes
        .iter()
        .find(|node| node["id"] == "consumer")
        .ok_or("consumer")?;
    let excerpt = consumer["outputs"]["result"]["excerpt"]
        .as_str()
        .ok_or("consumer excerpt")?;
    let full = (0..2000)
        .map(|n| format!("finding {n:04} body\n"))
        .collect::<String>()
        + "END-OF-LARGE-RESULT";
    assert_eq!(
        without_done(excerpt),
        format!(
            "read {} bytes; tail END-OF-LARGE-RESULT; sibling write denied true; parent listing denied true; Review=case-result-Review; review=case-result-review",
            full.len()
        ),
        "{status}"
    );
    cleanup.disarm();
    Ok(())
}

fn graph_retries_real_registry_lock_and_runs_writer() -> Result {
    use provenant_core::Clock;
    use provenant_os::fs::{Directory, LockMode};
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = claude_fixture()?;
    let cleanup = rig.process_cleanup();
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let commit = rig.execute(&git, &["commit", "--allow-empty", "-qm", "base"], None)?;
    assert_eq!(code(&commit), 0);
    let create = rig.command(&[
        "worktree",
        "create",
        "graph-lock-writer",
        "--base",
        "HEAD",
        "--json",
    ])?;
    assert_eq!(
        code(&create),
        0,
        "{}",
        String::from_utf8_lossy(&create.stderr.bytes)
    );
    let head = rig.execute(&git, &["rev-parse", "HEAD"], None)?;
    let head = std::str::from_utf8(&head.stdout.bytes)?.trim();
    let graph = json!({"schema_version":1,"graph_id":"real-lock-provider","nodes":[{
        "id":"write","prompt":"reply OK","access":"worktree_write","worktree":".worktrees/graph-lock-writer",
        "source":{"type":"commit","commit":head},
        "route":{"adapter":"claude","model":"claude/claude-sonnet-5-5","effort":"medium","account":"pro"}
    }]});
    fs::write(rig.project.join("graph.json"), serde_json::to_vec(&graph)?)?;
    let registry_root = rig.state.join("worktrees");
    let registry = fs::read_dir(registry_root)?
        .next()
        .ok_or("registry")??
        .path();
    let directory = Directory::open_anchor(&registry)?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let lock = directory.lock(
        std::ffi::OsStr::new("worktrees.lock"),
        LockMode::Exclusive,
        provenant_core::WaitBudget::cli(clock.mono_now(), 30)?,
        &cancel,
        &clock,
    )?;
    let first = rig.command(&[
        "fabric",
        "graph",
        "run",
        "--document",
        "graph.json",
        "--json",
    ])?;
    assert_eq!(
        code(&first),
        0,
        "{} {}",
        String::from_utf8_lossy(&first.stdout.bytes),
        String::from_utf8_lossy(&first.stderr.bytes)
    );
    let result: Value = serde_json::from_slice(&first.stdout.bytes)?;
    assert_eq!(
        result["data"]["checkpoint"]["nodes"][0]["state"], "running",
        "{result}"
    );
    assert!(
        result["data"]["checkpoint"]["nodes"][0]["launch_retry"]["wake_at"].is_string(),
        "{result}"
    );
    drop(lock);
    clock.wait_until(
        clock
            .mono_now()
            .checked_add(provenant_core::DurationMicros::from_micros(5_500_000))?,
    )?;
    let second = rig.command(&[
        "fabric",
        "graph",
        "resume",
        "--document",
        "graph.json",
        "--json",
    ])?;
    assert_eq!(
        code(&second),
        0,
        "{} {}",
        String::from_utf8_lossy(&second.stdout.bytes),
        String::from_utf8_lossy(&second.stderr.bytes)
    );
    let result: Value = serde_json::from_slice(&second.stdout.bytes)?;
    let node = &result["data"]["checkpoint"]["nodes"][0];
    assert_ne!(node["state"], "blocked", "{result}");
    assert_eq!(node["attempts"][0]["launched"], true, "{result}");
    cleanup.disarm();
    Ok(())
}

fn graph_settlement_advances_writer_and_account_routed_review() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = claude_fixture()?;
    let cleanup = rig.process_cleanup();
    writeln!(
        OpenOptions::new()
            .append(true)
            .open(rig.state.join("config.toml"))?,
        "\n[domains.routing.pools]\ngraph-test = [{{ model = 'claude/claude-sonnet-5-5' }}]"
    )?;
    let git = std::env::var_os("PROVENANT_GIT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/bin/git"));
    let commit = rig.execute(&git, &["commit", "--allow-empty", "-qm", "Initial"], None)?;
    assert_eq!(code(&commit), 0);
    let created = rig.command(&[
        "worktree",
        "create",
        "graph-writer",
        "--base",
        "HEAD",
        "--json",
    ])?;
    assert_eq!(
        code(&created),
        0,
        "{}",
        String::from_utf8_lossy(&created.stdout.bytes)
    );
    for path in [
        ".cargo/bin",
        ".cargo/registry",
        ".cargo/git",
        "Library/pnpm",
        "Library/Caches/pnpm",
        ".cache/node",
        ".rustup/toolchains",
        ".codex",
        ".claude/hooks",
        ".kiro/settings",
        ".gemini/config",
        ".cursor/extensions",
        ".npm",
    ] {
        fs::create_dir_all(rig.home.join(path))?;
    }
    fs::write(rig.home.join(".rustup/settings.toml"), "")?;
    let route = json!({"adapter":"claude","model":"claude/claude-sonnet-5-5","effort":"medium","account":"pro"});
    fs::write(
        rig.project.join("graph.json"),
        serde_json::to_vec(&json!({"schema_version":1,"graph_id":"settlement","nodes":[
            {"id":"write","title":"Write changes","prompt":"reply OK","access":"worktree_write","worktree":".worktrees/graph-writer","delivery":"none","route":route},
            {"id":"review","prompt":"reply OK","access":"read_only","depends_on":["write"],"route":{"alias":"graph-test","account":"pro"}}
        ]}))?,
    )?;
    let started = rig.command(&[
        "fabric",
        "graph",
        "run",
        "--document",
        "graph.json",
        "--json",
    ])?;
    assert_eq!(
        code(&started),
        0,
        "{}",
        String::from_utf8_lossy(&started.stdout.bytes)
    );
    // Status only observes. No resume or other mutating command drives the graph.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let status = rig.command(&["fabric", "graph", "status", "settlement", "--json"])?;
        assert_eq!(
            code(&status),
            0,
            "{} {}",
            String::from_utf8_lossy(&status.stdout.bytes),
            String::from_utf8_lossy(&status.stderr.bytes)
        );
        let status: Value = serde_json::from_slice(&status.stdout.bytes)?;
        if status["data"]["status"] == "complete" {
            let nodes = status["data"]["checkpoint"]["nodes"]
                .as_array()
                .ok_or("nodes")?;
            assert_eq!(nodes.len(), 2);
            for node in nodes {
                assert_eq!(
                    node["state"],
                    "succeeded",
                    "{status}; lanes={}",
                    String::from_utf8_lossy(
                        &rig.command(&["lanes", "--all", "--json"])?.stdout.bytes
                    )
                );
                assert_eq!(node["attempts"].as_array().ok_or("attempts")?.len(), 1);
            }
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "graph did not advance: {status}"
        );
        std::thread::park_timeout(std::time::Duration::from_millis(50));
    }
    let lanes = rig.command(&["lanes", "--all", "--json"])?;
    let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
    let lanes = lanes["data"]["lanes"].as_array().ok_or("lanes")?;
    assert_eq!(lanes.len(), 2);
    for lane in lanes {
        assert_eq!(lane["state"], "succeeded", "{lane}");
        assert_eq!(lane["account"], "pro", "{lane}");
    }
    let pool_lane = lanes
        .iter()
        .find(|lane| lane["applied_route"]["pool_selection"]["pool"] == "graph-test")
        .ok_or("pool route receipt")?;
    assert_eq!(pool_lane["applied_route"]["provider"], "claude");
    assert_eq!(pool_lane["title"], "review");
    assert!(lanes.iter().any(|lane| lane["title"] == "Write changes"));
    // Recreate a checkpoint saved before the terminal observation, after the
    // successful writer's checkout has already been pruned.
    assert_eq!(
        code(&rig.command(&["worktree", "remove", "graph-writer", "--json",])?),
        0
    );
    assert!(!rig.project.join(".worktrees/graph-writer").exists());
    {
        use provenant_core::{CancelToken, Clock, RootInputs, WaitBudget};
        use provenant_fabric_runtime::graph::{AttemptState, FileJournal, GraphJournal, NodeState};
        let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
        inputs.state_dir = Some(rig.state.clone());
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let project = provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(rig.project.join(".git"))?,
        )?
        .project_id();
        let clock = provenant_os::clock::SystemClock;
        let cancel = CancelToken::new();
        let mut journal = FileJournal::open(
            &roots.state,
            project,
            "settlement",
            WaitBudget::cli(clock.mono_now(), 5)?,
            &cancel,
            &clock,
        )?;
        let mut checkpoint = journal.load("settlement")?.ok_or("checkpoint")?;
        checkpoint.nodes[0].state = NodeState::Running;
        let attempt = checkpoint.nodes[0].attempts.last_mut().ok_or("attempt")?;
        attempt.state = AttemptState::Starting;
        attempt.outcome = None;
        journal.save(&checkpoint)?;
    }
    let replay = rig.command(&[
        "fabric",
        "graph",
        "resume",
        "--document",
        "graph.json",
        "--json",
    ])?;
    assert_eq!(
        code(&replay),
        0,
        "{}",
        String::from_utf8_lossy(&replay.stdout.bytes)
    );
    let replay: Value = serde_json::from_slice(&replay.stdout.bytes)?;
    assert_eq!(replay["data"]["status"], "complete", "{replay}");
    assert_eq!(
        replay["data"]["checkpoint"]["nodes"][0]["state"], "succeeded",
        "{replay}"
    );
    assert_eq!(
        replay["data"]["checkpoint"]["nodes"][0]["attempts"]
            .as_array()
            .ok_or("attempts")?
            .len(),
        1
    );
    cleanup.disarm();
    Ok(())
}

fn codex_capacity_settles_without_quota_refusal() -> Result {
    let message = "Selected model is at capacity. Please try a different model.";
    let stdout = format!(
        "{{\"type\":\"thread.started\",\"thread_id\":\"01234567-89ab-cdef-0123-456789abcdef\"}}\n{{\"type\":\"turn.failed\",\"error\":{{\"message\":\"{message}\",\"codex_error_info\":\"server_overloaded\"}}}}\n"
    );
    let (rig, terminal) = persisted_attempt_with(
        "codex",
        "codex/gpt-6.1-sol",
        ("work", "codex-fixture"),
        stdout.as_bytes(),
        b"",
        1,
        PersistedAttemptOptions {
            host_registry: true,
            ..Default::default()
        },
    )?;
    assert!(matches!(
        terminal.state,
        provenant_run::AttemptState::Terminal {
            outcome: provenant_run::Outcome::ModelUnavailable,
            ..
        }
    ));
    // A staged settlement retry appends its note after the typed reason.
    assert!(
        matches!(&terminal.state, provenant_run::AttemptState::Terminal { reason: Some(reason), .. } if reason.starts_with(&format!("provider capacity: {message}")))
    );
    let record = provenant_quota::AccountRecord::decode(&fs::read(
        rig.state.join("quota/codex-fixture/codex.json"),
    )?)?;
    assert!(record.refusals.is_empty());
    assert_eq!(record.capacity_cooldowns.len(), 1);
    assert_eq!(record.capacity_cooldowns[0].model, "gpt-6.1-sol");
    assert_eq!(
        record.capacity_cooldowns[0].account.account_key,
        "codex-fixture"
    );
    assert_eq!(
        record.capacity_cooldowns[0].expires_unix_seconds
            - record.capacity_cooldowns[0].observed_unix_seconds,
        600
    );
    Ok(())
}

/// Finish one attempt on `stdout` as provider output and return the quota
/// record its refusal persisted for the applied account.
fn persisted_refusal(
    provider: &str,
    model: &str,
    (label, key): (&str, &str),
    stdout: &[u8],
) -> Result<provenant_quota::AccountRecord> {
    let (rig, terminal) = persisted_attempt(provider, model, (label, key), stdout, b"", 1)?;
    assert!(matches!(
        terminal.state,
        provenant_run::AttemptState::Terminal {
            outcome: provenant_run::Outcome::QuotaLimited,
            ..
        }
    ));
    Ok(provenant_quota::AccountRecord::decode(&fs::read(
        rig.state.join(format!("quota/{key}/{provider}.json")),
    )?)?)
}

/// Publish sealed captures through the production holder and provider registry.
fn persisted_attempt(
    provider: &str,
    model: &str,
    (label, key): (&str, &str),
    stdout: &[u8],
    stderr: &[u8],
    exit_code: i64,
) -> Result<(TestRig, provenant_run::AttemptRecord)> {
    persisted_attempt_with(
        provider,
        model,
        (label, key),
        stdout,
        stderr,
        exit_code,
        PersistedAttemptOptions::default(),
    )
}

#[derive(Default)]
struct PersistedAttemptOptions {
    host_registry: bool,
    writer: bool,
    extra_task: bool,
    nonterminal: bool,
    lost_before_route: bool,
    stalled_import: bool,
    stop_reason: Option<provenant_supervisor::machine::StopReason>,
}

fn persisted_attempt_with(
    provider: &str,
    model: &str,
    (label, key): (&str, &str),
    stdout: &[u8],
    stderr: &[u8],
    exit_code: i64,
    options: PersistedAttemptOptions,
) -> Result<(TestRig, provenant_run::AttemptRecord)> {
    use provenant_core::{
        ActorId, AttemptId, CancelToken, Clock, Digest, OperationId, RootInputs, RunId, TaskId,
    };
    use provenant_os::clock::SystemClock;
    use provenant_os::exec::Termination;
    use provenant_os::fs::Directory;
    use provenant_records::{
        CanonicalV1,
        run::{CanonicalRequest, HarnessProfile, RequestedRoute, RouteRecord, TaskRequest},
    };
    use provenant_run::{ExecutionContext, NewAttempt, PreparedRun, Request, RunStore};
    use provenant_supervisor::custody::CaptureTotals;
    use provenant_supervisor::custody::ExitObservation;
    use provenant_supervisor::custody::PublicationState;
    use provenant_supervisor::custody::Scope;
    use provenant_supervisor::custody::SettlementEvidence;

    let rig = TestRig::new()?;
    if options.stalled_import {
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        let seeded = rig.execute(&git, &["commit", "--allow-empty", "-qm", "seed"], None)?;
        assert_eq!(code(&seeded), 0);
        let created = rig.command(&[
            "worktree",
            "create",
            "receipt-writer",
            "--base",
            "HEAD",
            "--json",
        ])?;
        assert_eq!(
            code(&created),
            0,
            "{}",
            String::from_utf8_lossy(&created.stdout.bytes)
        );
    }
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    fs::create_dir_all(&rig.state)?;
    fs::set_permissions(&rig.state, fs::Permissions::from_mode(0o700))?;
    let project_id = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(rig.project.join(".git"))?,
    )?
    .project_id();
    let actor_id = ActorId::from_bytes([22; 16]);
    let execution = ExecutionContext {
        state_root: roots.state.clone(),
        project_id,
        actor_id,
        read_prompts: true,
    };
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = RunStore::open(&execution, &call)?;
    let run_id = RunId::from_bytes([23; 16]);
    let attempt_id = AttemptId::from_bytes([24; 16]);
    let task_id = TaskId::from_bytes([25; 16]);
    let applied = RouteRecord {
        extra: Default::default(),
        provider: Some(provider.into()),
        model: Some(model.into()),
        effort: None,
        account_label: Some(label.into()),
        account_key: Some(key.into()),
        source: Some("fixture_route".into()),
        pool_selection: None,
    };
    let prompt = "reply OK";
    let mut prepared = PreparedRun {
        run_id,
        operation_id: OperationId::parse(&format!("{provider}-cooldown"))?,
        request: CanonicalRequest {
            extra: Default::default(),
            child_attempt_id_version: None,
            chair: None,
            fresh_continuation: None,
            schema_version: 1,
            tasks: vec![TaskRequest {
                context: None,
                extra: Default::default(),
                build_heavy: false,
                title: None,
                base: None,
                head: None,
                source_head: None,
                base_ref: None,
                profile: HarnessProfile::default(),
                task_id,
                prompt_digest: CanonicalV1::file_digest(prompt.as_bytes()),
                route: RequestedRoute::Model {
                    extra: Default::default(),
                    provider: provider.into(),
                    model: model.into(),
                    effort: None,
                },
                account: None,
                fallback: vec![],
                authority_digest: Digest::from_bytes([26; 32]),
                writer: None,
                provider_timeout_seconds: 60,
                delivery_id: None,
                deliver: None,
                access: Default::default(),
            }],
            concurrency: 1,
            parent: None,
            lane_depth: None,
            parent_attempt: None,
        },
        attempts: vec![NewAttempt {
            task_id,
            attempt_id,
            prompt: prompt.into(),
        }],
        created_at: clock.wall_now()?,
    };
    if options.writer {
        prepared.request.tasks[0].writer = Some(provenant_records::run::WriterRequest {
            extra: Default::default(),
            project: rig.project.clone(),
            worktree: rig.project.join(".worktrees/receipt-writer"),
        });
    }
    if options.extra_task {
        let mut task = prepared.request.tasks[0].clone();
        task.task_id = TaskId::from_bytes([27; 16]);
        prepared.attempts.push(NewAttempt {
            task_id: task.task_id,
            attempt_id: AttemptId::from_bytes([28; 16]),
            prompt: prompt.into(),
        });
        prepared.request.tasks.push(task);
    }
    store.create(&prepared, &call)?;
    let started_at = clock.wall_now()?;
    let mut running = store.record(attempt_id, &call)?;
    running.revision = 2;
    if options.stalled_import {
        running.generation = 1;
    }
    running.resolved_route = Some(applied.clone());
    running.applied_route = Some(applied.clone());
    running.started_at = Some(started_at);
    running.provider_launched = Some(true);
    running.state = provenant_run::AttemptState::Running {
        extra: Default::default(),
    };
    let running_bytes = CanonicalV1::bytes(&running)?.as_bytes().to_vec();
    let database = rusqlite::Connection::open(roots.state.as_path().join("run.sqlite3"))?;
    database.execute(
        "UPDATE attempts SET revision=?1,record=?2 WHERE id=?3",
        rusqlite::params![2_i64, running_bytes, attempt_id.to_string()],
    )?;

    if options.nonterminal && !options.stalled_import {
        return Ok((rig, running));
    }

    if options.lost_before_route {
        running.applied_route = None;
        running.resolved_route = None;
        running.state = provenant_run::AttemptState::Terminal {
            extra: Default::default(),
            outcome: provenant_run::Outcome::Lost,
            exit_code: None,
            finished_at: clock.wall_now()?,
            reason: Some("lost before recording a route".into()),
        };
        // Version 2 records carry the evidence that justifies the decision.
        running.terminal_evidence = Some(provenant_run::TerminalEvidence::Lost {
            reason: "lost before recording a route".into(),
        });
        let bytes = CanonicalV1::bytes(&running)?.as_bytes().to_vec();
        database.execute(
            "UPDATE attempts SET terminal=1,record=?1 WHERE id=?2",
            rusqlite::params![bytes, attempt_id.to_string()],
        )?;
        return Ok((rig, running));
    }
    let attempt_dir = roots
        .state
        .as_path()
        .join("runs")
        .join(run_id.to_string())
        .join("attempts")
        .join(attempt_id.to_string());
    let provider_state = attempt_dir.join("provider-state");
    fs::create_dir_all(&provider_state)?;
    let evidence = SettlementEvidence::DarwinCensusAndCustodyLease;
    let exit = ExitObservation {
        publication: PublicationState::Durable,
        scope: Scope {
            run: run_id,
            attempt: attempt_id,
            generation: running.generation,
        },
        termination: Some(Termination::Exit(exit_code)),
        never_spawned: false,
        stop_reason: options.stop_reason,
        delivered_signals: vec![],
        reaped: vec![],
        omitted_reaped: 0,
        spared: vec![],
        cancel_delivered_before_exit: None,
        omitted_spared: 0,
        warnings: vec![],
        omitted_warnings: 0,
        terminal_observed: true,
        unresolved_cleanup: false,
        unresolved_pids: vec![],
        settlement_evidence: evidence,
        stdout: CaptureTotals::default(),
        stderr: CaptureTotals::default(),
    };
    let mut sealed_exit = serde_json::to_value(&exit)?;
    let fields = sealed_exit.as_object_mut().ok_or("exit fixture")?;
    fields.remove("publication");
    fields.insert("schema_version".into(), 1.into());
    let exit_bytes = serde_json::to_vec(&sealed_exit)?;
    let mut exit_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(attempt_dir.join("process-exit.json"))?;
    exit_file.write_all(&exit_bytes)?;
    exit_file.sync_all()?;

    // Fixture provider output enters Registry.decode through RunStore::finish.
    let mut stdout_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(attempt_dir.join("stdout.log"))?;
    stdout_file.write_all(stdout)?;
    stdout_file.sync_all()?;
    let mut stderr_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(attempt_dir.join("stderr.log"))?;
    stderr_file.write_all(stderr)?;
    stderr_file.sync_all()?;
    let attempt = Directory::open_anchor(&attempt_dir)?;
    if options.stalled_import {
        use std::os::unix::fs::MetadataExt;
        let writer = prepared.request.tasks[0].writer.as_ref().ok_or("writer")?;
        let slot = rig
            .state
            .join("worktrees")
            .join(project_id.to_string())
            .join(provenant_confinement::worktrees::state_dir_name(
                &writer.worktree,
            ));
        fs::create_dir_all(&slot)?;
        let scope = exit.scope;
        let launch = provenant_run::prepare_writer(writer, &rig.state, project_id)?;
        let head = rig.execute(
            &provenant_os::git::git_executable().ok_or("Git unavailable")?,
            &["rev-parse", "HEAD"],
            None,
        )?;
        let base = provenant_core::GitOid::parse(std::str::from_utf8(&head.stdout.bytes)?.trim())?;
        let binding = provenant_supervisor::custody::WriterBinding::from_verified(
            launch.worktree.repo().root_dir(),
            launch.worktree.repo().private_dir(),
            launch.branch.as_str(),
            base,
        )?;
        drop(
            provenant_supervisor::fixture::acquire_writer_with_binding(
                Directory::open_anchor(&slot)?,
                scope,
                binding,
                call.lock_budget(),
                &cancel,
                &clock,
            )
            .map_err(|error| format!("acquire fixture writer: {error}"))?,
        );
        let reservation = slot
            .join(scope.attempt.to_string())
            .join("reservation.json");
        let mut value: Value = serde_json::from_slice(&fs::read(&reservation)?)?;
        let metadata = fs::metadata(&attempt_dir)?;
        value["launch_state"] = "settling".into();
        value["guardian"] = json!({"boot":value["boot"], "pid":123, "native_start":1, "group_id":123, "session_id":123});
        value["unresolved_cleanup"] = false.into();
        value["census_incomplete"] = false.into();
        value["attempt_device"] = metadata.dev().into();
        value["attempt_inode"] = metadata.ino().into();
        fs::write(reservation, serde_json::to_vec(&value)?)?;
        for (name, value) in [
            (
                "launch.json",
                json!({"schema_version":1,"scope":scope,"requester":actor_id}),
            ),
            (
                "settlement-progress.json",
                json!({"schema_version":1,"scope":scope,"phase":"import","attempts":3,"head":null,"base":null,"owner":null,"owner_since_ms":0,"retry_after_ms":0}),
            ),
        ] {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(attempt_dir.join(name))?;
            file.write_all(&serde_json::to_vec(&value)?)?;
            file.sync_all()?;
        }
        return Ok((rig, running));
    }
    let terminal = if options.writer {
        // Publish real terminal artifacts through the production commit path.
        // No launch grants are recorded, matching older writer attempts.
        store.publish_terminal(
            attempt_id,
            running.revision,
            &provenant_run::TerminalPublication {
                outcome: provenant_run::Outcome::Succeeded,
                finished_at: clock.wall_now()?,
                started_at: running.started_at,
                provider_ms: None,
                reason: None,
                result: "OK",
                complete: true,
                redacted: false,
                resolved_route: Some(applied.clone()),
                applied_route: Some(applied.clone()),
                observed_route: Some(applied),
                usage: Default::default(),
                session: None,
                partial_signals: Vec::new(),
            },
            &exit,
            &call,
        )?
    } else {
        store
            .finish(
                attempt_id,
                &attempt,
                &exit,
                &provenant_cli::run::Registry {
                    roots: options.host_registry.then_some(&roots),
                    ..Default::default()
                },
                &call,
            )
            .map_err(|error| format!("finish persisted provider fixture: {error:?}"))?
    };
    Ok((rig, terminal))
}

fn cancel_wait_settles_stalled_import_with_durable_exit() -> Result {
    let (rig, running) = persisted_attempt_with(
        "codex",
        "codex/gpt-6.1-sol",
        ("work", "fixture"),
        b"",
        b"",
        0,
        PersistedAttemptOptions {
            writer: true,
            nonterminal: true,
            stalled_import: true,
            ..Default::default()
        },
    )
    .map_err(|error| format!("stalled import fixture: {error}"))?;
    let _cleanup = rig.process_cleanup();
    let output = rig.command(&[
        "cancel",
        &running.attempt_id.to_string(),
        "--wait",
        "10s",
        "--json",
    ])?;
    let value: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(code(&output), 0, "{value}");
    assert_eq!(value["data"]["condition_met"], true, "{value}");
    let lanes = rig.command(&[
        "lanes",
        "--id",
        &running.attempt_id.to_string(),
        "--all",
        "--json",
    ])?;
    let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
    let lane = &lanes["data"]["lanes"][0];
    assert_eq!(lane["state"], "cancelled", "{lanes}");
    assert_eq!(lane["reason"], "cancelled during Git import", "{lanes}");
    Ok(())
}

fn receipt_output_resolves_run_task_and_attempt() -> Result {
    let (rig, terminal) = persisted_attempt(
        "codex",
        "codex/gpt-6.1-sol",
        ("work", "fixture"),
        b"{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}\n",
        b"",
        0,
    )?;
    let run = terminal.run_id.to_string();
    let task = terminal.task_id.to_string();
    let attempt = terminal.attempt_id.to_string();
    let targets = [
        run.clone(),
        format!("{run}/{task}"),
        format!("{run}/{task}/{attempt}"),
    ];
    let actor = provenant_run::fact(terminal.clone())?
        .reviewer
        .ok_or("reviewer fact")?
        .actor_id;
    let receipt = format!(
        "Reviewer: codex/gpt-6.1-sol@unknown (openai; resolved from explicit), run {run}/{task}/{attempt}"
    );
    let attribution =
        format!("Reviewer provenance: unknown, actor {actor}, model unknown, family unknown");
    let expected = format!("{receipt}\n{attribution}\n");
    for target in &targets {
        let human = rig.command(&["output", target, "--part", "receipt", "--full"])?;
        assert_eq!(
            code(&human),
            0,
            "{}",
            String::from_utf8_lossy(&human.stderr.bytes)
        );
        assert_eq!(String::from_utf8(human.stdout.bytes)?, expected);
        let compact = rig.command(&["output", target, "--part", "receipt"])?;
        assert_eq!(code(&compact), 0);
        assert!(compact.stdout.bytes.len() <= 2048);
        assert_eq!(String::from_utf8(compact.stdout.bytes)?, expected);
        let output = rig.command(&["output", target, "--part", "receipt", "--json"])?;
        assert_eq!(
            code(&output),
            0,
            "{}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        let data: Value = serde_json::from_slice(&output.stdout.bytes)?;
        assert_eq!(data["data"]["receipt"], receipt);
        assert_eq!(data["data"]["reviewer_provenance"], attribution);
        assert_eq!(data["data"]["reviewer"]["provenance"], "unknown");
        assert_eq!(data["data"]["reviewer"]["actor_id"], actor.to_string());
        assert_eq!(data["data"]["run_id"], run);
        assert_eq!(data["data"]["task_id"], task);
        assert_eq!(data["data"]["attempt_id"], attempt);
        assert_eq!(data["data"]["resolved_from"], "explicit");
        assert_eq!(data["data"]["role"], "reviewer");
    }
    Ok(())
}

fn receipt_output_classifies_writers_and_refuses_ambiguous_or_running_attempts() -> Result {
    for options in [
        PersistedAttemptOptions {
            writer: true,
            ..Default::default()
        },
        PersistedAttemptOptions {
            extra_task: true,
            ..Default::default()
        },
        PersistedAttemptOptions {
            nonterminal: true,
            ..Default::default()
        },
    ] {
        let writer = options.writer;
        let ambiguous = options.extra_task;
        let (rig, record) = persisted_attempt_with(
            "codex",
            "codex/gpt-6.1-sol",
            ("work", "fixture"),
            b"{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}\n",
            b"",
            0,
            options,
        )?;
        let exact = format!("{}/{}/{}", record.run_id, record.task_id, record.attempt_id);
        let target = if ambiguous {
            record.run_id.to_string()
        } else {
            exact.clone()
        };
        let output = rig.command(&["output", &target, "--part", "receipt", "--json"])?;
        let data: Value = serde_json::from_slice(&output.stdout.bytes)?;
        if writer {
            assert_eq!(code(&output), 0, "{data}");
            assert_eq!(data["data"]["role"], "writer");
            let line = data["data"]["receipt"].as_str().ok_or("receipt line")?;
            assert!(line.starts_with("Writer: codex/gpt-6.1-sol@unknown (openai;"));
            let human = rig.command(&["output", &exact, "--part", "receipt", "--full"])?;
            assert_eq!(code(&human), 0);
            assert_eq!(data["data"]["reviewer"]["provenance"], "observed");
            assert_eq!(data["data"]["reviewer"]["family"], "openai");
            let provenance = data["data"]["reviewer_provenance"]
                .as_str()
                .ok_or("provenance line")?;
            assert_eq!(
                String::from_utf8(human.stdout.bytes)?,
                format!("{line}\n{provenance}\n")
            );
        } else if ambiguous {
            assert_ne!(code(&output), 0, "{data}");
            assert_eq!(data["error"]["code"], "CONFLICT");
            assert!(
                data["error"]["message"]
                    .as_str()
                    .ok_or("message")?
                    .contains("exactly one attempt")
            );
            let exact_output = rig.command(&["output", &exact, "--part", "receipt", "--full"])?;
            assert_eq!(code(&exact_output), 0);
        } else {
            assert_ne!(code(&output), 0, "{data}");
            assert_eq!(data["error"]["code"], "NONTERMINAL");
            assert_eq!(
                data["error"]["message"],
                "receipt requires a settled attempt"
            );
        }
    }
    Ok(())
}

fn output_names_the_attempt_it_reads_and_writes_or_locates_parts() -> Result {
    let (rig, terminal) = persisted_attempt(
        "codex",
        "codex/gpt-6.1-sol",
        ("work", "fixture"),
        b"",
        b"provider 500: upstream failed\n",
        1,
    )?;
    let run = terminal.run_id.to_string();
    let task = terminal.task_id.to_string();
    let attempt = terminal.attempt_id.to_string();
    let target = format!("{run}/{task}/{attempt}");
    // A run or task names its settled attempt, and the reply says which one.
    for named in [run.clone(), format!("{run}/{task}")] {
        let output = rig.command(&["output", &named, "--part", "stderr", "--json"])?;
        let data: Value = serde_json::from_slice(&output.stdout.bytes)?;
        assert_eq!(code(&output), 0, "{data}");
        assert_eq!(data["data"]["attempt_id"], attempt);
        assert_eq!(data["data"]["selected"]["attempts"], 1);
        assert_eq!(data["data"]["selected"]["rule"], "newest settled");
    }
    // A failed attempt's result names its provider error and its result path.
    let output = rig.command(&["output", &target, "--json"])?;
    let data: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(code(&output), 0, "{data}");
    assert_eq!(data["data"]["state"], "failed");
    assert_eq!(
        data["data"]["provider_error"],
        "provider 500: upstream failed"
    );
    let schema = provenant_records::wire::data_schema("output").ok_or("output schema")?;
    provenant_records::wire::schema::validate(&schema, &data["data"])?;
    let result_path: provenant_records::StructuredPathWire =
        serde_json::from_value(data["data"]["result_path"].clone())?;
    assert!(result_path.to_path_buf()?.ends_with("result.md"));
    // A launched attempt has a custody lock, which the wait path inspects.
    fs::write(
        rig.state
            .join("runs")
            .join(&run)
            .join("attempts")
            .join(&attempt)
            .join("custody.lock"),
        b"",
    )?;
    for args in [
        vec!["lanes", "--id", &attempt],
        vec!["lanes", "--id", &attempt, "--wait", "1"],
    ] {
        let listing = rig.command(&args)?;
        assert_eq!(
            code(&listing),
            0,
            "{args:?}: {}",
            String::from_utf8_lossy(&listing.stderr.bytes)
        );
        let human = String::from_utf8(listing.stdout.bytes)?;
        assert!(
            human.contains("provider 500: upstream failed"),
            "{args:?}: {human}"
        );
        assert!(human.contains("result:"), "{human}");
    }
    let path = rig.command(&["output", &target, "--part", "stderr", "--path", "--json"])?;
    let data: Value = serde_json::from_slice(&path.stdout.bytes)?;
    assert_eq!(code(&path), 0, "{data}");
    assert!(
        serde_json::from_value::<provenant_records::StructuredPathWire>(
            data["data"]["path"].clone()
        )
        .ok()
        .and_then(|path| path.to_path_buf().ok())
        .is_some_and(|path| path.ends_with("stderr.log")),
        "{data}"
    );
    let saved = rig.root.join("saved.txt");
    let written = rig.command(&[
        "output",
        &target,
        "--part",
        "stderr",
        "--to",
        saved.to_str().ok_or("saved path is not UTF-8")?,
        "--json",
    ])?;
    let data: Value = serde_json::from_slice(&written.stdout.bytes)?;
    assert_eq!(code(&written), 0, "{data}");
    assert_eq!(std::fs::read(&saved)?, b"provider 500: upstream failed\n");
    Ok(())
}

fn receipt_output_reports_unknown_route_for_settled_lost_attempt() -> Result {
    let (rig, record) = persisted_attempt_with(
        "codex",
        "codex/gpt-6.1-sol",
        ("work", "fixture"),
        b"",
        b"",
        0,
        PersistedAttemptOptions {
            lost_before_route: true,
            ..Default::default()
        },
    )?;
    let target = format!("{}/{}/{}", record.run_id, record.task_id, record.attempt_id);
    let output = rig.command(&["output", &target, "--part", "receipt", "--json"])?;
    let data: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(code(&output), 0, "{data}");
    for field in ["adapter", "model", "effort", "family", "resolved_from"] {
        assert_eq!(data["data"][field], "unknown", "{field}: {data}");
    }
    assert_eq!(data["data"]["reviewer"]["provenance"], "unknown");
    let actor = provenant_run::fact(record)?
        .reviewer
        .ok_or("reviewer fact")?
        .actor_id;
    assert_eq!(data["data"]["reviewer"]["actor_id"], actor.to_string());
    let human = rig.command(&["output", &target, "--part", "receipt", "--full"])?;
    assert_eq!(code(&human), 0);
    assert_eq!(
        String::from_utf8(human.stdout.bytes)?,
        format!(
            "Reviewer: unknown@unknown (unknown; resolved from unknown), run {target}\nReviewer provenance: unknown, actor {actor}, model unknown, family unknown\n"
        )
    );
    Ok(())
}

fn agy_denial_result_reaches_terminal_reason() -> Result {
    let (rig, terminal) = persisted_attempt(
        "agy",
        "agy/gemini-3.8-flash",
        ("work", "google-fixture"),
        include_bytes!("fixtures/providers/agy/denied-result.jsonl"),
        b"",
        0,
    )?;
    assert!(matches!(
        terminal.state,
        provenant_run::AttemptState::Terminal {
            outcome: provenant_run::Outcome::Failed,
            ..
        }
    ));
    let lanes = rig.command(&[
        "lanes",
        "--id",
        &terminal.attempt_id.to_string(),
        "--all",
        "--json",
    ])?;
    assert_eq!(code(&lanes), 0, "{:?}", lanes.stderr.bytes);
    let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
    assert_eq!(
        lanes["data"]["lanes"][0]["reason"],
        "provider denied: RunCommand"
    );
    Ok(())
}

fn agy_denied_command_reaches_result_and_terminal_reason() -> Result {
    let denial = include_bytes!("fixtures/providers/agy/command-denied.stderr");
    // Synthetic partial stdout and the real stderr-only failure case.
    let partial = br#"{"event":"step_update","step_update":{"conversation_id":"fixture-session","step_type":"agent_response","text_delta":"I verified part of the claim."}}"#;
    let failed_response = br#"{"event":"result","result":{"conversation_id":"","status":"ERROR","response":"I verified part of the claim.","error":"command permission denied","usage":{"input_tokens":0,"output_tokens":0}}}"#;
    for stdout in [
        b"".as_slice(),
        partial.as_slice(),
        failed_response.as_slice(),
    ] {
        let (rig, terminal) = persisted_attempt(
            "agy",
            "agy/gemini-3.8-flash",
            ("work", "google-fixture"),
            stdout,
            denial,
            1,
        )?;
        assert!(matches!(
            terminal.state,
            provenant_run::AttemptState::Terminal {
                outcome: provenant_run::Outcome::Failed,
                ..
            }
        ));
        let lanes = rig.command(&[
            "lanes",
            "--id",
            &terminal.attempt_id.to_string(),
            "--all",
            "--json",
        ])?;
        assert_eq!(code(&lanes), 0, "{:?}", lanes.stderr.bytes);
        let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
        let reason = lanes["data"]["lanes"][0]["reason"]
            .as_str()
            .ok_or("terminal reason")?;
        assert!(reason.contains(r#""command" permission"#), "{lanes}");
        let output = rig.command(&["output", &terminal.attempt_id.to_string(), "--json"])?;
        assert_eq!(code(&output), 0, "{:?}", output.stderr.bytes);
        let output: Value = serde_json::from_slice(&output.stdout.bytes)?;
        let result = output["data"]["bytes"]["data"]
            .as_str()
            .ok_or("result bytes")?;
        assert!(result.contains(r#""command" permission"#), "{output}");
        if !stdout.is_empty() {
            assert!(
                result.starts_with("I verified part of the claim."),
                "{output}"
            );
        }
    }
    // A valid successful answer wins even when stderr contains a denial notice.
    let success = br###"{"event":"init","conversation_id":"fixture-session","init":{"model":"gemini-3.8-flash"}}
{"event":"result","result":{"conversation_id":"fixture-session","status":"SUCCESS","response":"## Findings\nVerified.\n## Evidence\nChecked the claim.\n```handoff\n{\"schema_version\":2,\"status\":\"done\",\"remaining\":[],\"decisions\":[],\"verify\":[],\"note\":\"verified\"}\n```","error":null,"usage":{"input_tokens":0,"output_tokens":0}}}"###;
    let (rig, terminal) = persisted_attempt(
        "agy",
        "agy/gemini-3.8-flash",
        ("work", "google-fixture"),
        success,
        denial,
        0,
    )?;
    assert!(matches!(
        terminal.state,
        provenant_run::AttemptState::Terminal {
            outcome: provenant_run::Outcome::Succeeded,
            ..
        }
    ));
    let output = rig.command(&["output", &terminal.attempt_id.to_string(), "--json"])?;
    assert_eq!(code(&output), 0, "{:?}", output.stderr.bytes);
    let output: Value = serde_json::from_slice(&output.stdout.bytes)?;
    let result = output["data"]["bytes"]["data"]
        .as_str()
        .ok_or("result bytes")?;
    assert!(
        result.starts_with("## Findings\nVerified.\n## Evidence\nChecked the claim."),
        "{output}"
    );
    Ok(())
}

fn opencode_free_pool_cools_second_model() -> Result {
    use provenant_records::{
        ResolvedRoute, RouteConfidentiality, RouteEligibility, RouteReadiness,
    };
    let stdout = br#"{"type":"error","error":{"type":"provider.quota","message":"free tier exhausted","response":{"body":{"error":{"type":"FreeUsageLimitError","message":"fixture free allowance exhausted"}}}}}"#;
    let quota_record = persisted_refusal(
        "opencode",
        "opencode/opencode/deepseek-v4.1-flash-free",
        ("open-code-free", "keyless-free"),
        stdout,
    )?;
    assert_eq!(quota_record.refusals.len(), 1);
    assert_eq!(
        quota_record.refusals[0].pool.as_deref(),
        Some("opencode-free")
    );
    let now = quota_record.refusals[0].observed_unix_seconds;

    let second = ResolvedRoute {
        extra: Default::default(),
        adapter: "opencode".into(),
        model: "opencode/glm-5.3-flash-free".into(),
        effort: None,
        catalogue_revision: "fixture-catalogue".into(),
        account_key: Some("keyless-free".into()),
        host: "fixture-host".into(),
        alternatives: Default::default(),
        exclusions: vec![],
        eligibility: RouteEligibility::Eligible,
        readiness: RouteReadiness::Ready {
            extra: Default::default(),
            availability_ref: "fixture-preflight".into(),
            expires_unix_seconds: now + 300,
        },
        confidentiality: RouteConfidentiality {
            extra: Default::default(),
            evidence_ref: "fixture-access".into(),
        },
        pool_selection: None,
    };
    let pools = vec!["opencode-free".to_owned()];
    assert!(matches!(
        provenant_quota::Admission {
            policies: &provenant_providers::quota_policies(provenant_providers::REGISTRY),
            now_unix_seconds: now,
            records: std::slice::from_ref(&quota_record),
            pools: &pools,
        }
        .check(&second),
        Err(provenant_quota::QuotaError::Refused(
            "free tier exhausted; use a paid route or wait for its allowance reset"
        ))
    ));
    Ok(())
}

fn opencode_free_reset_cools_pool_until_reset_and_restores() -> Result {
    use provenant_records::{
        ResolvedRoute, RouteConfidentiality, RouteEligibility, RouteReadiness,
    };
    let fixture_stdout = include_bytes!("../tests/fixtures/providers/opencode/free-limit.jsonl");
    let quota_record = persisted_refusal(
        "opencode",
        "opencode/opencode/deepseek-v4.1-flash-free",
        ("open-code-free", "keyless-free"),
        fixture_stdout,
    )?;
    assert_eq!(quota_record.refusals.len(), 1);
    let refusal = &quota_record.refusals[0];
    assert_eq!(refusal.pool.as_deref(), Some("opencode-free"));
    let now = refusal.observed_unix_seconds;
    let resets = now + 240;
    assert_eq!(refusal.resets_unix_seconds, Some(resets));

    let first = ResolvedRoute {
        extra: Default::default(),
        adapter: "opencode".into(),
        model: "opencode/deepseek-v4.1-flash-free".into(),
        effort: None,
        catalogue_revision: "fixture-catalogue".into(),
        account_key: Some("keyless-free".into()),
        host: "fixture-host".into(),
        alternatives: Default::default(),
        exclusions: vec![],
        eligibility: RouteEligibility::Eligible,
        readiness: RouteReadiness::Ready {
            extra: Default::default(),
            availability_ref: "fixture-preflight".into(),
            expires_unix_seconds: resets + 300,
        },
        confidentiality: RouteConfidentiality {
            extra: Default::default(),
            evidence_ref: "fixture-access".into(),
        },
        pool_selection: None,
    };
    let mut second = first.clone();
    second.model = "opencode/glm-5.3-flash-free".into();

    let pools = vec!["opencode-free".to_owned()];
    let admit = |route: &ResolvedRoute, at| {
        provenant_quota::Admission {
            policies: &provenant_providers::quota_policies(provenant_providers::REGISTRY),
            now_unix_seconds: at,
            records: std::slice::from_ref(&quota_record),
            pools: &pools,
        }
        .check(route)
    };

    // Both free models are refused until the stated reset time.
    for at in [now, now + 120, resets - 1] {
        for route in [&first, &second] {
            assert!(matches!(
                admit(route, at),
                Err(provenant_quota::QuotaError::Refused(
                    "free tier exhausted; use a paid route or wait for its allowance reset"
                ))
            ));
        }
    }

    // At and after the reset passes, both free models are admitted again.
    admit(&first, resets)?;
    admit(&second, resets)?;
    admit(&first, resets + 10)?;
    admit(&second, resets + 10)?;

    // Missing, null, negative and huge values fall back or clamp.
    for (retry_ms, expected_offset) in [
        (None, None),
        (Some(serde_json::Value::Null), None),
        (Some(serde_json::json!(-5000)), None),
        (Some(serde_json::json!(999_999_999)), Some(86_400)),
    ] {
        let mut err = serde_json::json!({
            "type": "provider.quota",
            "message": "free tier exhausted",
            "response": {
                "body": {
                    "error": {
                        "type": "FreeUsageLimitError",
                        "message": "fixture free allowance exhausted"
                    }
                }
            }
        });
        if let Some(val) = retry_ms {
            err["retryAfterMs"] = val;
        }
        let payload = serde_json::to_vec(&serde_json::json!({
            "type": "error",
            "error": err
        }))?;
        let record = persisted_refusal(
            "opencode",
            "opencode/opencode/deepseek-v4.1-flash-free",
            ("open-code-free", "keyless-free"),
            &payload,
        )?;
        let record_now = record.refusals[0].observed_unix_seconds;
        assert_eq!(
            record.refusals[0].resets_unix_seconds,
            expected_offset.map(|offset| record_now + offset),
            "failed for retry_ms: {err:?}"
        );
    }

    Ok(())
}

fn agy_individual_quota_cools_model_until_reset() -> Result {
    use provenant_records::{
        ResolvedRoute, RouteConfidentiality, RouteEligibility, RouteReadiness,
    };
    let stdout = br#"{"event":"result","result":{"conversation_id":"","status":"ERROR","response":"","error":"Individual quota reached. Resets in 7h23m.","usage":{"input_tokens":0,"output_tokens":0}}}"#;
    let quota_record = persisted_refusal(
        "agy",
        "agy/gemini-3.8-flash",
        ("work", "google-fixture"),
        stdout,
    )?;
    assert_eq!(quota_record.refusals.len(), 1);
    let refusal = &quota_record.refusals[0];
    assert_eq!(
        (refusal.model.as_str(), refusal.pool.as_deref()),
        ("gemini-3.8-flash", None)
    );
    let now = refusal.observed_unix_seconds;
    let resets = now + 7 * 3600 + 23 * 60;
    assert_eq!(refusal.resets_unix_seconds, Some(resets));

    let mut route = ResolvedRoute {
        extra: Default::default(),
        adapter: "agy".into(),
        model: "gemini-3.8-flash".into(),
        effort: None,
        catalogue_revision: "fixture-catalogue".into(),
        account_key: Some("google-fixture".into()),
        host: "fixture-host".into(),
        alternatives: Default::default(),
        exclusions: vec![],
        eligibility: RouteEligibility::Eligible,
        readiness: RouteReadiness::Ready {
            extra: Default::default(),
            availability_ref: "fixture-preflight".into(),
            expires_unix_seconds: resets + 300,
        },
        confidentiality: RouteConfidentiality {
            extra: Default::default(),
            evidence_ref: "fixture-access".into(),
        },
        pool_selection: None,
    };
    let admit = |route: &ResolvedRoute, now_unix_seconds| {
        provenant_quota::Admission {
            policies: &provenant_providers::quota_policies(provenant_providers::REGISTRY),
            now_unix_seconds,
            records: std::slice::from_ref(&quota_record),
            pools: &[],
        }
        .check(route)
    };
    // Past the unknown-reset hour, the stated reset still holds the model.
    for at in [now, now + 3600, resets - 1] {
        assert!(matches!(
            admit(&route, at),
            Err(provenant_quota::QuotaError::Refused(
                "scoped provider refusal"
            ))
        ));
    }
    admit(&route, resets)?;
    route.model = "gemini-3.8-pro".into();
    admit(&route, now)?;
    Ok(())
}

fn owned_dispatch_retains_failed_attempt_and_replays() -> Result {
    let rig = TestRig::new()?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    for args in [
        vec!["commit", "--allow-empty", "-qm", "base"],
        vec!["branch", "-M", "main"],
    ] {
        let output = rig.execute(&git, &args, None)?;
        assert_eq!(code(&output), 0);
    }
    let prompt = rig.root.join("owned-prompt.md");
    fs::write(&prompt, "Do not change files.")?;
    let args = [
        "dispatch",
        "--operation-id",
        "owned-replay",
        "--model",
        "codex/gpt-6-luna",
        "--mode",
        "worktree-write",
        "--worktree-branch",
        "fixture/owned",
        "--base",
        "main",
        "--deliver",
        "push",
        "--prompt-file",
        prompt.to_str().ok_or("prompt")?,
        "--json",
    ];
    // A failed attempt retains its checkout even when it is clean.
    let first = rig.command(&args)?;
    let first: Value = serde_json::from_slice(&first.stdout.bytes)?;
    let data = &first["data"];
    assert_eq!(data["worktree"]["owned"], true, "{first}");
    assert_eq!(
        data["worktree_settlements"][0]["state"], "retained",
        "{first}"
    );
    let path = data["worktree"]["path"].as_str().ok_or("worktree path")?;
    assert!(std::path::Path::new(path).exists());
    // Later repository protection governs new admissions and delivery, not
    // replay of this already accepted operation.
    for args in [
        vec![
            "remote",
            "add",
            "origin",
            rig.project.to_str().ok_or("project")?,
        ],
        vec![
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/fixture/owned",
        ],
    ] {
        assert_eq!(code(&rig.execute(&git, &args, None)?), 0);
    }
    let second = rig.command(&args)?;
    let second: Value = serde_json::from_slice(&second.stdout.bytes)?;
    assert_eq!(second["data"]["run_id"], data["run_id"], "{second}");
    assert!(std::path::Path::new(path).exists());
    // Explicit worktrees use the ordinary admission path, which must also
    // preserve replay after a publication target becomes protected.
    assert_eq!(
        code(&rig.execute(
            &git,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/unrelated"
            ],
            None
        )?),
        0
    );
    let explicit_args = [
        "dispatch",
        "--operation-id",
        "explicit-replay",
        "--model",
        "codex/gpt-6-luna",
        "--mode",
        "worktree-write",
        "--worktree",
        path,
        "--base",
        "main",
        "--deliver",
        "push",
        "--prompt-file",
        prompt.to_str().ok_or("prompt")?,
        "--json",
    ];
    let explicit = rig.command(&explicit_args)?;
    let explicit: Value = serde_json::from_slice(&explicit.stdout.bytes)?;
    assert!(explicit["data"]["run_id"].is_string(), "{explicit}");
    assert_eq!(
        code(&rig.execute(
            &git,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/fixture/owned",
            ],
            None
        )?),
        0
    );
    let explicit_replay = rig.command(&explicit_args)?;
    let explicit_replay: Value = serde_json::from_slice(&explicit_replay.stdout.bytes)?;
    assert_eq!(
        explicit_replay["data"]["run_id"], explicit["data"]["run_id"],
        "{explicit_replay}"
    );
    fs::write(&prompt, "Changed prompt")?;
    let conflict = rig.command(&args)?;
    assert_ne!(code(&conflict), 0);
    let explicit_conflict = rig.command(&explicit_args)?;
    assert_ne!(code(&explicit_conflict), 0);
    assert!(std::path::Path::new(path).exists());
    Ok(())
}

fn owned_dispatch_refuses_adopted_owner_and_cleans_refused_creation() -> Result {
    let rig = TestRig::new()?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    for args in [
        vec!["commit", "--allow-empty", "-qm", "base"],
        vec!["branch", "-M", "main"],
    ] {
        assert_eq!(code(&rig.execute(&git, &args, None)?), 0);
    }
    let branch = "fixture/adopted";
    assert_eq!(
        code(&rig.command(&["worktree", "create", branch, "--base", "main", "--json",])?),
        0
    );
    let tree = rig.project.join(".worktrees/fixture-adopted");
    let admin = rig.execute(
        &git,
        &[
            "-C",
            tree.to_str().ok_or("tree")?,
            "rev-parse",
            "--absolute-git-dir",
        ],
        None,
    )?;
    assert_eq!(code(&admin), 0);
    let owner_path =
        PathBuf::from(String::from_utf8(admin.stdout.bytes)?.trim()).join("provenant-owner.json");
    let base_head = rig.execute(&git, &["rev-parse", "HEAD"], None)?;
    assert_eq!(code(&base_head), 0);
    let mut owner = json!({
        "attempt_id": null,
        "operation_id": null,
        "branch": branch,
        "base": String::from_utf8(base_head.stdout.bytes)?.trim(),
        "host": provenant_core::HostId::from_bytes([1; 16]),
        "created_at": provenant_core::WallTime::from_micros(1)?,
        "origin": "adopted",
        "label": "human-session"
    });
    let base = owner["base"].clone();
    let prompt = rig.root.join("owned-prompt.md");
    fs::write(&prompt, "Do not change files.")?;
    for same_operation in [false, true] {
        owner["operation_id"] = if same_operation {
            json!("adopted-refused")
        } else {
            Value::Null
        };
        fs::write(&owner_path, serde_json::to_vec(&owner)?)?;
        let before = fs::read(&owner_path)?;
        let out = rig.command(&[
            "dispatch",
            "--operation-id",
            "adopted-refused",
            "--model",
            "codex/gpt-6-luna",
            "--mode",
            "worktree-write",
            "--worktree-branch",
            branch,
            "--base",
            "main",
            "--prompt-file",
            prompt.to_str().ok_or("prompt")?,
            "--json",
        ])?;
        assert_ne!(code(&out), 0);
        let result: Value = serde_json::from_slice(&out.stdout.bytes)?;
        assert!(
            result["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("adopted or foreign owner"),
            "{result}"
        );
        assert_eq!(fs::read(&owner_path)?, before);
        assert_eq!(owner["base"], base);
        assert_eq!(owner["label"], "human-session");
    }
    // A new checkout made before prompt loading fails must be removed.
    let missing = rig.root.join("missing-prompt.md");
    let out = rig.command(&[
        "dispatch",
        "--operation-id",
        "creation-refused",
        "--model",
        "codex/gpt-6-luna",
        "--mode",
        "worktree-write",
        "--worktree-branch",
        "fixture/refused",
        "--base",
        "main",
        "--prompt-file",
        missing.to_str().ok_or("missing")?,
        "--json",
    ])?;
    assert_ne!(code(&out), 0);
    let refused_tree = rig.project.join(".worktrees/fixture-refused");
    if refused_tree.exists() {
        // A native census that cannot prove the tree free must retain its
        // durable lane owner for GC rather than force removal.
        let result: Value = serde_json::from_slice(&out.stdout.bytes)?;
        assert!(
            result["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("new managed worktree retained for gc"),
            "{result}"
        );
        let admin = rig.execute(
            &git,
            &[
                "-C",
                refused_tree.to_str().ok_or("tree")?,
                "rev-parse",
                "--absolute-git-dir",
            ],
            None,
        )?;
        let owner: Value = serde_json::from_slice(&fs::read(
            PathBuf::from(String::from_utf8(admin.stdout.bytes)?.trim())
                .join("provenant-owner.json"),
        )?)?;
        assert_eq!(owner["origin"], "lane");
        assert_eq!(owner["operation_id"], "creation-refused");
    }
    Ok(())
}

fn fixture_owner_repro(ready_path: &std::path::Path) -> Result {
    let mut rig = TestRig::new()?;
    let data = rig.home.join(".local/share/opencode");
    fs::create_dir_all(&data)?;
    let db = rusqlite::Connection::open(data.join("opencode.db"))?;
    db.execute_batch(
        r#"PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
        CREATE TABLE session_message (session_id TEXT, type TEXT, data TEXT);
        INSERT INTO session_message VALUES ('ses_x', 'assistant',
        '{"model":{"providerID":"opencode-go","id":"glm-5.3-flash"},"finish":"stop"}');"#,
    )?;
    let bin = rig.root.join("provider/bin");
    fs::create_dir_all(&bin)?;
    rig.copy_executable(&std::env::current_exe()?, &bin.join("opencode"))?;
    rig.env.retain(|(key, _)| key != "PATH");
    rig.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    let prompt = rig.root.join("prompt.md");
    fs::write(&prompt, "fixture owner lifetime probe")?;
    let output = rig.command(&[
        "dispatch",
        "--model",
        "opencode/opencode-go/glm-5.3-flash",
        "--operation-id",
        "fixture-owner-lifetime",
        "--prompt-file",
        prompt.to_str().ok_or("prompt path")?,
        "--wait",
        "1s",
        "--json",
    ])?;
    // The provider is still running when the one-second wait ends (exit 124).
    assert!(matches!(code(&output), 0 | 124), "{output:?}");
    let identity = provenant_test_support::wait_for(
        &format!(
            "guardian identity; dispatch output: {}",
            String::from_utf8_lossy(&output.stdout.bytes)
        ),
        std::time::Duration::from_secs(10),
        || find_guardian_identity(&rig.state),
        |result| !matches!(result, Ok(None)),
    )??
    .ok_or("guardian identity missing")?;
    fs::write(
        ready_path,
        serde_json::to_vec(&json!({"root": rig.root, "guardian": identity}))?,
    )?;
    // Keep the owner alive until the parent kills it to test guardian cleanup.
    #[allow(clippy::disallowed_methods)] // Bound fixture lifetime if cleanup fails.
    std::thread::park_timeout(std::time::Duration::from_secs(120));
    Ok(())
}

fn find_guardian_identity(root: &std::path::Path) -> Result<Option<Value>> {
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            if let Some(identity) = find_guardian_identity(&path)? {
                return Ok(Some(identity));
            }
        } else if path
            .file_name()
            .is_some_and(|name| name == "reservation.json")
        {
            let value: Value = serde_json::from_slice(&fs::read(path)?)?;
            if let Some(identity) = value.get("guardian") {
                return Ok(Some(identity.clone()));
            }
        }
    }
    Ok(None)
}

fn fixture_guardian_exits_when_sigkilled_test_owner_dies() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    use provenant_supervisor::census::{ProcessObservation, SystemProcessObserver};
    use std::os::unix::process::ExitStatusExt;
    let ready = std::env::temp_dir().join(format!("pv-owner-{}.json", std::process::id()));
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .env("PROVENANT_FIXTURE_OWNER_REPRO", &ready)
        .spawn()?;
    let observation = provenant_test_support::wait_for(
        "fixture owner readiness",
        std::time::Duration::from_secs(20),
        || child.try_wait().map(|status| (ready.exists(), status)),
        |result| !matches!(result, Ok((false, None))),
    );
    let (is_ready, status) = match observation {
        Ok(result) => result?,
        Err(error) => {
            child.kill()?;
            let _ = child.wait()?;
            return Err(error.into());
        }
    };
    if let Some(status) = status {
        return Err(format!("fixture owner repro exited early: {status}").into());
    }
    assert!(is_ready);
    let record: Value = serde_json::from_slice(&fs::read(&ready)?)?;
    let identity: provenant_supervisor::census::ProcessIdentity =
        serde_json::from_value(record["guardian"].clone())?;
    child.kill()?;
    let status = child.wait()?;
    assert_eq!(status.signal(), Some(9));
    provenant_test_support::wait_for(
        &format!("guardian {} exit after owner death", identity.pid),
        std::time::Duration::from_secs(30),
        || SystemProcessObserver.observe(identity.pid),
        |observation| !matches!(observation, ProcessObservation::Present(fact) if fact.identity == identity),
    )?;
    fs::remove_file(&ready)?;
    fs::remove_dir_all(record["root"].as_str().ok_or("fixture root")?)?;
    Ok(())
}

fn main() -> ExitCode {
    if let Some(path) = std::env::var_os("PROVENANT_FIXTURE_OWNER_REPRO") {
        return match fixture_owner_repro(std::path::Path::new(&path)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let _ = writeln!(io::stderr(), "fixture owner repro: {error}");
                ExitCode::FAILURE
            }
        };
    }
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    #[cfg(target_os = "macos")]
    if std::env::args_os().next().is_some_and(|arg| {
        std::path::Path::new(&arg)
            .file_name()
            .is_some_and(|name| name == "agy")
    }) {
        return if agy_lease_fixture().is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let result = if args
        .first()
        .is_some_and(|s| s == "exec" || s == "run" || s == "--version" || s == "auth")
        // Claude options such as graph input `--add-dir` may precede `-p`.
        || args.iter().any(|s| s == "-p")
    {
        fixture()
    } else {
        provenant_supervisor::fixture::run_libtest(
            &args,
            &[
                #[cfg(target_os = "macos")]
                "dispatch_proceeds_when_provider_update_is_busy",
                "run_refuses_pacing_reserve",
                "cancel_wait_settles_stalled_import_with_durable_exit",
                "receipt_output_reports_unknown_route_for_settled_lost_attempt",
                "output_names_the_attempt_it_reads_and_writes_or_locates_parts",
                "receipt_output_resolves_run_task_and_attempt",
                "receipt_output_classifies_writers_and_refuses_ambiguous_or_running_attempts",
                "owned_dispatch_retains_failed_attempt_and_replays",
                "owned_dispatch_refuses_adopted_owner_and_cleans_refused_creation",
                "printed_credentials_leave_no_plaintext_in_run_files",
                "codex_dispatch_observes_rollout",
                "policy_empty_result_falls_back_without_a_chair_turn",
                "policy_incomplete_result_continues_the_retained_session",
                "stats_reports_partial_signals_outside_graph_groups",
                "submit_prints_each_brief_read_root_warning_once",
                "interrupted_progress_recovers_on_another_account_in_the_same_worktree",
                "handoff_recovery_requires_a_settled_successor",
                "capacity_settlement_resumes_the_same_session",
                "fixture_guardian_exits_when_sigkilled_test_owner_dies",
                "review_resolves_identity_only_dispositions",
                "review_fans_out_and_collects_fake_provider",
                "opencode_usage_limit_settlement_is_bounded",
                "opencode_usage_limit_stops_hanging_provider",
                "opencode_stderr_live_quota",
                "opencode_dispatch_observes_v2_wal_session",
                "opus_writer_dispatch_advises_a_cheaper_route",
                "claude_dispatch_refreshes_registered_account",
                "pool_receipt_excludes_prompt_and_credential_from_artifacts",
                "host_native_dispatch_uses_the_declared_agent_and_refuses_resume",
                "host_native_preparation_uses_the_provider_contract",
                "unconfigured_host_native_repair_launches_alias_fallback",
                "graph_read_only_alias_does_not_redraw_after_launch_quota",
                "direct_writer_launches_second_eligible_pool_member",
                "graph_writer_launches_second_eligible_pool_member",
                "graph_retries_real_registry_lock_and_runs_writer",
                "graph_settlement_advances_writer_and_account_routed_review",
                "graph_without_deadline_launches_on_tick",
                "graph_node_reads_full_upstream_result_from_its_inputs_directory",
                "review_overcap_runs_through_fake_providers",
                "graph_settlement_records_a_failed_advance_for_status",
                "opencode_free_pool_cools_second_model",
                "opencode_free_reset_cools_pool_until_reset_and_restores",
                "agy_individual_quota_cools_model_until_reset",
                "agy_denied_command_reaches_result_and_terminal_reason",
                "agy_denial_result_reaches_terminal_reason",
                "codex_capacity_settles_without_quota_refusal",
                "capacity_holder_persists_and_explicit_and_resume_ignore_cooldown",
            ],
            |name| {
                let result = match name {
                    #[cfg(target_os = "macos")]
                    "dispatch_proceeds_when_provider_update_is_busy" => dispatch_proceeds_when_provider_update_is_busy(),
                    "run_refuses_pacing_reserve" => run_refuses_pacing_reserve(),
                    "cancel_wait_settles_stalled_import_with_durable_exit" => cancel_wait_settles_stalled_import_with_durable_exit(),
                    "receipt_output_reports_unknown_route_for_settled_lost_attempt" => {
                        receipt_output_reports_unknown_route_for_settled_lost_attempt()
                    }
                    "output_names_the_attempt_it_reads_and_writes_or_locates_parts" => {
                        output_names_the_attempt_it_reads_and_writes_or_locates_parts()
                    }
                    "receipt_output_resolves_run_task_and_attempt" => {
                        receipt_output_resolves_run_task_and_attempt()
                    }
                    "receipt_output_classifies_writers_and_refuses_ambiguous_or_running_attempts" => {
                        receipt_output_classifies_writers_and_refuses_ambiguous_or_running_attempts()
                    }
                    "owned_dispatch_retains_failed_attempt_and_replays" => {
                        owned_dispatch_retains_failed_attempt_and_replays()
                    }
                    "owned_dispatch_refuses_adopted_owner_and_cleans_refused_creation" => {
                        owned_dispatch_refuses_adopted_owner_and_cleans_refused_creation()
                    }
                    "printed_credentials_leave_no_plaintext_in_run_files" => printed_credentials_leave_no_plaintext_in_run_files(),
                    "codex_dispatch_observes_rollout" => codex_dispatch_observes_rollout(),
                    "policy_empty_result_falls_back_without_a_chair_turn" => policy_empty_result_falls_back_without_a_chair_turn(),
                    "policy_incomplete_result_continues_the_retained_session" => policy_incomplete_result_continues_the_retained_session(),
                    "stats_reports_partial_signals_outside_graph_groups" => stats_reports_partial_signals_outside_graph_groups(),
                    "submit_prints_each_brief_read_root_warning_once" => submit_prints_each_brief_read_root_warning_once(),
                    "interrupted_progress_recovers_on_another_account_in_the_same_worktree" => interrupted_progress_recovers_on_another_account_in_the_same_worktree(),
                    "handoff_recovery_requires_a_settled_successor" => handoff_recovery_requires_a_settled_successor(),
                    "capacity_settlement_resumes_the_same_session" => capacity_settlement_resumes_the_same_session(),
                    "fixture_guardian_exits_when_sigkilled_test_owner_dies" => fixture_guardian_exits_when_sigkilled_test_owner_dies(),
                    "review_resolves_identity_only_dispositions" => review_resolves_identity_only_dispositions(),
                    "review_fans_out_and_collects_fake_provider" => review_fans_out_and_collects_fake_provider(),
                    "review_overcap_runs_through_fake_providers" => review_overcap_runs_through_fake_providers(),
                    "opencode_usage_limit_settlement_is_bounded" => {
                        opencode_usage_limit_settlement_is_bounded()
                    }
                    "opencode_usage_limit_stops_hanging_provider" => {
                        opencode_usage_limit_stops_hanging_provider()
                    }
                    "opencode_stderr_live_quota" => opencode_stderr_live_quota(),
                    "opencode_dispatch_observes_v2_wal_session" => {
                        opencode_dispatch_observes_v2_wal_session()
                    }
                    "opus_writer_dispatch_advises_a_cheaper_route" => {
                        opus_writer_dispatch_advises_a_cheaper_route()
                    }
                    "pool_receipt_excludes_prompt_and_credential_from_artifacts" => pool_receipt_excludes_prompt_and_credential_from_artifacts(),
                    "claude_dispatch_refreshes_registered_account" => {
                        claude_dispatch_refreshes_registered_account()
                    }
                    "host_native_dispatch_uses_the_declared_agent_and_refuses_resume" => {
                        host_native_dispatch_uses_the_declared_agent_and_refuses_resume()
                    }
                    "host_native_preparation_uses_the_provider_contract" => {
                        host_native_preparation_uses_the_provider_contract()
                    }
                    "unconfigured_host_native_repair_launches_alias_fallback" => {
                        unconfigured_host_native_repair_launches_alias_fallback()
                    }
                    "graph_read_only_alias_does_not_redraw_after_launch_quota" => graph_read_only_alias_does_not_redraw_after_launch_quota(),
                    "direct_writer_launches_second_eligible_pool_member" => direct_writer_launches_second_eligible_pool_member(),
                    "graph_writer_launches_second_eligible_pool_member" => graph_writer_launches_second_eligible_pool_member(),
                    "graph_retries_real_registry_lock_and_runs_writer" => graph_retries_real_registry_lock_and_runs_writer(),
                    "graph_without_deadline_launches_on_tick" => graph_without_deadline_launches_on_tick(),
                    "graph_settlement_advances_writer_and_account_routed_review" => {
                        graph_settlement_advances_writer_and_account_routed_review()
                    }
                    "graph_node_reads_full_upstream_result_from_its_inputs_directory" => {
                        graph_node_reads_full_upstream_result_from_its_inputs_directory()
                    }
                    "graph_settlement_records_a_failed_advance_for_status" => {
                        graph_settlement_records_a_failed_advance_for_status()
                    }
                    "opencode_free_pool_cools_second_model" => {
                        opencode_free_pool_cools_second_model()
                    }
                    "opencode_free_reset_cools_pool_until_reset_and_restores" => {
                        opencode_free_reset_cools_pool_until_reset_and_restores()
                    }
                    "agy_individual_quota_cools_model_until_reset" => {
                        agy_individual_quota_cools_model_until_reset()
                    }
                    "agy_denied_command_reaches_result_and_terminal_reason" => {
                        agy_denied_command_reaches_result_and_terminal_reason()
                    }
                    "agy_denial_result_reaches_terminal_reason" => {
                        agy_denial_result_reaches_terminal_reason()
                    }
                    "codex_capacity_settles_without_quota_refusal" => {
                        codex_capacity_settles_without_quota_refusal()
                    }
                    "capacity_holder_persists_and_explicit_and_resume_ignore_cooldown" => {
                        capacity_holder_persists_and_explicit_and_resume_ignore_cooldown()
                    }
                    _ => opencode_free_pool_cools_second_model(),
                };
                result.map_err(|e| io::Error::other(e.to_string()))
            },
        )
        .map_err(Into::into)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr(), "dispatch fixture: {error}");
            ExitCode::FAILURE
        }
    }
}
