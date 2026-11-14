
#[test]
fn identity_probes_preserve_auth_failures_only_from_intact_captures() {
    use provenant_os::exec::{
        CapturedStream, ExecError, ExecOptions, ExecOutcome, Executor, Termination,
    };
    struct SignedOut<'a>(&'a [u8], bool, bool, bool, bool);
    impl Executor for SignedOut<'_> {
        fn execute(&self, options: ExecOptions<'_>) -> Result<ExecOutcome, ExecError> {
            assert!(matches!(
                options.stderr,
                provenant_os::exec::StreamPolicy::Capture { .. }
            ));
            Ok(ExecOutcome {
                termination: Termination::Exit(1),
                stdout: CapturedStream {
                    bytes: if self.4 { self.0.to_vec() } else { vec![] },
                    truncated: self.1 && self.4,
                    ..Default::default()
                },
                stderr: CapturedStream {
                    bytes: if self.4 { vec![] } else { self.0.to_vec() },
                    truncated: self.1 && !self.4,
                    ..Default::default()
                },
                timed_out: self.2,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: self.3,
                elapsed_micros: 0,
            })
        }
    }
    let scratch = tempfile::tempdir().unwrap();
    for (adapter, capture, structured) in [
        (
            "agy",
            include_bytes!("../../../tests/fixtures/providers/agy/auth-required.stderr").as_slice(),
            false,
        ),
        ("claude", b"Not logged in".as_slice(), false),
        ("claude", br#"{"loggedIn":false}"#.as_slice(), true),
    ] {
        for (truncated, timed_out, cancelled) in [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            let result = accounts::detect(
                &SignedOut(capture, truncated, timed_out, cancelled, structured),
                adapter,
                Path::new("/fixture/agy"),
                scratch.path(),
                OsStr::new("fixture"),
                scratch.path(),
                scratch.path(),
            );
            if truncated || timed_out || cancelled || (adapter == "claude" && !structured) {
                assert!(matches!(result, Err(ProviderError::UnknownIdentity)));
            } else if adapter == "claude" {
                assert!(matches!(result, Err(ProviderError::LoginRequired)));
            } else {
                assert!(matches!(result, Err(ProviderError::CredentialUnavailable)));
            }
        }
    }
}

#[test]
fn user_skills_grants_resolve_linked_roots_resources_and_cycles()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().canonicalize()?;
    let root = home.join("installed");
    let target = home.join("releases/digest/skill");
    let resource = home.join("shared/reference.md");
    std::fs::create_dir_all(&root)?;
    std::fs::create_dir_all(&target)?;
    std::fs::create_dir_all(resource.parent().unwrap())?;
    std::fs::write(&resource, "reference")?;
    std::os::unix::fs::symlink(&root, home.join("skills"))?;
    std::os::unix::fs::symlink(&target, root.join("skill"))?;
    let deep = target.join("a/b/c/d/e/f/g");
    std::fs::create_dir_all(&deep)?;
    std::os::unix::fs::symlink(&resource, deep.join("reference.md"))?;
    std::os::unix::fs::symlink(&root, target.join("cycle"))?;
    std::os::unix::fs::symlink(home.join("absent"), root.join("broken"))?;
    let staged =
        crate::skill_stage::capture(&home.join("skills"), &home.join("staged"), &home, 4096);
    assert_eq!(staged.file_count, 1);
    assert!(
        staged
            .warnings
            .iter()
            .any(|warning| warning.reason == "cycle")
    );
    assert!(
        staged.setup().iter().any(
            |step| matches!(step, SetupStep::WriteFile { bytes, .. } if bytes == b"reference")
        )
    );
    assert!(
        crate::skill_stage::capture(&home.join("absent"), &home.join("staged"), &home, 4096)
            .root
            .is_none()
    );
    Ok(())
}

#[test]
fn optional_discovered_grants_are_filtered_without_touching_required_grants()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().canonicalize()?;
    let secret = home.join(".gemini/private");
    let optional = home.join("optional");
    std::fs::create_dir_all(&secret)?;
    std::os::unix::fs::symlink(&secret, &optional)?;
    let required = crate::launch::subtree(&secret);
    let mut grants = vec![
        required.clone(),
        crate::launch::subtree(&optional),
        crate::launch::subtree(&secret),
    ];
    let mut omitted = crate::launch::OmittedGrant::skills(vec![secret.clone()]);
    crate::launch::filter_optional_grants(
        &mut grants,
        1,
        &home,
        crate::launch::OmissionSource::Toolchain,
        &mut omitted,
    );
    assert_eq!(grants, vec![required]);
    assert_eq!(
        omitted,
        vec![
            crate::launch::OmittedGrant {
                path: secret.clone(),
                source: crate::launch::OmissionSource::UserSkill,
            },
            crate::launch::OmittedGrant {
                path: secret,
                source: crate::launch::OmissionSource::Toolchain,
            },
        ]
    );
    Ok(())
}

#[test]
fn optional_unrepresentable_discovery_is_omitted_with_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::ffi::OsStringExt;
    let temp = tempfile::tempdir()?;
    let home = temp.path().canonicalize()?;
    let target = home.join(std::ffi::OsString::from_vec(b"tool-\xff".to_vec()));
    let required = crate::launch::subtree(&target);
    let control = home.join("tool-\u{1}");
    let mut grants = vec![
        required.clone(),
        required.clone(),
        crate::launch::subtree(&control),
    ];
    let mut omitted = Vec::new();
    crate::launch::filter_optional_grants(
        &mut grants,
        1,
        &home,
        crate::OmissionSource::Toolchain,
        &mut omitted,
    );
    assert_eq!(grants, vec![required]);
    assert_eq!(omitted.len(), 2);
    assert_eq!(omitted[0].path, target);
    assert_eq!(omitted[1].path, control);
    let receipt = crate::AppliedAccess {
        libraries: vec![],
        staged_content: None,
        toolchain_roots: vec![],
        omitted_grants: omitted,
    }
    .receipt(&home);
    assert_eq!(
        receipt["omitted_grants"][1]["path"],
        "<unrepresentable path>"
    );
    Ok(())
}

#[test]
fn optional_unrepresentable_write_is_omitted_from_applied_access()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::ffi::OsStringExt;
    let temp = tempfile::tempdir()?;
    let home = temp.path().canonicalize()?;
    let target = home.join(std::ffi::OsString::from_vec(b"write-\xff".to_vec()));
    let plan = fixture_access_plan(Access::ReadOnly, Some("login"), false);
    let mut route = route("cursor", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture".into());
    let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
    input.home = &home;
    let mut spec = crate::cursor::Cursor.prepare(input)?;
    spec.tool_requirements = Default::default();
    spec.tool_requirements
        .writes
        .push(crate::launch::subtree(&target));
    spec.complete_tool_requirements(&plan, &home, Path::new("/fixture/repo"));
    assert!(
        spec.profile
            .spec
            .tool_writes
            .iter()
            .all(|grant| grant.path != target)
    );
    assert!(
        spec.omitted_discovery
            .iter()
            .any(|entry| entry.path == target)
    );
    let receipt = spec.applied_access().receipt(&home);
    assert!(
        receipt["omitted_grants"]
            .as_array()
            .ok_or("omissions")?
            .iter()
            .any(|entry| entry["reason"] == "unrepresentable_path")
    );
    Ok(())
}

#[cfg(not(target_os = "macos"))]
#[test]
fn unrepresentable_library_target_reports_incomplete_closure()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::ffi::OsStringExt;
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let home = root.join("home");
    let executable = root.join("package/bin/provider");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(executable.parent().ok_or("package")?)?;
    let library = root.join(std::ffi::OsString::from_vec(b"library-\xff.dylib".to_vec()));
    std::fs::write(&library, test_macho(&[]))?;
    let alias = root.join("library-link");
    std::os::unix::fs::symlink(&library, &alias)?;
    std::fs::write(&executable, test_macho(&[alias.to_str().ok_or("alias")?]))?;
    let plan = fixture_access_plan(Access::ReadOnly, Some("login"), false);
    let mut route = route("opencode", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture".into());
    let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
    input.executable = &executable;
    input.home = &home;
    let spec = crate::opencode::OpenCode.prepare(input)?;
    let tool = spec
        .prepared_tools()
        .iter()
        .find(|tool| tool.executable == executable)
        .ok_or("tool")?;
    assert_eq!(
        tool.dependency_closure.outcome,
        crate::ClosureOutcome::Incomplete
    );
    assert!(tool.dependency_closure.reasons.contains(
        &crate::IncompleteReason::OmittedUnrepresentable(library.clone())
    ));
    assert!(
        spec.profile
            .spec
            .runtime_reads
            .iter()
            .all(|grant| grant.path != library)
    );
    assert!(
        spec.omitted_discovery
            .iter()
            .any(|entry| entry.path == library)
    );
    assert_eq!(tool.receipt()["reason"], "unrepresentable_path");
    let receipt = spec.applied_access().receipt(&home);
    assert!(
        receipt["omitted_grants"]
            .as_array()
            .ok_or("omissions")?
            .iter()
            .any(|entry| entry["reason"] == "unrepresentable_path")
    );
    Ok(())
}

#[test]
fn discovered_grants_in_credential_roots_are_omitted() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().canonicalize()?;
    let credential = home.join(".gemini/private");
    let node_prefix = credential.join("node-prefix");
    std::fs::create_dir_all(credential.join("bin"))?;
    std::fs::create_dir_all(node_prefix.join("bin"))?;
    std::fs::create_dir_all(home.join(".rustup/toolchains"))?;
    std::os::unix::fs::symlink(&credential, home.join(".cargo"))?;
    let node_alias = home.join("node-prefix");
    std::os::unix::fs::symlink(&node_prefix, &node_alias)?;

    let plan = fixture_access_plan(Access::WorktreeWrite, Some("login"), false);
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture".into());
    let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
    input.home = &home;
    let mut spec = crate::codex::Codex.prepare(input)?;
    assert!(
        spec.omitted_discovery
            .iter()
            .any(|entry| entry.path == credential)
    );
    assert!(
        spec.profile
            .spec
            .runtime_reads
            .iter()
            .all(|grant| !grant.path.starts_with(&credential))
    );

    // Feed an independently discovered Node prefix through the same resolver.
    spec.tool_requirements = crate::launch::ToolRequirements::default();
    crate::launch::homebrew_tools(&node_alias, &mut spec.tool_requirements, &mut spec.env);
    spec.complete_tool_requirements(&plan, &home, Path::new("/fixture/repo"));
    let node_bin = node_prefix.join("bin");
    assert!(
        spec.omitted_discovery
            .iter()
            .any(|entry| entry.path == node_bin)
    );
    assert!(
        spec.profile
            .spec
            .tool_exec_roots
            .iter()
            .all(|grant| grant.path != node_alias.join("bin"))
    );
    assert!(
        !spec
            .applied_access()
            .receipt(&home)
            .to_string()
            .contains(&home.display().to_string())
    );
    Ok(())
}

#[test]
fn opencode_skill_and_provider_config_omissions_keep_both_sources()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().canonicalize()?;
    let secret = home.join(".gemini/skills");
    std::fs::create_dir_all(&secret)?;
    std::fs::create_dir_all(home.join(".config"))?;
    std::os::unix::fs::symlink(&secret, home.join(".config/opencode"))?;
    std::os::unix::fs::symlink(&secret, secret.join("skills"))?;
    let mut route = route("opencode", "fixture-model");
    route.account_key = None;
    let plan = fixture_access_plan(Access::ReadOnly, Some("login"), false);
    let prompt = Prompt::new("fixture".into());
    let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
    input.home = &home;
    let spec = opencode::OpenCode.prepare(input)?;
    assert!(
        spec.user_skills
            .warnings
            .iter()
            .any(|warning| warning.reason == "credential_root")
    );
    assert_eq!(
        spec.omitted_discovery,
        vec![crate::launch::OmittedGrant {
            path: secret,
            source: crate::launch::OmissionSource::ProviderConfig,
        }]
    );
    Ok(())
}

#[test]
fn discovered_skill_links_into_credential_roots_are_omitted()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().canonicalize()?;
    let skills = home.join("skills");
    let secret = home.join(".gemini/private");
    std::fs::create_dir_all(&skills)?;
    std::fs::create_dir_all(&secret)?;
    std::os::unix::fs::symlink(&secret, skills.join("linked"))?;
    let staged = crate::skill_stage::capture(&skills, &home.join("staged"), &home, 4096);
    assert!(
        staged
            .warnings
            .iter()
            .any(|warning| warning.path == Path::new("linked")
                && warning.reason == "credential_root")
    );
    assert_eq!(staged.file_count, 0);
    Ok(())
}

fn test_macho(names: &[&str]) -> Vec<u8> {
    let mut image = vec![0; 32];
    image[..4].copy_from_slice(&0xfeed_facf_u32.to_le_bytes());
    image[12..16].copy_from_slice(&6_u32.to_le_bytes());
    image[16..20].copy_from_slice(&(names.len() as u32).to_le_bytes());
    for name in names {
        let size = (24 + name.len() + 1).next_multiple_of(8);
        image.extend(0xc_u32.to_le_bytes());
        image.extend((size as u32).to_le_bytes());
        image.extend(24_u32.to_le_bytes());
        image.resize(image.len() + 12, 0);
        image.extend(name.as_bytes());
        image.resize(image.len() + size - 24 - name.len(), 0);
    }
    image
}

fn assert_lane_user_skills(adapter: &str) -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().canonicalize()?;
    // Exercise OpenCode's host-selected XDG location, not the fallback only.
    let config_home = home.join("xdg-config");
    let host_var =
        |key: &str| (key == "XDG_CONFIG_HOME").then(|| config_home.clone().into_os_string());
    let skills = match adapter {
        "claude" => home.join(".claude/skills"),
        "codex" => home.join(".agents/skills"),
        "agy" => home.join(".gemini/skills"),
        "cursor" => home.join(".cursor/skills"),
        "opencode" => config_home.join("opencode/skills"),
        _ => unreachable!(),
    };
    let opencode_config = config_home.join("opencode/opencode.json");
    if adapter == "opencode" {
        std::fs::create_dir_all(opencode_config.parent().unwrap())?;
        std::fs::write(&opencode_config, "{}")?;
    }
    let target = home.join(".local/share/provenant/v1/releases/digest/skill");
    std::fs::create_dir_all(&target)?;
    let resource = home.join("shared/reference.md");
    std::fs::create_dir_all(resource.parent().unwrap())?;
    std::fs::write(&resource, "fixture reference")?;
    std::os::unix::fs::symlink(&resource, target.join("reference.md"))?;
    // Give every adapter a real, synthetic Mach-O closure so equality is
    // checked for an applied library as well as an empty closure.
    let library = home.join("libraries/libfixture.dylib");
    let executable = home.join("package/bin/provider");
    std::fs::create_dir_all(library.parent().unwrap())?;
    std::fs::create_dir_all(executable.parent().unwrap())?;
    std::fs::write(&library, test_macho(&[]))?;
    std::fs::write(&executable, test_macho(&[library.to_str().unwrap()]))?;
    let prompt = Prompt::new("fixture".into());
    let token = fixture_token();
    let authentication = match adapter {
        "codex" | "cursor" | "opencode" => Some("login"),
        "agy" => Some("keychain"),
        _ => None,
    };
    let provider = REGISTRY
        .iter()
        .find(|entry| entry.adapter.id() == adapter)
        .unwrap()
        .adapter;
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        for state in ["missing", "discover", "off"] {
            let enabled = state != "off";
            if state == "discover" {
                std::fs::create_dir_all(&skills)?;
                std::os::unix::fs::symlink(&target, skills.join("skill"))?;
            }
            let plan = fixture_access_plan(access, authentication, false);
            let mut route = route(adapter, "fixture-model");
            route.account_key = None;
            route.set_user_skills(enabled);
            let mut input = fixture_input(
                &route,
                &plan,
                &prompt,
                Path::new("/fixture/repo"),
                Some(&token),
            );
            input.home = &home;
            input.executable = &executable;
            input.host_var = &host_var;
            let spec = provider.prepare(input)?;
            provenant_confinement::permissions::snapshot::render(
                plan.permissions(),
                &spec.profile,
                &home,
            )?;
            assert_applied_access(&spec, &home);
            assert!(
                spec.applied_access()
                    .libraries
                    .contains(&crate::launch::exact(&library)),
                "{adapter} {access:?} {state}"
            );
            for grant in &spec.applied_access().libraries {
                assert_eq!(
                    grant.scope,
                    provenant_confinement::permissions::PathScope::Exact
                );
                assert!(
                    provenant_confinement::write_grants(plan.permissions(), &spec.profile)
                        .iter()
                        .all(|write| !write.path.starts_with(&grant.path)
                            && !grant.path.starts_with(&write.path))
                );
                assert!(
                    spec.profile.spec.runtime_reads.iter().all(|read| read.scope
                        != provenant_confinement::permissions::PathScope::Subtree
                        || !grant.path.starts_with(&read.path)),
                    "library {grant:?}, reads {:?}",
                    spec.profile.spec.runtime_reads
                );
            }
            let exposed = state == "discover";
            assert_eq!(
                spec.user_skills.root.is_some(),
                exposed,
                "{adapter} {access:?} {state}"
            );
            for path in [&skills, &target, &resource] {
                assert!(
                    spec.profile
                        .spec
                        .runtime_reads
                        .iter()
                        .all(|grant| !covers(grant, path)),
                    "{adapter} granted host skill path {path:?}"
                );
            }
            if let Some(staged) = &spec.user_skills.root {
                assert!(
                    spec.profile
                        .spec
                        .runtime_reads
                        .contains(&crate::launch::subtree(staged))
                );
                let writes = spec
                    .profile
                    .spec
                    .tool_writes
                    .iter()
                    .chain(&spec.profile.spec.state_writes)
                    .chain(
                        spec.profile
                            .spec
                            .authentication
                            .iter()
                            .flat_map(|auth| &auth.in_place_writes),
                    )
                    .chain(std::iter::once(&plan.permissions().scratch))
                    .chain(&plan.permissions().private_state);
                assert!(
                    writes
                        .into_iter()
                        .all(|grant| !overlaps_skill(grant, staged))
                );
                let env_path = |name: &str| -> std::path::PathBuf {
                    spec.env
                        .iter()
                        .find(|(key, _)| key == name)
                        .map(|(_, value)| std::path::PathBuf::from(value))
                        .expect("adapter environment")
                };
                let expected = match adapter {
                    "claude" => env_path("CLAUDE_CONFIG_DIR").join("skills"),
                    "codex" => env_path("HOME").join(".agents/skills"),
                    "agy" => env_path("HOME").join(".gemini/skills"),
                    "cursor" => env_path("HOME").join(".cursor/skills"),
                    "opencode" => env_path("XDG_CONFIG_HOME").join("opencode/skills"),
                    _ => unreachable!(),
                };
                assert!(
                    spec.setup.iter().any(|step| matches!(step,
                    SetupStep::Symlink { link, target } if link == &expected && target == staged)),
                    "{adapter} must discover staged tree at {expected:?}"
                );
                assert_eq!(spec.user_skills.file_count, 1);
            }
            if adapter == "opencode" {
                assert!(
                    spec.profile
                        .spec
                        .authentication[0].roots
                        .contains(&crate::launch::exact(&opencode_config))
                );
            }
            if state == "off" {
                std::fs::remove_file(skills.join("skill"))?;
                std::fs::remove_dir(&skills)?;
            }
        }
    }
    Ok(())
}

fn assert_applied_access(spec: &crate::LaunchSpec, home: &Path) {
    use sha2::{Digest, Sha256};
    use std::os::unix::ffi::OsStrExt;
    let applied = spec.applied_access();
    for tool in spec.prepared_tools() {
        for library in &tool.dependency_closure.libraries {
            let grant = crate::launch::exact(library);
            assert!(spec.profile.spec.runtime_reads.contains(&grant));
            assert!(applied.libraries.contains(&grant));
        }
    }
    assert_eq!(applied.toolchain_roots, spec.profile.spec.tool_exec_roots);
    assert_eq!(applied.omitted_grants, spec.omitted_discovery);
    assert_eq!(
        applied.staged_content,
        spec.user_skills.root.as_ref().map(crate::launch::subtree)
    );
    let receipt = applied.receipt(home);
    assert_eq!(
        receipt["libraries"].as_array().unwrap().len(),
        applied.libraries.len()
    );
    assert_eq!(
        receipt["toolchain_roots"].as_array().unwrap().len(),
        applied.toolchain_roots.len()
    );
    assert_eq!(
        receipt["omitted_grants"].as_array().unwrap().len(),
        applied.omitted_grants.len()
    );
    for (key, grants) in [
        ("libraries", &applied.libraries),
        ("toolchain_roots", &applied.toolchain_roots),
    ] {
        for (reported, grant) in receipt[key].as_array().unwrap().iter().zip(grants) {
            let mut digest = Sha256::new();
            digest.update(b"provenant.applied-path.v1\0");
            digest.update(grant.path.as_os_str().as_bytes());
            assert_eq!(
                reported["path_id"],
                format!("sha256:{}", provenant_core::hex(&digest.finalize()))
            );
            assert_eq!(
                reported["scope"],
                serde_json::to_value(grant.scope).unwrap()
            );
        }
    }
    for (reported, omitted) in receipt["omitted_grants"]
        .as_array()
        .unwrap()
        .iter()
        .zip(&applied.omitted_grants)
    {
        let mut digest = Sha256::new();
        digest.update(b"provenant.applied-path.v1\0");
        digest.update(omitted.path.as_os_str().as_bytes());
        assert_eq!(
            reported["path_id"],
            format!("sha256:{}", provenant_core::hex(&digest.finalize()))
        );
    }
    assert!(!receipt.to_string().contains(&home.display().to_string()));
}

#[test]
fn applied_access_matches_compiled_profile() -> Result<(), Box<dyn std::error::Error>> {
    for spec in provenant_records::ADAPTER_SPECS
        .iter()
        .filter(|spec| spec.skills.is_some())
    {
        assert_lane_user_skills(spec.name)?;
    }
    let plan = fixture_access_plan(Access::ReadOnly, None, false);
    let mut route = route("command", "");
    route.account_key = None;
    let prompt = Prompt::new("fixture".into());
    let temp = tempfile::tempdir()?;
    let home = temp.path().canonicalize()?;
    let library = home.join("library.dylib");
    let executable = home.join("tool");
    std::fs::write(&library, test_macho(&[]))?;
    std::fs::write(&executable, test_macho(&[library.to_str().unwrap()]))?;
    let argv = vec![executable.display().to_string()];
    let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
    input.home = &home;
    input.executable = &executable;
    input.command = Some(crate::command::Invocation {
        argv: &argv,
        program: &executable,
        runtimes: &[],
    });
    let mut spec = crate::command::Command.prepare(input)?;
    let project_toolchain = home.join("project-toolchain");
    std::fs::create_dir_all(&project_toolchain)?;
    let project_grant = crate::launch::subtree(&project_toolchain);
    let project_receipt = serde_json::json!({"exec_roots": [{"path": "./project-toolchain"}]});
    spec.set_project_requirements(
        vec![project_grant.clone()],
        vec![project_grant.clone()],
        vec![],
        project_receipt.clone(),
    );
    spec.complete_tool_requirements(&plan, &home, Path::new("/fixture/repo"));
    assert_eq!(spec.project_requirements_receipt(), Some(&project_receipt));
    assert!(spec.profile.spec.runtime_reads.contains(&project_grant));
    assert!(spec.profile.spec.tool_exec_roots.contains(&project_grant));
    provenant_confinement::permissions::snapshot::render(plan.permissions(), &spec.profile, &home)?;
    assert_applied_access(&spec, &home);
    assert!(
        spec.applied_access()
            .libraries
            .contains(&crate::launch::exact(&library))
    );
    Ok(())
}

#[test]
fn lane_path_is_independent_of_host_dependency_path() -> Result<(), Box<dyn std::error::Error>> {
    let plan = fixture_access_plan(Access::WorktreeWrite, Some("login"), false);
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture".into());
    let login = |key: &str| (key == "PATH").then(|| "/host/login/bin:/usr/bin".into());
    let daemon = |key: &str| (key == "PATH").then(|| "/host/daemon/bin:/bin".into());
    let build = |host_var: &dyn Fn(&str) -> Option<std::ffi::OsString>| {
        let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
        input.host_var = host_var;
        crate::codex::Codex.prepare(input)
    };
    let login = build(&login)?;
    let daemon = build(&daemon)?;
    let path = |spec: &crate::LaunchSpec| {
        spec.env
            .iter()
            .find(|(key, _)| key == "PATH")
            .map(|(_, value)| value.clone())
    };
    assert_eq!(path(&login), path(&daemon));
    assert_eq!(login.profile.spec, daemon.profile.spec);
    Ok(())
}

macro_rules! user_skills_adapter_test {
    ($name:ident, $adapter:literal) => {
        #[test]
        fn $name() -> Result<(), Box<dyn std::error::Error>> {
            assert_lane_user_skills($adapter)
        }
    };
}
user_skills_adapter_test!(
    claude_user_skills_are_read_only_optional_and_missing_safe,
    "claude"
);
user_skills_adapter_test!(
    codex_user_skills_are_read_only_optional_and_missing_safe,
    "codex"
);
user_skills_adapter_test!(
    agy_user_skills_are_read_only_optional_and_missing_safe,
    "agy"
);
user_skills_adapter_test!(
    opencode_user_skills_are_read_only_optional_and_missing_safe,
    "opencode"
);
user_skills_adapter_test!(
    cursor_user_skills_are_read_only_optional_and_missing_safe,
    "cursor"
);

#[test]
fn claude_user_skills_use_private_placement_for_registered_readers()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().canonicalize()?;
    let skills = home.join(".claude/skills");
    std::fs::create_dir_all(&skills)?;
    let account_dir = home.join("accounts/claude");
    let key = identity::fingerprint("fixture@example.test").unwrap();
    let account = identity::RegisteredAccount::new("claude", &key, account_dir.clone())?;
    let token = fixture_token().for_account(&key, &account_dir);
    let plan = fixture_access_plan(Access::ReadOnly, None, false);
    let prompt = Prompt::new("fixture".into());
    let mut route = route("claude", "fixture-model");
    route.account_key = Some(key);
    let mut input = fixture_input(
        &route,
        &plan,
        &prompt,
        Path::new("/fixture/repo"),
        Some(&token),
    );
    input.home = &home;
    input.account = Some(&account);
    let spec = crate::claude::Claude.prepare(input)?;
    let staged = spec.user_skills.root.as_ref().expect("staged skills");
    assert!(
        spec.env
            .iter()
            .any(|(key, value)| key == "CLAUDE_CONFIG_DIR"
                && value == "/fixture/state/claude-config")
    );
    assert!(
        spec.setup
            .iter()
            .any(|step| matches!(step, SetupStep::Symlink { target, .. } if target == staged))
    );
    assert!(
        !spec
            .setup
            .iter()
            .any(|step| matches!(step, SetupStep::Symlink { target, .. } if target == &skills))
    );
    assert!(
        spec.profile
            .spec
            .runtime_reads
            .iter()
            .all(|grant| !covers(grant, &account_dir))
    );
    Ok(())
}

#[test]
#[cfg_attr(not(target_os = "macos"), ignore = "requires macOS sandbox")]
#[allow(clippy::disallowed_methods)]
fn staged_skills_native_probes() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::permissions::{
        AccessMode, WorktreeWriteAuthority, snapshot::render,
    };
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    for adapter in ["claude", "codex", "cursor", "agy", "opencode"] {
        for access in [Access::ReadOnly, Access::WorktreeWrite] {
            let fixture = root.join(format!("{adapter}-{access:?}"));
            let home = fixture.join("home");
            let repo = fixture.join("repo");
            for dir in [
                &home,
                &repo,
                &fixture.join("scratch"),
                &fixture.join("state"),
                &fixture.join("package/bin"),
            ] {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::create_dir_all(repo.join(".git"))?;
            std::fs::create_dir_all(fixture.join("cargo-cache"))?;
            let skills = match adapter {
                "claude" => home.join(".claude/skills"),
                "codex" => home.join(".agents/skills"),
                "cursor" => home.join(".cursor/skills"),
                "agy" => home.join(".gemini/skills"),
                _ => home.join(".config/opencode/skills"),
            };
            std::fs::create_dir_all(&skills)?;
            std::fs::write(skills.join("SKILL.md"), b"fixture skill")?;
            let codex_auth = home.join(".codex/auth.json");
            std::fs::create_dir_all(codex_auth.parent().ok_or("auth parent")?)?;
            std::fs::write(codex_auth, b"fixture auth")?;
            let secret = home.join(".claude/.credentials.json");
            std::fs::create_dir_all(secret.parent().ok_or("secret parent")?)?;
            std::fs::write(&secret, b"sentinel")?;
            let keychain = home.join("Library/Keychains/login.keychain-db");
            std::fs::create_dir_all(keychain.parent().ok_or("keychain parent")?)?;
            std::fs::write(keychain, b"fixture")?;
            let executable = fixture.join("package/bin/provider");
            std::fs::copy(std::env::current_exe()?, &executable)?;
            let authentication = match adapter {
                "agy" => Some("keychain"),
                "codex" | "cursor" | "opencode" => Some("login"),
                _ => None,
            };
            let base = read_only_plan(fixture.to_str().ok_or("fixture path")?, authentication);
            let mut permissions = base.permissions().clone();
            if access == Access::WorktreeWrite {
                let authority: WorktreeWriteAuthority =
                    serde_json::from_value(serde_json::json!({
                        "git_grants_digest": vec![0; 32], "publishing": false,
                        "common_git_dir": repo.join(".git"),
                        "cargo_cache": {"path": fixture.join("cargo-cache"), "scope": "subtree"}
                    }))?;
                permissions.mode = AccessMode::WorktreeWrite(Box::new(authority));
            }
            let state = fixture.join("state");
            permissions.policy = Some(
                provenant_confinement::run_config::RunConfig::default().policy_for_launch(
                    &state,
                    provenant_confinement::permissions::MandatoryRoots::new(&home, &state, &repo)?,
                ),
            );
            let mut plan = ConfinedPlan::new(PlanRequest {
                scope: base.scope(),
                permissions,
                codex: CodexPolicy::Clean,
                extensions: ExtensionPolicy::Deny,
                agy_state: None,
            })?;
            let mut route = route(adapter, "fixture-model");
            route.account_key = None;
            route.set_user_skills(true);
            let prompt = Prompt::new("fixture".into());
            let token = fixture_token();
            let mut input = fixture_input(&route, &plan, &prompt, &repo, Some(&token));
            input.home = &home;
            input.executable = &executable;
            let provider = REGISTRY
                .iter()
                .find(|entry| entry.adapter.id() == adapter)
                .ok_or("adapter")?
                .adapter;
            let launch = provider.prepare(input)?;
            let stage = launch.user_skills.root.as_ref().ok_or("missing stage")?;
            std::fs::create_dir_all(stage)?;
            for step in launch.user_skills.setup() {
                match step {
                    SetupStep::CreateDir(path) => std::fs::create_dir_all(path)?,
                    SetupStep::WriteFile { path, bytes } => std::fs::write(path, bytes)?,
                    _ => return Err("unexpected staged setup".into()),
                }
            }
            // Preparation binds the stage as an immutable mandatory read.
            plan.policy_mut()
                .ok_or("missing lane policy")?
                .mandatory_mut()
                .add_state_read_exec(provenant_confinement::permissions::PathGrant {
                    path: stage.clone(),
                    scope: provenant_confinement::permissions::PathScope::Subtree,
                })?;
            let profile = render(plan.permissions(), &launch.profile, &home)?;
            let mut probe = std::process::Command::new("/usr/bin/sandbox-exec");
            probe
                .args(["-p", &profile])
                .arg(&executable)
                .args(["--exact", "tests::staged_skills_probe_child", "--ignored", "--nocapture"])
                .env_clear()
                .env("PROVENANT_PROBE_STAGE", stage.join("SKILL.md"))
                .env("PROVENANT_PROBE_SECRET", &secret);
            // Codex's host skills sit outside the mandatory credential roots.
            if adapter != "codex" {
                probe.env("PROVENANT_PROBE_HOST", skills.join("SKILL.md"));
            }
            let output = probe.output()?;
            let error = String::from_utf8_lossy(&output.stderr);
            if error.contains("sandbox_apply: Operation not permitted") {
                provenant_test_support::skip_sandbox_dependent_test(error.to_string())?;
                return Ok(());
            }
            assert!(
                output.status.success(),
                "{adapter} {access:?}: {error} {}",
                String::from_utf8_lossy(&output.stdout)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout)
                    .contains("staged skills assertions completed"),
                "{adapter} {access:?}: probe child did not run"
            );
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "invoked by staged_skills_native_probes"]
fn staged_skills_probe_child() {
    let stage = std::env::var_os("PROVENANT_PROBE_STAGE").expect("stage path");
    let secret = std::env::var_os("PROVENANT_PROBE_SECRET").expect("secret path");
    assert_eq!(std::fs::read(&stage).unwrap(), b"fixture skill");
    assert!(std::fs::read(&secret).is_err());
    assert!(std::fs::write(&stage, b"x").is_err());
    assert!(std::fs::write(&secret, b"x").is_err());
    if let Some(host) = std::env::var_os("PROVENANT_PROBE_HOST") {
        assert!(std::fs::read(&host).is_err());
        assert!(std::fs::write(&host, b"x").is_err());
    }
    std::io::Write::write_all(
        &mut std::io::stdout(),
        b"staged skills assertions completed\n",
    )
    .expect("write probe marker");
}

#[test]
fn opencode_starts_each_completed_text_part_on_a_new_message() -> Result<(), ProviderError> {
    let stream = r#"{"type":"text","sessionID":"s","part":{"text":"prose"}}
{"type":"text","sessionID":"s","part":{"text":"VERDICT: PASS"}}
"#;
    let (events, _) = replay(&opencode::OpenCode, stream, EXIT)?;
    let boundary = events
        .iter()
        .position(|event| *event == ProviderEvent::Text("VERDICT: PASS".into()))
        .and_then(|index| index.checked_sub(1))
        .map(|index| &events[index]);
    assert_eq!(boundary, Some(&ProviderEvent::MessageStart));
    Ok(())
}

#[test]
fn tool_search_path_preserves_native_home_and_omits_unusable_entry() {
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;
    let home = PathBuf::from(OsString::from_vec(b"/fixture/home-\xff".to_vec()));
    let path = crate::launch::tool_search_path(&home, true);
    assert_eq!(
        std::env::split_paths(&path).next(),
        Some(home.join(".cargo/bin"))
    );
    let path = crate::launch::tool_search_path(Path::new("/fixture/home:with-separator"), true);
    assert_eq!(
        std::env::split_paths(&path).collect::<Vec<_>>(),
        [PathBuf::from("/usr/bin"), PathBuf::from("/bin")]
    );
    let path = crate::launch::tool_search_path(&home, false);
    assert_eq!(
        std::env::split_paths(&path).next(),
        Some(PathBuf::from("/usr/bin"))
    );
}

#[test]
fn claude_writer_requires_private_state() -> Result<(), ProviderError> {
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    let original = fixture_access_plan(Access::WorktreeWrite, None, false);
    let mut permissions = original.permissions().clone();
    permissions.private_state = None;
    let plan = ConfinedPlan::new(PlanRequest {
        scope: original.scope(),
        permissions,
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: None,
    })
    .expect("fixture plan");
    let mut route = route("claude", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture".into());
    let token = fixture_token();
    assert!(matches!(claude::Claude.prepare(fixture_input(
        &route, &plan, &prompt, Path::new("/fixture/repo"), Some(&token),
    )), Err(ProviderError::UnsupportedPolicy)));
    Ok(())
}

#[test]
fn opencode_requires_private_state() -> Result<(), ProviderError> {
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    let route = route("opencode", "fixture-model");
    let prompt = Prompt::new("fixture".into());
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        let original = fixture_access_plan(access, Some("login"), false);
        let mut permissions = original.permissions().clone();
        permissions.private_state = None;
        let plan = ConfinedPlan::new(PlanRequest {
            scope: original.scope(),
            permissions,
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
        })
        .expect("fixture plan");
        assert!(matches!(opencode::OpenCode.prepare(fixture_input(
            &route, &plan, &prompt, Path::new("/fixture/repo"), None,
        )), Err(ProviderError::UnsupportedPolicy)));

    }
    Ok(())
}

#[test]
fn agy_uses_scratch_when_private_state_is_absent() -> Result<(), ProviderError> {
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    let mut route = route("agy", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture".into());
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        let original = fixture_access_plan(access, Some("keychain"), false);
        let mut permissions = original.permissions().clone();
        permissions.private_state = None;
        let plan = ConfinedPlan::new(PlanRequest {
            scope: original.scope(),
            permissions,
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
        })
        .expect("fixture plan");
        let launch = agy::Agy.prepare(fixture_input(
            &route,
            &plan,
            &prompt,
            Path::new("/fixture/repo"),
            None,
        ))?;
        let scratch = &plan.permissions().scratch.path;
        assert!(
            launch
                .env
                .iter()
                .any(|(name, value)| name == "HOME" && value == scratch.as_os_str())
        );
        assert_eq!(launch.profile.spec.authentication.len(), 1);
        let authentication = &launch.profile.spec.authentication[0];
        assert_eq!(authentication.mechanism, "keychain");
        assert_eq!(
            authentication.roots,
            [crate::launch::exact(
                "/fixture/home/Library/Keychains/login.keychain-db"
            )]
        );
        assert!(authentication.in_place_writes.is_empty());
    }
    Ok(())
}
