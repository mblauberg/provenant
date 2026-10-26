use super::*;
use std::cell::RefCell;

/// Records launches instead of opening Terminal.
#[derive(Default)]
struct Recorder(RefCell<Vec<PathBuf>>);
impl Launcher for Recorder {
    fn launch(&self, script: &Path) -> Result<(), String> {
        self.0.borrow_mut().push(script.to_path_buf());
        Ok(())
    }
}
fn tree(branch: Option<&str>, primary: bool, dirty: bool, pushed: bool) -> Tree {
    Tree {
        path: PathBuf::from(format!("/repo/{}", branch.unwrap_or("detached"))),
        branch: branch.map(Into::into),
        head: "a".repeat(40),
        primary,
        dirty,
        unknown: false,
        live_writer: false,
        upstream: branch.filter(|_| pushed).map(|_| "a".repeat(40)),
        pushed,
    }
}
#[test]
fn listed_refs_become_branch_names() {
    assert_eq!(short_branch("refs/heads/feat/x"), "feat/x");
    assert_eq!(short_branch("main"), "main");
}

fn delivery(dir: &Path, launch: bool) -> Delivery<'static> {
    Delivery {
        dir: operation_dir(dir, &OperationId::parse("handoff-1").unwrap()),
        project: Path::new("/Users/me/Repos/project"),
        state: Path::new("/Users/me/.local/share/provenant/v1"),
        account_label: "work",
        account: PathBuf::from("/Users/me/.claude-work"),
        model: Some("opus"),
        prompt: "# Session hand-off\n\nBriefing.",
        source_label: "laptop".into(),
        worktrees: vec![HandoffWorktree {
            branch: "feat/x".into(),
            path: Some("/Users/me/Repos/project/.worktrees/feat-x".into()),
            head: Some("b".repeat(40)),
            created: true,
            error: None,
        }],
        attachments: vec![("notes.md".into(), b"hello".to_vec())],
        launch,
    }
}

#[test]
fn problems_name_what_blocks_a_clean_hand_off() {
    assert!(
        tree(Some("feat/x"), false, false, true)
            .problems()
            .is_empty()
    );
    assert_eq!(
        tree(Some("feat/x"), false, true, true).problems(),
        ["uncommitted changes"]
    );
    assert_eq!(
        tree(Some("feat/x"), false, false, false).problems(),
        ["no origin branch"]
    );
    let mut ahead = tree(Some("feat/x"), false, false, false);
    ahead.upstream = Some("c".repeat(40));
    assert_eq!(ahead.problems(), ["commits not on origin"]);
    assert!(
        tree(None, false, false, false)
            .problems()
            .contains(&"detached HEAD")
    );
}

#[test]
fn generated_prompt_carries_checkpoint_branches_lanes_and_notes() {
    let trees = [
        tree(Some("main"), true, false, true),
        tree(Some("feat/x"), false, true, true),
    ];
    let lanes = Ok(vec!["run/task/attempt".to_owned()]);
    let notes = ["Review PR 12 next".to_owned()];
    let input = PromptInput {
        source: HostId::from_bytes([7; 16]),
        agent: "chair",
        session: Some("explicit-session"),
        body: None,
        checkpoint: Some("Goal: ship it."),
        trees: &trees,
        lanes: &lanes,
        notes: &notes,
    };
    let prompt = build_prompt(&input);
    for expected in [
        "explicit-session",
        "Goal: ship it.",
        "`main` (primary checkout)",
        "`feat/x` at `aaaa",
        "uncommitted changes on the source host",
        "- `run/task/attempt`",
        "- Review PR 12 next",
    ] {
        assert!(
            prompt.contains(expected),
            "{expected} missing from\n{prompt}"
        );
    }
    let replaced = build_prompt(&PromptInput {
        body: Some("--allowedTools Bash\nInvestigate the flaky test."),
        ..input
    });
    assert!(replaced.starts_with("# Session hand-off\n\n--allowedTools Bash\n"));
    assert!(replaced.contains("- Review PR 12 next"));
    assert!(!replaced.contains("## Branches"));
}

#[test]
fn delivery_stores_the_prompt_and_launches_once_through_the_seam() {
    let temp = tempfile::tempdir().unwrap();
    let recorder = Recorder::default();
    let first = delivery(temp.path(), true);
    assert!(first.dir.ends_with("op-handoff-1"));
    assert!(reserve(&first.dir).unwrap());
    let receipt = deliver(first, &recorder);
    let HandoffReceipt::Delivered {
        prompt_path,
        attachments,
        script_path,
        launched,
        launch_error,
        ..
    } = &receipt
    else {
        panic!("expected a delivery: {receipt:?}");
    };
    assert!(*launched && launch_error.is_none());
    let script_path = PathBuf::from(script_path.as_deref().unwrap());
    assert_eq!(*recorder.0.borrow(), std::slice::from_ref(&script_path));
    let script = fs::read_to_string(&script_path).unwrap();
    assert!(script.contains("export CLAUDE_CONFIG_DIR='/Users/me/.claude-work'"));
    assert!(script.contains(&format!(
        "exec claude --model 'opus' -- \"$(cat '{prompt_path}')\""
    )));
    let prompt = fs::read_to_string(prompt_path).unwrap();
    for expected in [
        "Briefing.",
        "## On this host",
        "`feat/x`: `/Users/me/Repos/project/.worktrees/feat-x` (created from origin)",
        "provenant lanes --host laptop",
    ] {
        assert!(
            prompt.contains(expected),
            "{expected} missing from\n{prompt}"
        );
    }
    assert_eq!(fs::read(&attachments[0]).unwrap(), b"hello");

    // A replay finds the directory taken, so it never launches again.
    assert!(!reserve(&delivery(temp.path(), true).dir).unwrap());
    assert_eq!(recorder.0.borrow().len(), 1);
}

#[test]
fn a_prompt_that_looks_like_an_option_stays_a_prompt() {
    let temp = tempfile::tempdir().unwrap();
    let recorder = Recorder::default();
    let mut hostile = delivery(temp.path(), true);
    hostile.prompt = "--allowedTools Bash --dangerously-skip-permissions";
    reserve(&hostile.dir).unwrap();
    let HandoffReceipt::Delivered {
        prompt_path,
        script_path,
        ..
    } = deliver(hostile, &recorder)
    else {
        panic!("expected a delivery");
    };
    let prompt = fs::read_to_string(&prompt_path).unwrap();
    assert!(prompt.starts_with("# Session hand-off\n\n--allowedTools"));
    let script = fs::read_to_string(script_path.unwrap()).unwrap();
    assert!(script.contains(&format!("-- \"$(cat '{prompt_path}')\"")));
}

#[test]
fn a_reused_worktree_warning_reaches_the_stored_prompt() {
    let temp = tempfile::tempdir().unwrap();
    let mut stale = delivery(temp.path(), false);
    stale.worktrees[0].created = false;
    stale.worktrees[0].error = Some("local branch differs from origin".into());
    reserve(&stale.dir).unwrap();
    let HandoffReceipt::Delivered { prompt_path, .. } = deliver(stale, &Recorder::default()) else {
        panic!("expected a delivery");
    };
    let prompt = fs::read_to_string(prompt_path).unwrap();
    assert!(
        prompt.contains("(warning: local branch differs from origin"),
        "{prompt}"
    );
}

#[test]
fn a_large_checkpoint_is_cut_to_fit_and_a_large_prompt_file_is_refused() {
    let trees = [tree(Some("feat/x"), false, false, true)];
    let lanes = Ok(Vec::new());
    let big = "checkpoint line\n".repeat(PROMPT_LIMIT / 8);
    let input = PromptInput {
        source: HostId::from_bytes([7; 16]),
        agent: "chair",
        session: Some("explicit-session"),
        body: None,
        checkpoint: Some(&big),
        trees: &trees,
        lanes: &lanes,
        notes: &[],
    };
    let prompt = compose(&input).unwrap();
    assert!(prompt.len() <= PROMPT_LIMIT);
    assert!(prompt.contains("[Checkpoint cut to fit the hand-off."));
    assert!(prompt.contains("## Branches"));
    let body = "x".repeat(PROMPT_LIMIT);
    assert!(
        compose(&PromptInput {
            body: Some(&body),
            ..input
        })
        .is_err()
    );
}

#[test]
fn no_launch_writes_the_script_but_never_launches() {
    let temp = tempfile::tempdir().unwrap();
    let recorder = Recorder::default();
    let quiet = delivery(temp.path(), false);
    reserve(&quiet.dir).unwrap();
    let receipt = deliver(quiet, &recorder);
    assert!(matches!(
        receipt,
        HandoffReceipt::Delivered {
            launched: false,
            script_path: Some(_),
            launch_error: None,
            ..
        }
    ));
    assert!(recorder.0.borrow().is_empty());
}

#[test]
fn script_quotes_paths_and_refuses_control_characters() {
    let text = script(
        Path::new("/Users/me/it's"),
        Path::new("/Users/me/.claude-work"),
        Path::new("/state"),
        None,
        "laptop",
        Path::new("/state/prompt.md"),
    )
    .unwrap();
    let notice = text.find("printf ").unwrap();
    assert!(text.contains("'Provenant session hand-off from laptop.'"));
    assert!(text.contains("'Prompt: /state/prompt.md'"));
    assert!(notice < text.find("exec claude").unwrap());
    assert!(text.contains("cd '/Users/me/it'\\''s' || exit 1"));
    assert!(text.contains("exec claude -- \"$(cat '/state/prompt.md')\""));
    assert!(
        script(
            Path::new("/a\nb"),
            Path::new("/c"),
            Path::new("/d"),
            None,
            "laptop",
            Path::new("/e")
        )
        .is_err()
    );
}

#[test]
fn receive_refuses_until_the_host_opts_in() {
    let Some(HandoffReceipt::Refused { code, message, .. }) = gate(false) else {
        panic!("a host that has not opted in must refuse");
    };
    assert_eq!(code, "HANDOFF_DISABLED");
    assert!(message.contains("fabric.accept_handoff"));
    assert!(gate(true).is_none());
}

fn delivered(branch: &str, path: &str) -> HandoffReceipt {
    HandoffReceipt::Delivered {
        prompt_path: "/state/prompt.md".into(),
        project: "/repo".into(),
        worktrees: vec![HandoffWorktree {
            branch: branch.into(),
            path: Some(path.into()),
            head: None,
            created: true,
            error: Some("warn\x1b[2J".into()),
        }],
        attachments: Vec::new(),
        script_path: Some("/state/launch.command\x07".into()),
        launched: false,
        launch_error: Some("x".repeat(5000)),
    }
}

#[test]
fn receipt_text_is_plain_and_bounded() {
    let requested = vec!["feat/x".to_owned()];
    let HandoffReceipt::Delivered {
        worktrees,
        script_path,
        launch_error,
        ..
    } = vet_receipt(delivered("feat/x", "/w\x1b]0;owned\x07"), &requested).unwrap()
    else {
        panic!("expected a delivery");
    };
    assert_eq!(worktrees[0].path.as_deref(), Some("/w]0;owned"));
    assert_eq!(worktrees[0].error.as_deref(), Some("warn[2J"));
    assert_eq!(script_path.as_deref(), Some("/state/launch.command"));
    assert_eq!(launch_error.unwrap().chars().count(), RECEIPT_TEXT);
    let HandoffReceipt::Refused { message, .. } = vet_receipt(
        HandoffReceipt::Refused {
            code: "c\x1b".into(),
            message: "m\x1b[31m".into(),
            accounts: vec!["a\n".into()],
        },
        &requested,
    )
    .unwrap() else {
        panic!("expected a refusal");
    };
    assert_eq!(message, "m[31m");
}

#[test]
fn a_receipt_with_an_unrequested_branch_is_refused() {
    let requested = vec!["feat/x".to_owned()];
    let mismatch = vet_receipt(delivered("evil\x1b[2J", "/w"), &requested).unwrap_err();
    assert!(!mismatch.to_string().contains('\x1b'));
    assert!(vet_receipt(delivered("feat/x", "/w"), &[]).is_err());
}

#[test]
fn admission_refuses_before_it_touches_the_disk_or_the_accounts() {
    let temp = tempfile::tempdir().unwrap();
    let lease = temp.path().join("handoff/project");
    let input = SessionHandoff {
        operation_id: OperationId::parse("handoff-1").unwrap(),
        account: "work".into(),
        model: None,
        prompt: "# Session hand-off\n\nBriefing.".into(),
        branches: vec!["feat/x".into()],
        attachments: Vec::new(),
        launch: true,
    };
    let refusal = *admit(false, &input, &lease, |_| {
        panic!("accounts must not be read while hand-off is off")
    })
    .unwrap_err();
    assert!(matches!(
        refusal,
        HandoffReceipt::Refused { ref code, .. } if code == "HANDOFF_DISABLED"
    ));
    assert!(!lease.exists());
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    // Opted in, an unregistered account still stops before the directory.
    let refusal = *admit(true, &input, &lease, |label| {
        assert_eq!(label, "work");
        Ok(None)
    })
    .unwrap_err();
    assert!(matches!(
        refusal,
        HandoffReceipt::Refused { ref code, ref accounts, .. }
            if code == "account_unregistered" && accounts.is_empty()
    ));
    assert!(!lease.exists());
}

#[test]
fn the_peer_takes_the_lease_only_for_a_launch_or_no_launch() {
    let mut receipt = delivered("feat/x", "/w");
    assert!(!takes_lease(&receipt, true), "launch failed");
    assert!(takes_lease(&receipt, false), "--no-launch");
    if let HandoffReceipt::Delivered { launched, .. } = &mut receipt {
        *launched = true;
    }
    assert!(takes_lease(&receipt, true));
    assert!(!takes_lease(&refused("io", "x"), false));
}

#[test]
fn refusal_codes_pass_through_as_plain_upper_case() {
    assert_eq!(refusal_code("HANDOFF_DISABLED"), "HANDOFF_DISABLED");
    assert_eq!(refusal_code("account_unregistered"), "ACCOUNT_UNREGISTERED");
    assert_eq!(refusal_code("\x1b[2J 1"), "J");
    assert_eq!(refusal_code("123"), "HANDOFF_REFUSED");
}

#[test]
fn the_peer_works_to_less_than_the_source_waits() {
    assert_eq!(peer_seconds(Some(30), 30), 25);
    assert_eq!(peer_seconds(Some(10), 30), 5);
    assert_eq!(peer_seconds(Some(30), 12), 7);
    assert_eq!(peer_seconds(None, 30), 25);
    assert_eq!(peer_seconds(Some(3), 30), 1);
}

#[test]
fn owner_lease_flips_and_reads_back() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("handoff/project");
    assert!(read_owner(&dir).is_none());
    let mut owner = Owner {
        schema_version: 1,
        host: HostId::from_bytes([2; 16]),
        previous: Some(HostId::from_bytes([1; 16])),
        operation_id: OperationId::parse("handoff-1").unwrap(),
        session_id: Some("explicit-session".into()),
        at_unix: 1,
    };
    write_owner(&dir, &owner).unwrap();
    assert_eq!(read_owner(&dir).unwrap(), owner);
    owner.host = HostId::from_bytes([1; 16]);
    owner.previous = Some(HostId::from_bytes([2; 16]));
    write_owner(&dir, &owner).unwrap();
    assert_eq!(read_owner(&dir).unwrap(), owner);
}
