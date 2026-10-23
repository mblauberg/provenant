//! End-to-end behaviour of the built binary against fixture checkouts.

mod common;

use common::*;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::Stdio;

fn real(path: &Path) -> String {
    fs::canonicalize(path).unwrap().display().to_string()
}

#[test]
fn installed_stub_resolves_the_product_root_and_rewrites_the_environment() {
    let sandbox = Sandbox::new("stub");
    let checkout = sandbox.checkout("checkout");
    let stub = sandbox.install_binary("bin/provenant");
    let output = command(&stub, &sandbox.home())
        .env("AGENT_FABRIC_PRODUCT_ROOT", &checkout)
        .env("AGENTS_HOME", "/somewhere/else")
        .env("AGENT_FABRIC_INSTANCE_ROOT", sandbox.dir.join("instance"))
        .args(["lanes", "--json", "", "a b", "--x"])
        .output()
        .unwrap();
    let out = stdout(&output);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        field(&out, "argv0"),
        format!("{}/runtime/fabric/bin/fabric", real(&checkout))
    );
    assert_eq!(args(&out), ["lanes", "--json", "", "a b", "--x"]);
    assert_eq!(field(&out, "root"), real(&checkout));
    assert_eq!(field(&out, "home"), real(&checkout));
    assert_eq!(
        field(&out, "instance"),
        sandbox.dir.join("instance").display().to_string()
    );
}

#[test]
fn the_instance_root_is_never_invented() {
    let sandbox = Sandbox::new("instance");
    let checkout = sandbox.checkout("checkout");
    let stub = sandbox.install_binary("bin/provenant");
    let output = command(&stub, &sandbox.home())
        .env("AGENT_FABRIC_PRODUCT_ROOT", &checkout)
        .arg("gate")
        .output()
        .unwrap();
    assert_eq!(field(&stdout(&output), "instance"), "<unset>");
    assert!(!sandbox.home().join(".agents").exists());
}

#[test]
fn stub_ignores_same_named_commands_beside_it() {
    let sandbox = Sandbox::new("beside");
    let checkout = sandbox.checkout("checkout");
    let stub = sandbox.install_binary("bin/provenant");
    write_executable(&sandbox.dir.join("bin/fabric"), "#!/bin/sh\necho wrong\n");
    let output = command(&stub, &sandbox.home())
        .env("AGENT_FABRIC_PRODUCT_ROOT", &checkout)
        .args(["fabric", "x"])
        .output()
        .unwrap();
    assert!(field(&stdout(&output), "argv0").starts_with(&real(&checkout)));
}

#[test]
fn checkout_binary_prefers_its_own_tree_and_leaves_the_environment_alone() {
    let sandbox = Sandbox::new("local");
    let checkout = sandbox.checkout("checkout");
    let other = sandbox.checkout("other");
    let binary = sandbox.install_binary("checkout/bin/provenant");
    let output = command(&binary, &sandbox.home())
        .env("AGENT_FABRIC_PRODUCT_ROOT", &other)
        .env("AGENTS_HOME", "/x")
        .args(["worktree", "list"])
        .output()
        .unwrap();
    let out = stdout(&output);
    assert_eq!(
        field(&out, "argv0"),
        format!("{}/scripts/worktree", real(&checkout))
    );
    assert_eq!(field(&out, "root"), other.display().to_string());
    assert_eq!(field(&out, "home"), "/x");
    let bare = command(&binary, &sandbox.home())
        .arg("gate")
        .output()
        .unwrap();
    assert_eq!(field(&stdout(&bare), "root"), "<unset>");
}

#[test]
fn a_symlinked_binary_resolves_to_its_real_tree() {
    let sandbox = Sandbox::new("symlinked");
    let checkout = sandbox.checkout("checkout");
    sandbox.install_binary("checkout/bin/provenant");
    fs::create_dir_all(sandbox.dir.join("elsewhere")).unwrap();
    let link = sandbox.dir.join("elsewhere/provenant");
    symlink(checkout.join("bin/provenant"), &link).unwrap();
    let output = command(&link, &sandbox.home())
        .arg("gate")
        .output()
        .unwrap();
    assert_eq!(
        field(&stdout(&output), "argv0"),
        format!("{}/scripts/gate", real(&checkout))
    );
}

#[test]
fn a_checkout_without_the_launcher_falls_through_to_resolution() {
    let sandbox = Sandbox::new("nolauncher");
    let tree = sandbox.checkout("tree");
    fs::remove_file(tree.join("runtime/fabric/bin/fabric-mcp")).unwrap();
    let product = sandbox.checkout("product");
    let binary = sandbox.install_binary("tree/bin/provenant");
    let output = command(&binary, &sandbox.home())
        .env("AGENT_FABRIC_PRODUCT_ROOT", &product)
        .arg("gate")
        .output()
        .unwrap();
    let out = stdout(&output);
    assert_eq!(
        field(&out, "argv0"),
        format!("{}/scripts/gate", real(&product))
    );
    assert_eq!(field(&out, "root"), real(&product));
}

#[test]
fn help_and_usage_need_no_product_root() {
    let sandbox = Sandbox::new("help");
    let stub = sandbox.install_binary("bin/provenant");
    for flag in ["help", "-h", "--help"] {
        let output = command(&stub, &sandbox.home()).arg(flag).output().unwrap();
        assert_eq!(output.status.code(), Some(0));
        let text = stdout(&output);
        assert!(text.contains("  lanes|events   runtime/fabric/bin/fabric lanes|events\n"));
        assert!(text.contains("  lint ") && text.contains("  lease "));
        assert!(output.stderr.is_empty());
    }
    let output = command(&stub, &sandbox.home())
        .arg("bogus")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(stderr(&output).starts_with("usage: provenant <help|root|route|"));
    let output = command(&stub, &sandbox.home()).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let output = command(&stub, &sandbox.home())
        .args(["help", "routes"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(3),
        "help routes executes, so it needs a root"
    );
    let output = command(&stub, &sandbox.home())
        .args(["-h", "routes"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn help_routes_runs_the_route_owner() {
    let sandbox = Sandbox::new("helproutes");
    let checkout = sandbox.checkout("checkout");
    let stub = sandbox.install_binary("bin/provenant");
    let output = command(&stub, &sandbox.home())
        .env("AGENT_FABRIC_PRODUCT_ROOT", &checkout)
        .args(["help", "routes"])
        .output()
        .unwrap();
    assert_eq!(args(&stdout(&output)), ["routes", "--health"]);
}

#[test]
fn resolution_failures_exit_3_with_one_clean_line() {
    let sandbox = Sandbox::new("unresolvable");
    let stub = sandbox.install_binary("bin/provenant");
    for words in [&["root"][..], &["gate"], &["route"]] {
        let output = command(&stub, &sandbox.home())
            .args(words)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(3), "{words:?}");
        assert!(output.stdout.is_empty());
        let err = stderr(&output);
        assert_eq!(err.lines().count(), 1, "{err}");
        assert!(
            err.starts_with("provenant: Fabric MCP launcher is missing or not executable at "),
            "{err}"
        );
        assert!(err.contains("repair: re-run install-harness from the product checkout, or set AGENT_FABRIC_PRODUCT_ROOT"));
        assert!(!err.contains("panicked"));
    }
}

#[test]
fn root_prints_the_resolved_path_and_refuses_extra_arguments() {
    let sandbox = Sandbox::new("root");
    let checkout = sandbox.checkout("checkout");
    symlink(&checkout, sandbox.dir.join("link")).unwrap();
    let stub = sandbox.install_binary("bin/provenant");
    let output = command(&stub, &sandbox.home())
        .env(
            "AGENT_FABRIC_PRODUCT_ROOT",
            format!("{}/link/", sandbox.dir.display()),
        )
        .arg("root")
        .output()
        .unwrap();
    assert_eq!(stdout(&output), format!("{}\n", real(&checkout)));
    let extra = command(&stub, &sandbox.home())
        .args(["root", "x"])
        .output()
        .unwrap();
    assert_eq!(extra.status.code(), Some(2));
    assert!(extra.stdout.is_empty());
}

#[test]
fn root_ignores_a_local_checkout_and_the_default_is_under_home() {
    let sandbox = Sandbox::new("rootdefault");
    let local = sandbox.checkout("local");
    let home_checkout = sandbox.checkout("home/.agents");
    let binary = sandbox.install_binary("local/bin/provenant");
    let output = command(&binary, &sandbox.home())
        .arg("root")
        .output()
        .unwrap();
    assert_eq!(stdout(&output), format!("{}\n", real(&home_checkout)));
    assert_ne!(real(&local), real(&home_checkout));
}

#[test]
fn a_stale_pointer_fails_instead_of_falling_back() {
    let sandbox = Sandbox::new("pointer");
    let fallback = sandbox.checkout("fallback");
    let stub = sandbox.install_binary("bin/provenant");
    let instance = sandbox.dir.join("instance");
    fs::create_dir_all(instance.join(".agent-fabric")).unwrap();
    fs::write(
        instance.join(".agent-fabric/product-root.json"),
        r#"{"schema_version":1,"product_root":"/nope/missing"}"#,
    )
    .unwrap();
    let output = command(&stub, &sandbox.home())
        .env("AGENT_FABRIC_INSTANCE_ROOT", &instance)
        .env("AGENTS_HOME", &fallback)
        .arg("root")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert!(stderr(&output).contains("product-root pointer is invalid or stale at "));
}

#[test]
fn empty_variables_count_as_unset() {
    let sandbox = Sandbox::new("empty");
    let default = sandbox.checkout("home/.agents");
    let stub = sandbox.install_binary("bin/provenant");
    let output = command(&stub, &sandbox.home())
        .env("AGENT_FABRIC_PRODUCT_ROOT", "")
        .env("AGENTS_HOME", "")
        .env("AGENT_FABRIC_INSTANCE_ROOT", "")
        .arg("root")
        .output()
        .unwrap();
    assert_eq!(stdout(&output), format!("{}\n", real(&default)));
}

#[test]
fn owner_status_cwd_stdin_and_pid_survive_the_exec() {
    let sandbox = Sandbox::new("exec");
    let checkout = sandbox.checkout("checkout");
    let stub = sandbox.install_binary("bin/provenant");
    let work = sandbox.dir.join("work");
    fs::create_dir_all(&work).unwrap();
    symlink(&work, sandbox.dir.join("worklink")).unwrap();
    for status in ["0", "1", "2", "3", "7", "127"] {
        let output = command(&stub, &sandbox.home())
            .current_dir(&work)
            .env("AGENT_FABRIC_PRODUCT_ROOT", &checkout)
            .env("PROVENANT_TEST_EXIT", status)
            .arg("gate")
            .output()
            .unwrap();
        assert_eq!(
            output.status.code().map(|c| c.to_string()).as_deref(),
            Some(status)
        );
        assert_eq!(field(&stdout(&output), "cwd"), real(&work));
    }
    let child = command(&stub, &sandbox.home())
        .current_dir(sandbox.dir.join("worklink"))
        .env("AGENT_FABRIC_PRODUCT_ROOT", &checkout)
        .arg("gate")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id().to_string();
    let output = child.wait_with_output().unwrap();
    assert_eq!(field(&stdout(&output), "pid"), pid, "exec keeps the PID");
}

#[test]
fn stdin_reaches_the_owner_unread() {
    let sandbox = Sandbox::new("stdin");
    let checkout = sandbox.checkout("checkout");
    write_executable(&checkout.join("scripts/fabric-hosts"), "#!/bin/sh\ncat\n");
    let stub = sandbox.install_binary("bin/provenant");
    let payload: Vec<u8> = (0..=255u8).cycle().take(300_000).collect();
    let mut child = command(&stub, &sandbox.home())
        .env("AGENT_FABRIC_PRODUCT_ROOT", &checkout)
        .arg("peer")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn({
        let payload = payload.clone();
        move || std::io::Write::write_all(&mut stdin, &payload).unwrap()
    });
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap();
    assert_eq!(output.stdout, payload);
}

#[test]
fn non_utf8_arguments_and_environment_arrive_byte_for_byte() {
    let sandbox = Sandbox::new("bytes");
    let checkout = sandbox.checkout("checkout");
    write_executable(
        &checkout.join("scripts/gate"),
        "#!/bin/sh\nprintf '%s|%s' \"$1\" \"$ODD\"\n",
    );
    let stub = sandbox.install_binary("bin/provenant");
    let odd = std::ffi::OsStr::from_bytes(b"a\xffb\n");
    let output = command(&stub, &sandbox.home())
        .env("AGENT_FABRIC_PRODUCT_ROOT", &checkout)
        .env("ODD", odd)
        .arg("gate")
        .arg(odd)
        .output()
        .unwrap();
    assert_eq!(output.stdout, b"a\xffb\n|a\xffb\n");
}

#[test]
fn exec_failures_exit_3_without_a_panic() {
    let sandbox = Sandbox::new("execfail");
    let checkout = sandbox.checkout("checkout");
    let stub = sandbox.install_binary("bin/provenant");
    fs::set_permissions(
        checkout.join("scripts/gate"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    fs::remove_file(checkout.join("scripts/worktree")).unwrap();
    write_executable(
        &checkout.join("scripts/fabric-hosts"),
        "#!/no/such/interpreter\n",
    );
    fs::remove_file(checkout.join("scripts/clean.py")).unwrap();
    fs::create_dir(checkout.join("scripts/clean.py")).unwrap();
    for (verb, why) in [
        ("gate", "Permission denied"),
        ("worktree", "No such file or directory"),
        ("peer", "No such file or directory"),
        ("clean", "Permission denied"),
    ] {
        let output = command(&stub, &sandbox.home())
            .env("AGENT_FABRIC_PRODUCT_ROOT", &checkout)
            .arg(verb)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(3), "{verb}");
        let err = stderr(&output);
        assert!(err.starts_with("provenant: cannot execute "), "{err}");
        assert!(err.contains(&format!(": {why}; repair: ")), "{err}");
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn seat_with_no_arguments_launches_the_mcp_server_directly() {
    let sandbox = Sandbox::new("seat");
    let checkout = sandbox.checkout("checkout");
    let stub = sandbox.install_binary("bin/provenant");
    for seat in ["chair", ""] {
        let output = command(&stub, &sandbox.home())
            .env("AGENT_FABRIC_PRODUCT_ROOT", &checkout)
            .env("AGENT_FABRIC_SEAT", seat)
            .output()
            .unwrap();
        let out = stdout(&output);
        assert_eq!(
            field(&out, "argv0"),
            format!("{}/runtime/fabric/bin/fabric-mcp", real(&checkout))
        );
        assert!(args(&out).is_empty());
        assert_eq!(field(&out, "root"), real(&checkout));
    }
    let with_args = command(&stub, &sandbox.home())
        .env("AGENT_FABRIC_PRODUCT_ROOT", &checkout)
        .env("AGENT_FABRIC_SEAT", "chair")
        .arg("bogus")
        .output()
        .unwrap();
    assert_eq!(with_args.status.code(), Some(2));
}

#[test]
fn lease_maps_to_landing_verbs_and_passes_exit_3_through() {
    let sandbox = Sandbox::new("lease");
    let checkout = sandbox.checkout("checkout");
    let stub = sandbox.install_binary("bin/provenant");
    let output = command(&stub, &sandbox.home())
        .env("AGENT_FABRIC_PRODUCT_ROOT", &checkout)
        .env("PROVENANT_TEST_EXIT", "3")
        .args(["lease", "acquire", "s1", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(args(&stdout(&output)), ["landing-acquire", "s1", "--json"]);
    for words in [
        &["lease"][..],
        &["lease", "wait"],
        &["lease", "--json"],
        &["lease", ""],
    ] {
        let output = command(&stub, &sandbox.home())
            .env("AGENT_FABRIC_PRODUCT_ROOT", &checkout)
            .args(words)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_eq!(
            stderr(&output),
            "usage: provenant lease <acquire|release|verify|status|push> ...\n"
        );
    }
    let output = command(&stub, &sandbox.home())
        .env("AGENT_FABRIC_PRODUCT_ROOT", &checkout)
        .args(["lease", "--help", "acquire"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout(&output).contains("Exit 0 ok, 1 refused or stale"));
}

#[test]
fn only_check_scrubs_git_redirects() {
    let sandbox = Sandbox::new("git");
    let checkout = sandbox.checkout("checkout");
    write_executable(
        &checkout.join("scripts/check_target.py"),
        &format!(
            "#!/usr/bin/env python3\nimport sys\nsys.stdout.buffer.write(b'{0}/scripts/check-harness\\0{0}\\0')\n",
            checkout.display()
        ),
    );
    let stub = sandbox.install_binary("bin/provenant");
    let run = |verb: &str| {
        let output = command(&stub, &sandbox.home())
            .env("AGENT_FABRIC_PRODUCT_ROOT", &checkout)
            .env("GIT_DIR", "/elsewhere/.git")
            .env("GIT_AUTHOR_NAME", "kept")
            .args([verb, "--flag"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
        stdout(&output)
    };
    let lint = run("lint");
    assert_eq!(field(&lint, "gitdir"), "/elsewhere/.git");
    assert_eq!(field(&lint, "stale"), "<unset>");
    let check = run("check");
    assert_eq!(field(&check, "gitdir"), "<unset>");
    assert_eq!(field(&check, "gitname"), "kept");
    assert_eq!(field(&check, "stale"), "1");
    assert_eq!(field(&check, "root"), real(&checkout));
    assert_eq!(args(&check), ["--flag"]);
}

#[test]
fn check_helper_refusal_exits_3_and_lets_stderr_through() {
    let sandbox = Sandbox::new("refuse");
    let checkout = sandbox.checkout("checkout");
    write_executable(
        &checkout.join("scripts/check_target.py"),
        "#!/usr/bin/env python3\nimport sys\nprint('provenant: copied worktree refused', file=sys.stderr)\nsys.exit(3)\n",
    );
    let stub = sandbox.install_binary("bin/provenant");
    let output = command(&stub, &sandbox.home())
        .env("AGENT_FABRIC_PRODUCT_ROOT", &checkout)
        .arg("check")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(stderr(&output), "provenant: copied worktree refused\n");
    assert!(output.stdout.is_empty());
}

#[test]
fn lint_and_lease_remain_available_without_owner_markers() {
    let sandbox = Sandbox::new("missing-owners");
    let checkout = sandbox.checkout("checkout");
    fs::remove_file(checkout.join("scripts/provenant_lint.py")).unwrap();
    fs::remove_file(checkout.join("runtime/fabric/src/landing-cli.ts")).unwrap();
    let stub = sandbox.install_binary("bin/provenant");
    let run = |words: &[&str]| {
        command(&stub, &sandbox.home())
            .env("AGENT_FABRIC_PRODUCT_ROOT", &checkout)
            .args(words)
            .output()
            .unwrap()
    };
    for topic in ["lint", "lease"] {
        let output = run(&["help", topic]);
        assert_eq!(output.status.code(), Some(0));
        assert!(stdout(&output).starts_with(&format!("usage: provenant {topic} ")));
        assert!(output.stderr.is_empty());
    }
    // A missing lint owner is an exec failure, not an unknown command.
    let lint = run(&["lint"]);
    assert_eq!(lint.status.code(), Some(3));
    assert!(stderr(&lint).contains("provenant_lint.py"));
    // Lease routes to Fabric even when its TypeScript marker is absent.
    let lease = run(&["lease", "status"]);
    assert_eq!(lease.status.code(), Some(0));
    assert_eq!(args(&stdout(&lease)), ["landing-status"]);
    let help = stdout(&run(&["help"]));
    assert!(help.contains("  lint ") && help.contains("  lease "));
    assert!(help.contains("  lanes|events   runtime/fabric/bin/fabric lanes|events\n"));
}

#[test]
fn a_shebangless_owner_is_not_run_through_a_shell() {
    let sandbox = Sandbox::new("enoexec");
    let checkout = sandbox.checkout("checkout");
    write_executable(
        &checkout.join("scripts/model-route"),
        "echo ran-through-a-shell\n",
    );
    let stub = sandbox.install_binary("bin/provenant");
    let output = command(&stub, &sandbox.home())
        .env("AGENT_FABRIC_PRODUCT_ROOT", &checkout)
        .arg("route")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty(), "{}", stdout(&output));
    assert!(stderr(&output).starts_with("provenant: cannot execute "));
    assert!(stderr(&output).contains("Exec format error"));
}

#[test]
fn the_binary_carries_the_installer_marker_and_digest_lines() {
    let bytes = fs::read(common::BINARY).unwrap();
    let marker = b"\n# Managed by scripts/install-harness; local edits to an installed copy are not preserved.\nprovenant-front-door source-sha256=";
    assert!(bytes.windows(marker.len()).any(|window| window == marker));
}
