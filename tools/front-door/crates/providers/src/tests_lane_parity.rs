/// Exercise the real adapter preparation and confinement renderer from the
/// registry. Provider-specific authentication stays declared; common lane
/// authority, networking and host Cargo bindings must not vary with a client.
#[test]
fn registered_adapters_render_common_lane_capabilities() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::permissions::{AccessMode, NetworkAccess, PathScope};
    use provenant_confinement::{ConfinedPlan, PlanRequest};
    use provenant_records::AdapterSpec;

    let prompt = Prompt::new("parity fixture".into());
    let token = fixture_token();
    let host_cargo = Path::new("/fixture/host-cargo");
    let host_var = |key: &str| (key == "CARGO_HOME").then(|| host_cargo.as_os_str().to_owned());
    for entry in REGISTRY {
        let declaration =
            AdapterSpec::get(entry.adapter.id()).expect("registered adapter declaration");
        assert_eq!(declaration.launch.default_network, NetworkAccess::Open);
        let accesses: &[Access] = if declaration.launch.write {
            &[Access::ReadOnly, Access::WorktreeWrite]
        } else {
            &[Access::ReadOnly]
        };
        for &access in accesses {
            for &network in declaration.launch.network {
                let base = fixture_network_plan(access, declaration.authentication, network);
                let mut permissions = base.permissions().clone();
                for name in provenant_core::CARGO_CREDENTIAL_FILES {
                    permissions.credential_roots.add(
                        host_cargo.join(name),
                        provenant_core::CredentialScope::Exact,
                        "host Cargo home declaration",
                        provenant_core::CredentialKind::Store,
                    );
                }
                let peers = ["/fixture/primary", "/fixture/sibling-worktree"];
                permissions
                    .protected_roots
                    .extend(peers.map(crate::launch::subtree));
                permissions
                    .read_roots
                    .push(crate::launch::subtree("/fixture/shared-read"));
                let plan = ConfinedPlan::new(PlanRequest {
                    scope: base.scope(),
                    permissions,
                    codex: provenant_confinement::CodexPolicy::Clean,
                    extensions: provenant_confinement::ExtensionPolicy::Deny,
                    agy_state: None,
                })?;
                let mut route = route(
                    declaration.name,
                    if declaration.launch.model {
                        "fixture-model"
                    } else {
                        ""
                    },
                );
                route.account_key = None;
                let mut input = fixture_input(
                    &route,
                    &plan,
                    &prompt,
                    Path::new("/fixture/repo"),
                    Some(&token),
                );
                input.host_var = &host_var;
                input.credential_roots = plan.permissions().credential_roots.clone();
                let command_argv = vec!["fixture-command".into()];
                if declaration.named_entry == provenant_records::NamedEntry::Command {
                    input.command = Some(crate::command::Invocation {
                        argv: &command_argv,
                        runtimes: &[],
                        program: input.executable,
                    });
                }
                let hook_binary = input.provenant.executable;
                let hook_state = input.provenant.state;
                let launch = entry.adapter.prepare(input)?;
                let context = format!("{} {access:?} {network:?}", declaration.name);
                if let Some(install) = declaration.install {
                    if let Some(instruction) = install.instruction {
                        assert!(install.instructions.is_supported(), "{context}");
                        let (variable, suffix) = install.lane_home;
                        let child_home = launch
                            .env
                            .iter()
                            .find(|(key, _)| key == variable)
                            .map(|(_, value)| std::path::PathBuf::from(value))
                            .expect("declared lane home binding");
                        let destination = child_home.join(suffix).join(instruction);
                        assert!(launch.setup.iter().any(|step| matches!(step,
                            SetupStep::WriteFile { path, bytes } if path == &destination && bytes.as_slice() == provenant_records::INSTRUCTION_CONTENT
                        )), "{context}: missing shared instruction projection");
                    } else if let provenant_records::CapabilitySupport::Unsupported {
                        reason,
                        source,
                    } = install.instructions
                    {
                        assert!(!reason.is_empty() && !source.is_empty(), "{context}");
                    } else {
                        panic!("{context}: supported instruction capability has no path");
                    }
                }
                if let Some(install) = declaration
                    .install
                    .filter(|install| install.hooks.is_supported())
                {
                    let (variable, suffix) = install.lane_home;
                    let child_home = launch
                        .env
                        .iter()
                        .find(|(key, _)| key == variable)
                        .map(|(_, value)| std::path::PathBuf::from(value))
                        .expect("declared lane home binding");
                    let destination = child_home
                        .join(suffix)
                        .join(install.hook_file.expect("supported hook path"));
                    let actual = launch
                        .setup
                        .iter()
                        .find_map(|step| match step {
                            SetupStep::WriteFile { path, bytes } if path == &destination => {
                                Some(bytes)
                            }
                            _ => None,
                        })
                        .expect("every supported harness receives native lane hooks");
                    if install.hook_protocol
                        == Some(provenant_records::HookProtocol::OpenCodePlugin)
                    {
                        assert_eq!(
                            actual,
                            &provenant_config::hook_plugin_source(hook_binary, hook_state)?,
                            "{context}"
                        );
                    } else {
                        let mut expected = provenant_config::lane_hook_settings(
                            declaration.id,
                            hook_binary,
                            hook_state,
                        )?;
                        if install.hook_settings_arg.is_some() {
                            expected["attribution"] = serde_json::json!({"commit": "", "pr": ""});
                        }
                        assert_eq!(
                            serde_json::from_slice::<serde_json::Value>(actual)?,
                            expected,
                            "{context}"
                        );
                    }
                }
                for step in &launch.setup {
                    let SetupStep::WriteFile { path, .. } = step else {
                        continue;
                    };
                    assert!(
                        !launch.setup.iter().any(|candidate| matches!(candidate,
                            SetupStep::Symlink { link, .. } if path.starts_with(link)
                        )),
                        "{context}: generated file traverses a setup symlink"
                    );
                }
                assert!(launch.profile.spec.allow_process_fork, "{context}");
                for shell_root in ["/bin", "/usr/bin"] {
                    assert!(
                        launch.profile.spec.tool_exec_roots.iter().any(|grant| {
                            grant.path == Path::new(shell_root) && grant.scope == PathScope::Subtree
                        }),
                        "{context}: missing shell tools at {shell_root}"
                    );
                }
                let cargo = launch
                    .cargo_home()
                    .expect("every launch finalises host Cargo");
                assert_eq!(cargo.path, host_cargo, "{context}");
                assert!(cargo.source_writes, "{context}");
                let env_path = |key: &str| {
                    launch
                        .env
                        .iter()
                        .find(|(name, _)| name == key)
                        .map(|(_, value)| Path::new(value))
                };
                assert_eq!(env_path("CARGO_HOME"), Some(host_cargo), "{context}");
                let target =
                    env_path("CARGO_TARGET_DIR").expect("every launch binds its Cargo target");
                let cache = match &plan.permissions().mode {
                    AccessMode::ReadOnly => &plan.permissions().scratch,
                    AccessMode::WorktreeWrite(authority) => authority.cargo_cache(),
                };
                assert!(
                    target.starts_with(&cache.path),
                    "{context}: target outside lane cache"
                );
                let rendered = provenant_confinement::permissions::snapshot::render(
                    plan.permissions(),
                    &launch.profile,
                    Path::new("/fixture/home"),
                )?;
                for path in peers.into_iter().chain(["/fixture/shared-read"]) {
                    assert!(
                        !rendered.contains(&format!("(deny file-read* (subpath \"{path}\"))")),
                        "{context}: read denied at {path}"
                    );
                }
                for path in peers {
                    assert!(
                        rendered.contains(&format!("(deny file-write* (subpath \"{path}\"))")),
                        "{context}: peer checkout is writable"
                    );
                }
                assert_eq!(
                    rendered.contains("(allow network-outbound (remote ip \"*:*\"))"),
                    network == NetworkAccess::Open,
                    "{context}"
                );
                for name in provenant_core::CARGO_CREDENTIAL_FILES {
                    let path = host_cargo.join(name);
                    for operation in ["file-read*", "file-write*"] {
                        assert!(
                            rendered.contains(&format!(
                                "(deny {operation} (literal \"{}\"))",
                                path.display()
                            )),
                            "{context}: Cargo credential denial missing"
                        );
                    }
                }
                assert_eq!(plan.permissions().network, network, "{context}");
                // Only execution entries without a provider API are entirely
                // offline in provider-only mode; provider adapters retain API reach.
                assert_eq!(
                    launch.profile.spec.confinement.network() == provenant_confinement::permissions::NetworkPolicy::Offline,
                    network == NetworkAccess::Provider
                        && launch.profile.spec.provider_api.is_empty(),
                    "{context}"
                );
            }
        }
    }
    Ok(())
}
