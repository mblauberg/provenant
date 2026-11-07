//! Graph provider composition with isolated, synthetic provider processes.
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

// Medium: a real shell inherits the provider's lane environment and native
// confinement. No network, provider login, compiler or sleeps are involved.
fn writer_shell_probe() -> Result<String> {
    let script = r#"set -eu
for name in AGENTS.md CLAUDE.md HARNESS.md; do
    test -f "$name"
    printf 'edited by lane shell\n' > "$name"
done
mkdir -p shell-scratch/build
printf disposable > shell-scratch/build/file
rm -rf shell-scratch/build
test ! -e shell-scratch/build
rmdir shell-scratch
git add AGENTS.md CLAUDE.md HARNESS.md
git commit -qm 'test: edit root instructions'
printf 'shell completed\n'
printf 'ghp_abcdefghijklmnopqrstuvwxyz0123456789\n'
printf 'shell stderr ghp_abcdefghijklmnopqrstuvwxyz0123456789\n' >&2
"#;
    // Claude uses the generated shell prefix; Codex runs an ordinary shell.
    let output = if let Some(prefix) = std::env::var_os("CLAUDE_CODE_SHELL_PREFIX") {
        std::process::Command::new(prefix).arg(script).output()?
    } else {
        std::process::Command::new("/bin/sh")
            .args(["-c", script])
            .output()?
    };
    assert!(
        output.status.success(),
        "shell failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    io::stderr().write_all(&output.stderr)?;
    Ok(String::from_utf8(output.stdout)?)
}

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
        } else if prompt.contains("fixture writer shell") {
            writer_shell_probe()?
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
    if !prompt.contains("fixture handoff recovery")
        && !prompt.contains("fixture graph redraw")
        && !prompt.contains("fixture partial quota redraw")
    {
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
                json!({"type":"turn_context","payload":{"model": if prompt.contains("fixture uncatalogued") || prompt.contains("fixture policy result") || prompt.contains("fixture policy continuation") || prompt.contains("fixture graph redraw") || prompt.contains("fixture partial quota redraw") { policy_model.unwrap_or("gpt-6-luna") } else if prompt.contains("fixture pool result") { "gpt-6.1-sol" } else { "gpt-6-luna" },"effort":"low"}}),
                json!({"type":"response_item","payload":{"content":recorded}})
            ),
        )?;
    }
    if prompt.contains("fixture handoff recovery") {
        return handoff_recovery_provider(&prompt, &args, &auth);
    }
    if prompt.contains("fixture partial quota redraw") {
        let writer = prompt.contains("writer partial quota");
        if policy_model == Some("gpt-6-luna") {
            if writer {
                fs::write("quota-partial.txt", "existing implementation survives")?;
                fs::write(
                    "quota-worktree.txt",
                    std::env::current_dir()?.as_os_str().as_encoded_bytes(),
                )?;
            }
            for frame in [
                json!({"type":"thread.started","thread_id":session}),
                json!({"type":"item.completed","item":{"type":"agent_message","text":"Partial implementation and verification notes"}}),
                json!({"type":"turn.completed","usage":{"input_tokens":38000,"output_tokens":2280}}),
                json!({"type":"turn.failed","error":{"message":"usage limit reached","codex_error_info":"usage_limit_exceeded"}}),
            ] {
                writeln!(io::stdout(), "{frame}")?;
            }
            return Ok(());
        }
        assert_eq!(policy_model, Some("gpt-6.1-terra"));
        assert!(!args.iter().any(|arg| arg == "resume"));
        if writer {
            assert!(prompt.contains("The previous member stopped on quota."));
            assert!(prompt.contains("quota-partial.txt"));
            assert!(prompt.contains("quota-worktree.txt"));
            assert!(prompt.contains("Inspect the retained worktree and any commits"));
            assert!(prompt.contains("rather than restart"));
            assert_eq!(
                fs::read_to_string("quota-partial.txt")?,
                "existing implementation survives"
            );
            assert_eq!(
                fs::read("quota-worktree.txt")?,
                std::env::current_dir()?.as_os_str().as_encoded_bytes()
            );
            fs::write("quota-successor.txt", "continued existing implementation")?;
        } else {
            assert!(!prompt.contains("Inspect the retained worktree"));
        }
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
    if prompt.contains("fixture uncatalogued refusal") {
        assert_eq!(policy_model, Some("uncatalogued-fixture-model"));
        writeln!(io::stderr(), "fixture provider rejected uncatalogued model")?;
        return Err("fixture provider rejected uncatalogued model".into());
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
    let answer = if prompt.contains("fixture writer shell") {
        format!("{}{DONE_FENCE}", writer_shell_probe()?)
    } else if prompt.contains("fixture council findings") {
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
    } else if prompt.contains("fixture slow result")
        || prompt.contains("fixture unknown verdict")
        || prompt.contains("check renamed path")
    {
        let lens = prompt
            .lines()
            .find(|line| line.starts_with("Lens: "))
            .unwrap_or("Lens: unknown");
        let denied = fs::write("review-write-sentinel", b"forbidden").is_err();
        let verdict = if prompt.contains("fixture unknown verdict") {
            "No recognised verdict"
        } else {
            "VERDICT: PASS"
        };
        format!(
            "{lens}\nwrite denied: {denied}\n```findings\n{{\"schema_version\":1,\"findings\":[]}}\n```\n{verdict}{DONE_FENCE}"
        )
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

/// Exercise the same public writer command with an isolated invocation directory.
fn writer_cli_launches_from_cwd(location: &str) -> Result {
    let native_available = graph_fixture_available()?;
    let mut rig = policy_codex_fixture()?;
    let cleanup = rig.process_cleanup();
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.routing.pools]\njunior=[{model='codex/gpt-6-luna',effort='low'}]\n",
    )?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    for name in ["AGENTS.md", "CLAUDE.md", "HARNESS.md"] {
        fs::write(rig.project.join(name), "original instructions\n")?;
    }
    for args in [
        vec!["add", "."],
        vec!["commit", "-qm", "instructions"],
        vec![
            "worktree",
            "add",
            "-qb",
            "writer-cwd",
            ".worktrees/writer-cwd",
        ],
    ] {
        let output = rig.execute(&git, &args, None)?;
        assert_eq!(code(&output), 0, "{output:?}");
    }
    let tree = rig.project.join(".worktrees/writer-cwd");
    fs::create_dir_all(tree.join("nested/directory"))?;
    // TestRig uses project as the child's cwd; PROVENANT_ROOT selects assets.
    rig.project = match location {
        "worktree" => tree.clone(),
        "subdirectory" => tree.join("nested/directory"),
        _ => rig.project.clone(),
    };
    let output = rig.command(&[
        "run",
        "fixture writer shell",
        "--tier",
        "junior",
        "--mode",
        "worktree-write",
        "--worktree",
        tree.to_str().ok_or("tree")?,
        "--wait=30s",
        "--json",
    ])?;
    let response: Value = serde_json::from_slice(&output.stdout.bytes)?;
    let node = &response["data"]["checkpoint"]["nodes"][0];
    assert!(
        node["attempts"][0]["attempt_ref"].is_string(),
        "{location}: {response}; {}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    // Admission is checked on every host. Native execution is also checked
    // when the sandbox probe succeeds (CI requires that probe on macOS).
    if !native_available {
        return Ok(());
    }
    assert_eq!(code(&output), 0, "{location}: {response}");
    assert_eq!(node["state"], "succeeded", "{location}: {response}");
    for name in ["AGENTS.md", "CLAUDE.md", "HARNESS.md"] {
        assert_eq!(
            fs::read_to_string(tree.join(name))?,
            "edited by lane shell\n"
        );
    }
    cleanup.disarm();
    Ok(())
}

fn writer_cli_launches_from_worktree_root() -> Result {
    writer_cli_launches_from_cwd("worktree")
}

fn writer_cli_launches_from_worktree_subdirectory() -> Result {
    writer_cli_launches_from_cwd("subdirectory")
}

fn writer_cli_launches_from_primary_checkout() -> Result {
    writer_cli_launches_from_cwd("primary")
}

fn remote_only_writer_cli_starts_at_remote_tip() -> Result {
    let rig = policy_codex_fixture()?;
    let git = provenant_os::git::git_executable().ok_or("git")?;
    let remote = rig.root.join("remote.git");
    for args in [
        vec!["init", "--bare", remote.to_str().ok_or("remote")?],
        vec!["commit", "--allow-empty", "-qm", "base"],
        vec!["branch", "remote-only"],
        vec!["remote", "add", "origin", remote.to_str().ok_or("remote")?],
        vec!["push", "origin", "remote-only"],
    ] {
        let output = rig.execute(&git, &args, None)?;
        assert_eq!(code(&output), 0, "{args:?}: {output:?}");
    }
    let remote_tip = {
        let output = rig.execute(&git, &["rev-parse", "refs/heads/remote-only"], None)?;
        String::from_utf8(output.stdout.bytes)?.trim().to_owned()
    };
    for args in [
        vec!["branch", "-D", "remote-only"],
        vec!["update-ref", "-d", "refs/remotes/origin/remote-only"],
        vec![
            "config",
            "--replace-all",
            "remote.origin.fetch",
            "+refs/heads/elsewhere:refs/remotes/origin/elsewhere",
        ],
        vec!["commit", "--allow-empty", "-qm", "different local tip"],
    ] {
        let output = rig.execute(&git, &args, None)?;
        assert_eq!(code(&output), 0, "{args:?}: {output:?}");
    }
    let auth = br#"{"tokens":{"account_id":"fixture-one"}}"#;
    let key = provenant_providers::identity::codex_account(auth).ok_or("account key")?;
    support::register_account(&rig, "codex", "fixture", &key, &rig.root.join("codex"))
        .map_err(|e| format!("register account: {e}"))?;
    let args = vec![
        "run",
        "remote writer",
        "--mode",
        "worktree-write",
        "--branch",
        "remote-only",
        "--base",
        "HEAD",
        "--wait=30s",
        "--json",
    ];
    let output = rig
        .command(&args)
        .map_err(|e| format!("run {args:?}: {e}"))?;
    let combined = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stdout.bytes),
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    assert_ne!(
        code(&output),
        0,
        "the fixture provider should remain unavailable"
    );
    let head = rig.execute(
        &git,
        &["-C", ".worktrees/remote-only", "rev-parse", "HEAD"],
        None,
    )?;
    assert_eq!(
        String::from_utf8(head.stdout.bytes)?.trim(),
        remote_tip,
        "writer must start at the remote branch tip"
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(
        report["data"]["checkpoint"]["nodes"][0]["source_commit"],
        remote_tip
    );
    assert_eq!(
        report["data"]["checkpoint"]["nodes"][0]["new_worktree_base"], remote_tip,
        "recovery must retain the selected remote tip before provisioning"
    );
    assert!(combined.contains("warning: ignoring --base"), "{combined}");

    let args = vec![
        "run",
        "remote writer",
        "--mode",
        "worktree-write",
        "--worktree",
        "unprovisioned",
        "--wait=30s",
        "--json",
    ];
    for _ in 0..2 {
        let output = rig
            .command(&args)
            .map_err(|e| format!("run {args:?}: {e}"))?;
        let combined = format!(
            "{} {}",
            String::from_utf8_lossy(&output.stdout.bytes),
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        assert_ne!(code(&output), 0, "{args:?}: {combined}");
        assert!(!combined.contains("WRITER_UNKNOWN"), "{combined}");
        assert!(
            combined.contains("worktree")
                && (combined.contains("not found")
                    || combined.contains("missing")
                    || combined.contains("does not exist")),
            "{combined}"
        );
    }
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

/// Read provider session facts from its committed WAL through a graph node.
fn graph_opencode_observes_v2_wal_session() -> Result {
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
            let (model, provider, session_model) = if expected == "model_unavailable" {
                (
                    "opencode/opencode/uncatalogued-fixture-model",
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
            let lane = run_graph_provider(
                &rig,
                operation,
                json!({
                    "id":"provider", "prompt":text, "access":"read_only", "route":{"adapter":"opencode", "model":model}
                }),
            )?;
            assert_eq!(lane["state"], expected, "{lane}");
            let account = if provider == "opencode-go" {
                "go"
            } else {
                "zen"
            };
            assert_eq!(lane["account"], account, "{lane}");
            // Unlisted models must reach the provider exactly as requested.
            assert_eq!(lane["applied_route"]["model"], model, "{lane}");
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
                        .is_some_and(|reason| reason.starts_with("provider quota exhausted; ")),
                    "{lane}"
                );
            } else {
                // The provider's own message is not projected; the typed
                // outcome and its fixed sentence are.
                assert_eq!(lane["state"], "model_unavailable", "{lane}");
                assert_eq!(
                    lane["reason"],
                    "the model is unavailable; check the selected model or wait for capacity"
                );
                assert!(!lane["reason"].to_string().contains("misspelt"), "{lane}");
            }
        }
        rig.assert_legacy_unchanged()?;
        cleanup.disarm();
    }
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
        "route":{"kind":"adapter","provider":"host-native","name":"ci-fixer"}, "account":"pro", "authority_digest":Digest::from_bytes([4;32]), "provider_timeout_seconds":20, "delivery_id":null
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
    assert!(error.to_string().contains("domains.fabric.host_native"));
    assert!(!error.to_string().contains("absent"));
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

    Ok(())
}

fn claude_dispatch(host_native: bool, pool: bool) -> Result {
    let rig = claude_fixture()?;
    // Open-network Claude dispatch must not require a Rust installation.
    assert!(!rig.home.join(".cargo").exists());
    assert!(!rig.home.join(".rustup").exists());
    let cleanup = rig.process_cleanup();
    let account = rig.home.join(".claude-pro");
    if host_native {
        writeln!(
            OpenOptions::new()
                .append(true)
                .open(rig.state.join("config.toml"))?,
            "\n[domains.fabric.host_native.ci-fixer]\nkind = 'claude-agent'\nmodel = 'claude/claude-sonnet-5-5'\neffort = 'medium'\ndescription = 'Repair CI'\nprompt = 'Repair failing CI checks.'"
        )?;
    } else if pool {
        writeln!(
            OpenOptions::new()
                .append(true)
                .open(rig.state.join("config.toml"))?,
            "\n[domains.routing.pools]\nsecret-fixture = [{{ model = 'claude/claude-sonnet-5-5', effort = 'medium' }}]"
        )?;
    }
    let graph_route = if pool {
        json!({"alias":"secret-fixture"})
    } else if host_native {
        json!({"adapter":"host-native","name":"ci-fixer"})
    } else {
        json!({"adapter":"claude","model":"claude-sonnet-5-5","effort":"medium"})
    };
    let mut node = json!({
        "id":"provider", "prompt":if host_native { "fixture host-native" } else if pool { "prompt-sentinel-2719-indigo-owl" } else { "reply OK" },
        "access":"read_only", "attempt":{"account":"pro"}
    });
    if host_native {
        node["routes"] = json!([
            graph_route,
            {"adapter":"claude","model":"claude-sonnet-5-5","effort":"medium"}
        ]);
    } else {
        node["route"] = graph_route;
    }
    let lane = run_graph_provider(&rig, "fixture-claude", node)?;
    if !cfg!(target_os = "macos") {
        assert_eq!(lane["state"], "rejected", "{lane}");
        assert_eq!(lane["cause"], "pre_launch_refusal", "{lane}");
        assert_eq!(lane["terminal_cause"], "unsupported_platform", "{lane}");
        assert!(lane["started_at"].is_null(), "{lane}");
        assert!(lane["applied_route"].is_null(), "{lane}");
        assert!(account.join("refreshed").exists());
        rig.assert_legacy_unchanged()?;
        cleanup.disarm();
        return Ok(());
    }
    assert_eq!(lane["state"], "succeeded", "{lane}");
    assert_eq!(lane["account"], "pro");
    if pool {
        assert_eq!(lane["requested_route"]["kind"], "named");
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

fn simultaneous_first_launches_pass_cutover_guard() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = claude_fixture()?;
    let _cleanup = rig.process_cleanup();
    fs::write(
        rig.home.join(".claude-pro/.credentials.json"),
        br#"{"claudeAiOauth":{"accessToken":"credential-sentinel-2719-violet-fox","expiresAt":4102444800000}}"#,
    )?;
    writeln!(
        OpenOptions::new()
            .append(true)
            .open(rig.state.join("config.toml"))?,
        "\n[domains.routing.pools]\njunior = [{{ model = 'claude/claude-sonnet-5-5', effort = 'medium' }}]"
    )?;
    let binary = option_env!("CARGO_BIN_EXE_provenant").ok_or("binary")?;
    let mut children = Vec::new();
    for index in 0..4 {
        children.push(
            std::process::Command::new(binary)
                .current_dir(&rig.project)
                .env_clear()
                .envs(rig.env.iter().cloned())
                .arg("--state-dir")
                .arg(&rig.state)
                .args([
                    "run",
                    &format!("parallel fixture {index}"),
                    "--tier",
                    "junior",
                    "--wait=30s",
                    "--json",
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()?,
        );
    }
    for child in children {
        let output = child.wait_with_output()?;
        let text = format!(
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!text.contains("UPGRADE_DRAIN_REQUIRED"), "{text}");
        assert!(output.status.success(), "{text}");
        let result: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(result["data"]["status"], "complete", "{result}");
        assert_eq!(result["data"]["outcome"], "succeeded", "{result}");
    }
    Ok(())
}

fn writer_registry_fixture() -> Result<TestRig> {
    let rig = policy_codex_fixture()?;
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.routing.pools]\njunior=[{model='codex/gpt-6-luna',effort='low'}]\n",
    )?;
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let seeded = rig.execute(&git, &["commit", "--allow-empty", "-qm", "base"], None)?;
    assert_eq!(code(&seeded), 0, "{seeded:?}");
    Ok(rig)
}

/// Admission must survive repository-wide contention even without native execution.
fn simultaneous_writer_launches_create_attempts() -> Result {
    let rig = writer_registry_fixture()?;
    let _cleanup = rig.process_cleanup();
    let binary = option_env!("CARGO_BIN_EXE_provenant").ok_or("binary")?;
    let barrier = std::sync::Barrier::new(6);
    std::thread::scope(|scope| -> Result {
        let mut launches = Vec::new();
        for index in 0..6 {
            let barrier = &barrier;
            let rig = &rig;
            launches.push(scope.spawn(move || {
                barrier.wait();
                std::process::Command::new(binary)
                    .current_dir(&rig.project)
                    .env_clear()
                    .envs(rig.env.iter().cloned())
                    .arg("--state-dir")
                    .arg(&rig.state)
                    .args([
                        "run",
                        "fixture writer",
                        "--tier",
                        "junior",
                        "--mode",
                        "worktree-write",
                        "--branch",
                        &format!("parallel-{index}"),
                        "--base",
                        "HEAD",
                        "--wait=55s",
                        "--json",
                    ])
                    .output()
            }));
        }
        for launch in launches {
            let output = launch.join().map_err(|_| "launch thread panicked")??;
            let response: Value = serde_json::from_slice(&output.stdout)?;
            assert!(
                response["data"]["checkpoint"]["nodes"][0]["attempts"][0]["attempt_ref"]
                    .is_string(),
                "{response}; {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    })
}

/// A held registry consumes the launch budget, then reports a typed CLI error.
fn writer_registry_timeout_is_bounded_and_reported() -> Result {
    use provenant_core::{Clock, RootInputs, WaitBudget};
    use provenant_os::{
        clock::SystemClock,
        fs::{Directory, LockMode, SystemPathResolver},
    };
    let rig = writer_registry_fixture()?;
    let _cleanup = rig.process_cleanup();
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&SystemPathResolver)?;
    let binding = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(fs::canonicalize(rig.project.join(".git"))?)?,
    )?;
    let registry = rig
        .state
        .join("worktrees")
        .join(binding.project_id().to_string());
    fs::create_dir_all(&registry)?;
    fs::set_permissions(&registry, fs::Permissions::from_mode(0o700))?;
    let directory = Directory::open_anchor(&registry)?;
    let clock = SystemClock;
    let _held = directory.lock(
        std::ffi::OsStr::new("worktrees.lock"),
        LockMode::Exclusive,
        WaitBudget::nonblocking(clock.mono_now()),
        &provenant_core::CancelToken::new(),
        &clock,
    )?;
    let start = std::time::Instant::now();
    let output = rig.command(&[
        "run",
        "fixture writer",
        "--tier",
        "junior",
        "--mode",
        "worktree-write",
        "--branch",
        "blocked-writer",
        "--base",
        "HEAD",
        "--wait=1s",
        "--json",
    ])?;
    let elapsed = start.elapsed();
    assert!(
        elapsed >= std::time::Duration::from_secs(1),
        "launch did not wait for the held registry: {elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(4),
        "launch exceeded its one-second wait budget: {elapsed:?}"
    );
    assert_ne!(code(&output), 0, "{output:?}");
    let response: Value = serde_json::from_slice(&output.stdout.bytes)?;
    assert_eq!(response["error"]["code"], "writer_lock_busy", "{response}");
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("bounded wait")),
        "{response}"
    );
    assert_eq!(
        response["data"]["checkpoint"]["nodes"][0]["attempts"],
        json!([]),
        "{response}"
    );
    Ok(())
}

/// A quota refusal with no generated work redraws the same alias in the graph.
fn graph_read_only_alias_retries_another_member_after_empty_launch_quota() -> Result {
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
    let started = rig.command(&["run", "--file", "graph.json", "--wait=30s", "--json"])?;
    assert_eq!(code(&started), 0, "{started:?}");
    let status: Value = serde_json::from_slice(&started.stdout.bytes)?;
    assert_eq!(status["data"]["status"], "complete", "{status}");
    assert_eq!(
        status["data"]["checkpoint"]["nodes"][0]["state"], "succeeded",
        "{status}"
    );
    // Graph retry archives the settled quota attempt and creates a new sealed
    // key for the same alias. The shared store has one run per graph attempt.
    let node = &status["data"]["checkpoint"]["nodes"][0];
    let previous = node["previous_attempts"]
        .as_array()
        .ok_or("previous attempts")?;
    let attempts = node["attempts"].as_array().ok_or("attempts")?;
    assert_eq!(previous.len(), 1, "{status}");
    assert_eq!(attempts.len(), 1, "{status}");
    assert_eq!(previous[0]["outcome"], "usage_limited", "{status}");
    assert_eq!(attempts[0]["outcome"], "succeeded", "{status}");
    assert_eq!(previous[0]["route"]["alias"], "retry-test", "{status}");
    assert_eq!(attempts[0]["route"], previous[0]["route"], "{status}");
    assert_ne!(attempts[0]["key"], previous[0]["key"], "{status}");
    assert_eq!(attempts[0]["key"]["retry_generation"], 1, "{status}");
    assert_eq!(
        attempts[0]["key"]["previous_attempt_ref"], previous[0]["attempt_ref"],
        "{status}"
    );
    let attempt_id = |attempt: &Value| -> Result<String> {
        Ok(attempt["attempt_ref"]
            .as_str()
            .ok_or("attempt reference")?
            .rsplit_once('/')
            .ok_or("qualified attempt reference")?
            .1
            .to_owned())
    };
    let first_id = attempt_id(&previous[0])?;
    let second_id = attempt_id(&attempts[0])?;
    let lanes = rig.command(&["status", "--all", "--json"])?;
    assert_eq!(code(&lanes), 0, "{lanes:?}");
    let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
    let rows: Vec<_> = lanes["data"]["rows"]
        .as_array()
        .ok_or("rows")?
        .iter()
        .filter(|row| row["kind"] == "attempt")
        .collect();
    assert_eq!(rows.len(), 2, "{lanes}");
    let first = rows
        .iter()
        .find(|row| {
            row["attempt_id"]
                .as_str()
                .is_some_and(|id| id.ends_with(&first_id))
        })
        .ok_or_else(|| format!("first attempt {first_id}: {lanes}"))?;
    let second = rows
        .iter()
        .find(|row| {
            row["attempt_id"]
                .as_str()
                .is_some_and(|id| id.ends_with(&second_id))
        })
        .ok_or("retry attempt")?;
    assert_eq!(first["state"], "quota_limited", "{lanes}");
    assert_eq!(
        first["applied_route"]["model"], "codex/gpt-6-luna",
        "{lanes}"
    );
    assert_ne!(first["run_id"], second["run_id"], "{lanes}");
    assert!(first["superseded_by"].is_null(), "{lanes}");
    assert!(second["retry_of"].is_null(), "{lanes}");
    let database = rusqlite::Connection::open(rig.state.join("run.sqlite3"))?;
    let retained: Vec<u8> = database.query_row(
        "SELECT record FROM attempts WHERE id=?1",
        [&second_id],
        |row| row.get(0),
    )?;
    let retained: provenant_run::AttemptRecord = serde_json::from_slice(&retained)?;
    let excluded = retained
        .profile
        .knobs
        .get("routing.graph_exclude")
        .ok_or("excluded spent model")?;
    assert_eq!(excluded, &json!(["codex/gpt-6-luna"]));
    assert_eq!(second["state"], "succeeded", "{lanes}");
    assert_eq!(
        second["applied_route"]["model"], "codex/gpt-6.1-terra",
        "the spent member is excluded: {lanes}"
    );
    cleanup.disarm();
    Ok(())
}

/// A quota stop after output redraws the alias for either lane class. Writers
/// must retain the first member's files and receive continuation instructions.
fn partial_quota_alias_fixture(writer: bool) -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = policy_codex_fixture()?;
    let cleanup = rig.process_cleanup();
    fs::write(
        rig.state.join("config.toml"),
        "schema_version=1\n[gc]\nfree_floor='1B'\n[domains.routing]\nsettlement_retries=1\n[domains.routing.pools]\npartial-quota=[{model='codex/gpt-6-luna',weight=1,effort='low'},{model='codex/gpt-6.1-terra',weight=0,effort='low'}]\n",
    )?;
    for (label, account) in [("one", "fixture-one"), ("two", "fixture-two")] {
        let directory = rig.root.join(format!("codex-{label}"));
        fs::create_dir_all(&directory)?;
        let auth = serde_json::to_vec(&json!({"tokens":{"account_id":account}}))?;
        fs::write(directory.join("auth.json"), &auth)?;
        let key = provenant_providers::identity::codex_account(&auth).ok_or("account key")?;
        support::register_account(&rig, "codex", label, &key, &directory)?;
    }
    if writer {
        let git = provenant_os::git::git_executable().ok_or("git")?;
        assert_eq!(
            code(&rig.execute(&git, &["commit", "--allow-empty", "-qm", "base"], None)?),
            0
        );
    }
    if writer {
        let created = rig.command(&[
            "worktree",
            "create",
            "partial-quota",
            "--base",
            "HEAD",
            "--json",
        ])?;
        assert_eq!(code(&created), 0, "{created:?}");
    }
    let mut node = json!({
        "id":"work", "prompt": if writer {
            "fixture partial quota redraw: writer partial quota"
        } else {
            "fixture partial quota redraw: read-only partial quota"
        },
        "access": if writer { "worktree_write" } else { "read_only" },
        "route":{"alias":"partial-quota"}
    });
    if writer {
        node["worktree"] = json!(".worktrees/partial-quota");
        node["delivery"] = json!("none");
    }
    fs::write(
        rig.project.join("graph.json"),
        serde_json::to_vec(&json!({
            "schema_version":2,"graph_id":"partial-quota","nodes":[node]
        }))?,
    )?;
    let output = rig.command(&["run", "--file", "graph.json", "--wait=30s", "--json"])?;
    assert_eq!(code(&output), if writer { 1 } else { 0 }, "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout.bytes)?;
    let node = &result["data"]["checkpoint"]["nodes"][0];
    let previous = node["previous_attempts"]
        .as_array()
        .ok_or("previous attempts")?;
    let current = node["attempts"].as_array().ok_or("current attempts")?;
    assert_eq!(previous.len(), 1, "{result}");
    assert_eq!(current.len(), 1, "{result}");
    assert_eq!(previous[0]["outcome"], "usage_limited", "{result}");
    assert_eq!(current[0]["key"]["retry_generation"], 1, "{result}");
    assert_eq!(
        current[0]["key"]["previous_attempt_ref"], previous[0]["attempt_ref"],
        "{result}"
    );
    let attempt_id = |attempt: &Value| -> Result<String> {
        Ok(attempt["attempt_ref"]
            .as_str()
            .ok_or("attempt reference")?
            .rsplit_once('/')
            .ok_or("qualified reference")?
            .1
            .to_owned())
    };
    let first_id = attempt_id(&previous[0])?;
    let second_id = attempt_id(&current[0])?;
    let status = rig.command(&["status", "--all", "--json"])?;
    assert_eq!(code(&status), 0, "{status:?}");
    let lanes: Value = serde_json::from_slice(&status.stdout.bytes)?;
    let rows: Vec<_> = lanes["data"]["rows"]
        .as_array()
        .ok_or("rows")?
        .iter()
        .filter(|row| row["kind"] == "attempt")
        .collect();
    assert_eq!(rows.len(), 2, "{lanes}");
    let first = rows
        .iter()
        .find(|row| {
            row["attempt_id"]
                .as_str()
                .is_some_and(|id| id.ends_with(&first_id))
        })
        .ok_or_else(|| format!("first: {first_id}: {lanes}"))?;
    let second = rows
        .iter()
        .find(|row| {
            row["attempt_id"]
                .as_str()
                .is_some_and(|id| id.ends_with(&second_id))
        })
        .ok_or("second")?;
    let database = rusqlite::Connection::open(rig.state.join("run.sqlite3"))?;
    let sealed: Vec<u8> = database.query_row(
        "SELECT record FROM attempts WHERE id=?1",
        [&second_id],
        |row| row.get(0),
    )?;
    let sealed: provenant_run::AttemptRecord = serde_json::from_slice(&sealed)?;
    assert_eq!(
        sealed
            .context
            .and_then(|context| context.writer_predecessor),
        writer.then(|| first_id.parse()).transpose()?,
        "graph writer predecessor must survive admission"
    );
    assert_eq!(first["state"], "quota_limited", "{lanes}");
    assert_eq!(
        first["usage"]["output_excluding_reasoning"], 2280,
        "{lanes}"
    );
    assert_eq!(
        first["applied_route"]["model"], "codex/gpt-6-luna",
        "{lanes}"
    );
    assert_eq!(
        second["applied_route"]["model"], "codex/gpt-6.1-terra",
        "spent member must be excluded: {lanes}"
    );
    // The graph owns retry lineage; each provider launch is a separate run.
    assert_ne!(first["run_id"], second["run_id"], "{lanes}");
    assert!(first["superseded_by"].is_null(), "{lanes}");
    assert!(second["retry_of"].is_null(), "{lanes}");
    assert_eq!(
        second["state"],
        if writer { "uncommitted" } else { "succeeded" },
        "{lanes}"
    );
    cleanup.disarm();
    Ok(())
}

fn named_route_writer_retries_quota_with_partial_work() -> Result {
    partial_quota_alias_fixture(true)
}

fn named_route_read_only_retries_quota_with_partial_output() -> Result {
    partial_quota_alias_fixture(false)
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
    // A staged settlement retry appends its note after the safe owner reason.
    assert!(matches!(
        &terminal.state,
        provenant_run::AttemptState::Terminal { reason: Some(reason), .. }
            if reason.starts_with(provenant_records::diagnostic::ProviderFailureFact::Capacity.reason())
                && !reason.contains(message)
    ));
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
                cloud: None,
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
        launch_refusal: None,
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
                terminal_cause: None,
                terminal_note: None,
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
        vec!["status", &attempt],
        vec!["status", &attempt, "--wait=1s"],
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

fn agy_denial_result_stays_out_of_terminal_diagnostics() -> Result {
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
        "status",
        &terminal.attempt_id.to_string(),
        "--all",
        "--json",
    ])?;
    assert_eq!(code(&lanes), 0, "{:?}", lanes.stderr.bytes);
    let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
    let lane = &lanes["data"]["rows"][0];
    assert_eq!(lane["state"], "failed");
    assert_eq!(lane["cause"], "failed");
    for diagnostic in [&lane["reason"], &lane["settlement"]["reason"]] {
        let diagnostic = diagnostic.as_str().ok_or("failure diagnostic")?;
        assert!(!diagnostic.is_empty());
        assert!(!diagnostic.contains("RunCommand"));
    }
    let captured = rig
        .state
        .join("runs")
        .join(terminal.run_id.to_string())
        .join("attempts")
        .join(terminal.attempt_id.to_string())
        .join("stdout.log");
    assert!(fs::read_to_string(captured)?.contains("RunCommand"));
    Ok(())
}

fn agy_denied_command_reaches_result_without_entering_terminal_diagnostics() -> Result {
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
            "status",
            &terminal.attempt_id.to_string(),
            "--all",
            "--json",
        ])?;
        assert_eq!(code(&lanes), 0, "{:?}", lanes.stderr.bytes);
        let lanes: Value = serde_json::from_slice(&lanes.stdout.bytes)?;
        let lane = &lanes["data"]["rows"][0];
        assert_eq!(lane["state"], "failed");
        assert_eq!(lane["cause"], "failed");
        for diagnostic in [&lane["reason"], &lane["settlement"]["reason"]] {
            let diagnostic = diagnostic.as_str().ok_or("failure diagnostic")?;
            assert!(!diagnostic.is_empty());
            assert!(!diagnostic.contains(r#""command" permission"#), "{lanes}");
            assert!(!diagnostic.contains("jetski:"), "{lanes}");
        }
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
        Err(provenant_quota::QuotaError::Refused(reason))
            if reason == provenant_quota::AccountBlock::FreeTierRefusal.repair()
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
                Err(provenant_quota::QuotaError::Refused(reason))
                    if reason == provenant_quota::AccountBlock::FreeTierRefusal.repair()
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
            Err(provenant_quota::QuotaError::Refused(reason))
                if reason == provenant_quota::AccountBlock::ScopedRefusal.repair()
        ));
    }
    admit(&route, resets)?;
    route.model = "gemini-3.8-pro".into();
    admit(&route, now)?;
    Ok(())
}

fn run_graph_provider(rig: &TestRig, graph_id: &str, node: Value) -> Result<Value> {
    run_graph_provider_with_wait(rig, graph_id, node, "5s")
}

fn run_graph_provider_with_wait(
    rig: &TestRig,
    graph_id: &str,
    node: Value,
    wait: &str,
) -> Result<Value> {
    let unlisted = node["route"]["model"] == "opencode/opencode/uncatalogued-fixture-model";
    let document = rig.root.join(format!("{graph_id}.json"));
    fs::write(
        &document,
        serde_json::to_vec(&json!({"schema_version":2,"graph_id":graph_id,"nodes":[node]}))?,
    )?;
    let out = rig.command(&[
        "run",
        "--file",
        document.to_str().ok_or("document")?,
        &format!("--wait={wait}"),
        "--json",
    ])?;
    if unlisted {
        let stderr = String::from_utf8_lossy(&out.stderr.bytes);
        assert!(stderr.contains("not in the catalogue"), "{stderr}");
    }
    let response: Value = serde_json::from_slice(&out.stdout.bytes)?;
    let expected_exit = match response["status"].as_str() {
        Some("ok" | "waiting") => 0,
        Some("failed" | "refused") => 1,
        Some("cancelled") => 130,
        other => {
            return Err(format!("unexpected graph response status {other:?}: {response}").into());
        }
    };
    assert_eq!(code(&out), expected_exit, "{response}");
    assert!(response["error"].is_null(), "{response}");
    assert!(response["data"]["checkpoint"].is_object(), "{response}");
    let reference = response["data"]["checkpoint"]["nodes"][0]["attempts"][0]["attempt_ref"]
        .as_str()
        .ok_or_else(|| format!("attempt reference missing: {response}"))?;
    let (_, attempt) = reference.split_once('/').ok_or("attempt reference")?;
    let mut inputs = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let identity =
        provenant_cli::fabric::Context::open(roots.clone(), &rig.home, &rig.project)?.identity;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let call = provenant_run::Request {
        clock: &clock,
        cancel: &cancel,
    };
    let context = provenant_run::ExecutionContext {
        state_root: roots.state,
        project_id: identity.project,
        actor_id: identity.inbox.actor,
        read_prompts: true,
    };
    let store = provenant_run::RunStore::open(&context, &call)?;
    let record = store.record(attempt.parse()?, &call)?;
    Ok(serde_json::to_value(provenant_cli::run::project_lane(
        &record,
        &[],
        rig.state.as_path(),
        None,
        false,
    ))?)
}

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
    let lane = run_graph_provider(
        &rig,
        "opencode-hanging-limit",
        json!({
            "id":"provider", "prompt":"free limit then hang", "access":"read_only",
            "route":{"adapter":"opencode","model":"opencode/glm-5.3-flash-free"}
        }),
    )?;
    let run = lane["run_id"].as_str().ok_or("run")?;
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
        let lane = run_graph_provider(
            &rig,
            "stderr-quota",
            json!({
                "id":"provider", "prompt":prompt_text, "access":"read_only",
                "route":{"adapter":"opencode","model":"opencode/opencode/glm-5.3-flash-free"}
            }),
        )?;
        let run = lane["run_id"].as_str().ok_or("run")?;
        assert_eq!(lane["state"], expected, "{prompt_text}: {lane}");
        if expected == "succeeded" {
            assert_eq!(
                lane["observed_route"]["model"], "opencode/opencode/glm-5.3-flash-free",
                "{lane}"
            );
        }
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

/// Medium: real confined writer shells for both primary adapters. The files
/// are tracked before launch so quarantine/protected-root regressions cannot
/// hide behind a newly created, unprotected instruction filename.
fn writer_shell_edits_instructions_and_redacts_logs() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    for adapter in ["codex", "claude"] {
        let rig = if adapter == "claude" {
            claude_fixture()?
        } else {
            policy_codex_fixture()?
        };
        let cleanup = rig.process_cleanup();
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        for name in ["AGENTS.md", "CLAUDE.md", "HARNESS.md"] {
            fs::write(rig.project.join(name), "original instructions\n")?;
        }
        fs::write(rig.project.join(".gitignore"), "setup-assets/\n")?;
        for args in [
            vec!["add", "AGENTS.md", "CLAUDE.md", "HARNESS.md", ".gitignore"],
            vec!["commit", "-qm", "instructions"],
        ] {
            let out = rig.execute(&git, &args, None)?;
            assert_eq!(code(&out), 0, "{out:?}");
        }
        // A plain Git worktree has no Provenant creation receipt. Setup scripts
        // may leave ignored files and links without making it unsafe to launch.
        let created = rig.execute(
            &git,
            &[
                "worktree",
                "add",
                "-b",
                "shell-evidence",
                ".worktrees/shell-evidence",
                "HEAD",
            ],
            None,
        )?;
        assert_eq!(code(&created), 0, "{created:?}");
        let tree = rig.project.join(".worktrees/shell-evidence");
        fs::create_dir_all(tree.join("setup-assets"))?;
        fs::write(tree.join("setup-assets/installed.txt"), "setup output\n")?;
        std::os::unix::fs::symlink("installed.txt", tree.join("setup-assets/current"))?;
        let mut node = json!({
            "id":"writer", "prompt":"fixture writer shell", "access":"worktree_write",
            "worktree":".worktrees/shell-evidence", "delivery":"none",
            "route": if adapter == "claude" {
                json!({"adapter":"claude","model":"claude-sonnet-5-5","effort":"medium"})
            } else {
                json!({"adapter":"codex","model":"gpt-6-luna","effort":"low"})
            }
        });
        if adapter == "claude" {
            node["attempt"] = json!({"account":"pro"});
        }
        let lane = run_graph_provider_with_wait(&rig, "shell-evidence", node, "30s")?;
        assert_eq!(lane["state"], "succeeded", "{lane}");
        assert_eq!(
            fs::read_to_string(tree.join("setup-assets/current"))?,
            "setup output\n"
        );
        for name in ["AGENTS.md", "CLAUDE.md", "HARNESS.md"] {
            assert_eq!(
                fs::read_to_string(tree.join(name))?,
                "edited by lane shell\n"
            );
        }
        assert!(!tree.join("shell-scratch").exists());
        // Settlement must import the shell's private Git commit intact.
        let head = rig.execute(
            &git,
            &["-C", tree.to_str().ok_or("tree")?, "show", "HEAD:AGENTS.md"],
            None,
        )?;
        assert_eq!(code(&head), 0, "{head:?}");
        assert_eq!(head.stdout.bytes, b"edited by lane shell\n");
        let run = lane["run_id"].as_str().ok_or("run")?;
        let id = lane["attempt_id"].as_str().ok_or("attempt")?;
        let attempt = rig.state.join("runs").join(run).join("attempts").join(id);
        let result = fs::read_to_string(attempt.join("result.md"))?;
        assert!(result.contains("shell completed\n[REDACTED]"), "{result}");
        assert_eq!(
            fs::read_to_string(attempt.join("stderr.log"))?,
            "shell stderr [REDACTED]\n"
        );
        assert_no_printed_credential(&rig.state.join("runs").join(run))?;
        cleanup.disarm();
    }
    Ok(())
}

/// Medium: the public --model front door must warn, launch and preserve the
/// provider's failure rather than refusing the uncatalogued ID at admission.
fn uncatalogued_cli_model_launches_and_surfaces_provider_failure() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = policy_codex_fixture()?;
    let cleanup = rig.process_cleanup();
    let auth = br#"{"tokens":{"account_id":"fixture-one"}}"#;
    let key = provenant_providers::identity::codex_account(auth).ok_or("account key")?;
    support::register_account(&rig, "codex", "fixture", &key, &rig.root.join("codex"))?;
    for (prompt, expected) in [
        ("fixture uncatalogued success", 0),
        ("fixture uncatalogued refusal", 1),
    ] {
        let out = rig.command(&[
            "run",
            prompt,
            "--model",
            "codex/uncatalogued-fixture-model",
            "--wait=30s",
            "--json",
        ])?;
        assert_eq!(code(&out), expected, "{out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr.bytes).contains("not in the catalogue"),
            "{out:?}"
        );
        let response: Value = serde_json::from_slice(&out.stdout.bytes)?;
        let reference = response["data"]["checkpoint"]["nodes"][0]["attempts"][0]["attempt_ref"]
            .as_str()
            .ok_or("attempt reference")?;
        let output = rig.command(&["output", reference, "--part", "stderr", "--json"])?;
        assert_eq!(code(&output), 0, "{output:?}");
        if expected == 1 {
            assert!(
                String::from_utf8_lossy(&output.stdout.bytes)
                    .contains("fixture provider rejected uncatalogued model"),
                "{output:?}"
            );
        }
    }
    cleanup.disarm();
    Ok(())
}

fn assert_no_printed_credential(root: &std::path::Path) -> Result {
    let mut pending = vec![root.to_owned()];
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
    Ok(())
}

fn printed_credentials_leave_no_plaintext_in_run_files() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = policy_codex_fixture()?;
    let cleanup = rig.process_cleanup();
    for mode in ["plain", "escaped"] {
        let lane = run_graph_provider(
            &rig,
            &format!("printed-{mode}"),
            json!({
                "id":"provider", "prompt":format!("fixture printed credential {mode}"), "access":"read_only",
                "route":{"adapter":"codex","model":"gpt-6-luna","effort":"low"}
            }),
        )?;
        let run = lane["run_id"].as_str().ok_or("run")?;
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
        assert_no_printed_credential(&run_dir)?;
    }
    cleanup.disarm();
    Ok(())
}

fn pool_receipt_excludes_prompt_and_credential_from_artifacts() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    claude_dispatch(false, true)
}

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
                {"id":"producer","use":"task","brief":"fixture large result","access":"read_only","route":route},
                {"id":"Review","use":"task","brief":"fixture case result Review","access":"read_only","route":route},
                {"id":"review","use":"task","brief":"fixture case result review","access":"read_only","route":route},
                {"id":"consumer","use":"task","brief":"fixture read upstream file","access":"read_only",
                 "after":["producer","Review","review"],
                 "inputs":[{"node":"producer","outputs":["result"]},
                           {"node":"Review","outputs":["result"]},
                           {"node":"review","outputs":["result"]}],"route":route}
            ]}),
        )?,
    )?;
    let started = rig.command(&["run", "--file", "graph.json", "--json"])?;
    assert_eq!(
        code(&started),
        0,
        "{}",
        String::from_utf8_lossy(&started.stdout.bytes)
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let status = loop {
        let status = rig.command(&["status", "full-inputs", "--json"])?;
        assert_eq!(code(&status), 0, "{status:?}");
        let status: Value = serde_json::from_slice(&status.stdout.bytes)?;
        if status["data"]["rows"].as_array().is_some_and(|rows| {
            rows.iter().any(|row| {
                row["kind"] == "graph"
                    && row["graph_id"] == "full-inputs"
                    && row["state"] == "complete"
            })
        }) {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "graph did not complete: {status}"
        );
        std::thread::park_timeout(std::time::Duration::from_millis(50));
    };
    // Status deliberately returns unified rows. Inspect the durable checkpoint
    // to verify full input transport and its confinement boundary separately.
    let identity = rig.command(&["fabric", "identity", "--json"])?;
    assert_eq!(code(&identity), 0);
    let identity: Value = serde_json::from_slice(&identity.stdout.bytes)?;
    let project = serde_json::from_value(identity["data"]["project"].clone())?;
    let mut inputs = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let checkpoint = provenant_fabric_runtime::graph::FileJournal::read_checkpoint(
        &roots.state,
        project,
        "full-inputs",
    )?
    .ok_or("full-inputs checkpoint")?;
    let checkpoint = serde_json::to_value(checkpoint)?;
    let nodes = checkpoint["nodes"].as_array().ok_or("nodes")?;
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
    let started = rig.command(&["run", "--file", "graph.json", "--json"])?;
    assert_eq!(
        code(&started),
        0,
        "{}",
        String::from_utf8_lossy(&started.stdout.bytes)
    );
    // Status only observes: the guardian's advance is the sole writer here.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let error = loop {
        let status = rig.command(&["status", "broken", "--json"])?;
        assert_eq!(code(&status), 0);
        let status: Value = serde_json::from_slice(&status.stdout.bytes)?;
        if let Some(error) = status["data"]["rows"][0]["reason"].as_str() {
            break error.to_owned();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no advance error recorded: {status}"
        );
        std::thread::park_timeout(std::time::Duration::from_millis(50));
    };
    assert!(!error.is_empty());
    let human = rig.command(&["status", "broken"])?;
    assert!(
        String::from_utf8_lossy(&human.stdout.bytes).contains(&error),
        "{}",
        String::from_utf8_lossy(&human.stdout.bytes)
    );
    cleanup.disarm();
    Ok(())
}

#[cfg(target_os = "macos")]
fn graph_launch_proceeds_when_provider_update_is_busy() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
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
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::metadata(rig.state.join("provider-updates/agy.lock"))?;
    let started = std::time::Instant::now();
    let lane = run_graph_provider_with_wait(
        &rig,
        "busy-provider",
        json!({
            "id":"provider", "prompt":format!("report lease status {} {}", metadata.dev(), metadata.ino()),
            "access":"read_only", "route":{"adapter":"agy","model":"gemini-3.8-flash"}
        }),
        "1s",
    )?;
    assert!(started.elapsed() < std::time::Duration::from_secs(8));
    let run = lane["run_id"].as_str().ok_or("run id")?;
    let lanes = rig.command(&["status", run, "--wait=60s", "--json"])?;
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

fn graph_codex_rollout_binds_observed_route_and_missing_rollout_is_explicit() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    let rig = policy_codex_fixture()?;
    let cleanup = rig.process_cleanup();
    for (id, brief, has_rollout) in [
        ("rollout-present", "reply OK", true),
        ("rollout-missing", "fixture missing rollout", false),
    ] {
        let lane = run_graph_provider(
            &rig,
            id,
            json!({
                "id":"provider", "prompt":brief, "access":"read_only",
                "route":{"adapter":"codex","model":"gpt-6-luna","effort":"low"}
            }),
        )?;
        if has_rollout {
            assert_eq!(lane["state"], "succeeded", "{lane}");
            assert_eq!(
                lane["observed_route"]["model"], "codex/gpt-6-luna",
                "{lane}"
            );
            assert_eq!(
                lane["transcript"]["session"],
                "01234567-89ab-cdef-0123-456789abcdef"
            );
        } else {
            assert_eq!(lane["state"], "failed", "{lane}");
            assert_eq!(lane["terminal_cause"], "identity_unobserved", "{lane}");
            assert!(lane["observed_route"].is_null(), "{lane}");
            assert_eq!(
                lane["transcript"]["session"], "01234567-89ab-cdef-0123-456789abcdef",
                "{lane}"
            );
            assert!(lane["transcript"]["path"].is_null(), "{lane}");
        }
    }
    cleanup.disarm();
    Ok(())
}

fn graph_host_native_launch_uses_the_provider_contract() -> Result {
    if !graph_fixture_available()? {
        return Ok(());
    }
    claude_dispatch(true, false)
}

fn main() -> ExitCode {
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
        type NamedTest = (&'static str, fn() -> Result);
        let tests: &[NamedTest] = &[
            (
                "writer_registry_timeout_is_bounded_and_reported",
                writer_registry_timeout_is_bounded_and_reported,
            ),
            (
                "simultaneous_writer_launches_create_attempts",
                simultaneous_writer_launches_create_attempts,
            ),
            (
                "simultaneous_first_launches_pass_cutover_guard",
                simultaneous_first_launches_pass_cutover_guard,
            ),
            (
                "writer_shell_edits_instructions_and_redacts_logs",
                writer_shell_edits_instructions_and_redacts_logs,
            ),
            (
                "uncatalogued_cli_model_launches_and_surfaces_provider_failure",
                uncatalogued_cli_model_launches_and_surfaces_provider_failure,
            ),
            (
                "writer_cli_launches_from_worktree_root",
                writer_cli_launches_from_worktree_root,
            ),
            (
                "writer_cli_launches_from_worktree_subdirectory",
                writer_cli_launches_from_worktree_subdirectory,
            ),
            (
                "writer_cli_launches_from_primary_checkout",
                writer_cli_launches_from_primary_checkout,
            ),
            (
                "remote_only_writer_cli_starts_at_remote_tip",
                remote_only_writer_cli_starts_at_remote_tip,
            ),
            #[cfg(target_os = "macos")]
            (
                "graph_launch_proceeds_when_provider_update_is_busy",
                graph_launch_proceeds_when_provider_update_is_busy,
            ),
            (
                "graph_settlement_records_a_failed_advance_for_status",
                graph_settlement_records_a_failed_advance_for_status,
            ),
            (
                "graph_node_reads_full_upstream_result_from_its_inputs_directory",
                graph_node_reads_full_upstream_result_from_its_inputs_directory,
            ),
            ("opencode_stderr_live_quota", opencode_stderr_live_quota),
            (
                "opencode_usage_limit_stops_hanging_provider",
                opencode_usage_limit_stops_hanging_provider,
            ),
            (
                "pool_receipt_excludes_prompt_and_credential_from_artifacts",
                pool_receipt_excludes_prompt_and_credential_from_artifacts,
            ),
            (
                "printed_credentials_leave_no_plaintext_in_run_files",
                printed_credentials_leave_no_plaintext_in_run_files,
            ),
            (
                "receipt_output_reports_unknown_route_for_settled_lost_attempt",
                receipt_output_reports_unknown_route_for_settled_lost_attempt,
            ),
            (
                "output_names_the_attempt_it_reads_and_writes_or_locates_parts",
                output_names_the_attempt_it_reads_and_writes_or_locates_parts,
            ),
            (
                "receipt_output_resolves_run_task_and_attempt",
                receipt_output_resolves_run_task_and_attempt,
            ),
            (
                "receipt_output_classifies_writers_and_refuses_ambiguous_or_running_attempts",
                receipt_output_classifies_writers_and_refuses_ambiguous_or_running_attempts,
            ),
            (
                "opencode_usage_limit_settlement_is_bounded",
                opencode_usage_limit_settlement_is_bounded,
            ),
            (
                "host_native_preparation_uses_the_provider_contract",
                host_native_preparation_uses_the_provider_contract,
            ),
            (
                "graph_host_native_launch_uses_the_provider_contract",
                graph_host_native_launch_uses_the_provider_contract,
            ),
            (
                "graph_codex_rollout_binds_observed_route_and_missing_rollout_is_explicit",
                graph_codex_rollout_binds_observed_route_and_missing_rollout_is_explicit,
            ),
            (
                "graph_read_only_alias_retries_another_member_after_empty_launch_quota",
                graph_read_only_alias_retries_another_member_after_empty_launch_quota,
            ),
            (
                "named_route_writer_retries_quota_with_partial_work",
                named_route_writer_retries_quota_with_partial_work,
            ),
            (
                "named_route_read_only_retries_quota_with_partial_output",
                named_route_read_only_retries_quota_with_partial_output,
            ),
            (
                "graph_opencode_observes_v2_wal_session",
                graph_opencode_observes_v2_wal_session,
            ),
            (
                "opencode_free_pool_cools_second_model",
                opencode_free_pool_cools_second_model,
            ),
            (
                "opencode_free_reset_cools_pool_until_reset_and_restores",
                opencode_free_reset_cools_pool_until_reset_and_restores,
            ),
            (
                "agy_individual_quota_cools_model_until_reset",
                agy_individual_quota_cools_model_until_reset,
            ),
            (
                "agy_denied_command_reaches_result_without_entering_terminal_diagnostics",
                agy_denied_command_reaches_result_without_entering_terminal_diagnostics,
            ),
            (
                "agy_denial_result_stays_out_of_terminal_diagnostics",
                agy_denial_result_stays_out_of_terminal_diagnostics,
            ),
            (
                "codex_capacity_settles_without_quota_refusal",
                codex_capacity_settles_without_quota_refusal,
            ),
        ];
        let names: Vec<_> = tests.iter().map(|(name, _)| *name).collect();
        provenant_supervisor::fixture::run_libtest(&args, &names, |name| {
            let (_, test) = tests
                .iter()
                .find(|(candidate, _)| *candidate == name)
                .ok_or_else(|| io::Error::other(format!("unknown test: {name}")))?;
            test().map_err(|e| io::Error::other(e.to_string()))
        })
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
