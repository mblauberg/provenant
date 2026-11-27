use super::*;
use crate::permissions::{AccessMode, NetworkAccess, PathGrant, PathScope, PlanPermissions};
use provenant_core::{ProjectId, WorkScope};
use provenant_os::home::HomeError;
use provenant_records::policy::{Boundary, Strength};
use provenant_records::warning;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};

fn request(root: &Path) -> PlanRequest {
    let root = fs::canonicalize(root).unwrap();
    let grant = |name: &str| PathGrant {
        path: root.join(name),
        scope: PathScope::Subtree,
    };
    PlanRequest {
        scope: WorkScope::Project(ProjectId::from_bytes([1; 16])),
        permissions: PlanPermissions {
            credential_roots: provenant_core::CredentialRoots::for_home(std::path::Path::new(
                "/fixture/home",
            )),
            policy: None,
            read_only: None,
            mode: AccessMode::ReadOnly,
            scratch: grant("scratch"),
            private_state: Some(grant("provider-state")),
            read_roots: vec![grant("work")],
            protected_roots: vec![],
            secret_roots: vec![],
            selected_authentication: None,
            include_continuation: false,
            write_roots: vec![],
            network: NetworkAccess::Provider,
            nested_confinement: false,
        },
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: Some(root.join("user-state")),
    }
}

// Spec: docs/specs/run-launch.md#policy-and-confinement
#[test]
fn unsupported_nested_confinement_is_reported_as_a_capability_error() {
    let temp = fixture();
    let root = fs::canonicalize(temp.path()).unwrap();
    let mut input = request(&root);
    input.permissions.nested_confinement = true;

    let error = ConfinedPlan::new(input)
        .err()
        .expect("nested backend is unavailable");
    assert!(matches!(
        &error,
        PlanError::Permission(PermissionError::NestedConfinementUnsupported)
    ));
    assert_eq!(error.boundary_class(), Boundary::None);
    // Split from the warn half: the nested claim is an immutable operation
    // row, so an explicit policy code cannot override it.
    let policy = Policy::default();
    let rule = policy
        .rule("PLAN_NESTED_CONFINEMENT_UNSUPPORTED")
        .rule
        .clone();
    assert_eq!(rule.boundary, Boundary::Operation);
    assert_eq!(rule.strength, Strength::Refuse);
    assert!(rule.knob.is_none());
}

#[test]
fn required_credential_grant_refusal_names_the_declared_source() {
    let temp = fixture();
    let root = fs::canonicalize(temp.path()).unwrap();
    let credential_home = root.join("credential-home");
    fs::create_dir(&credential_home).unwrap();
    let credentials = provenant_core::CredentialRoots::for_home(&credential_home);
    let declaration = credentials
        .entries()
        .iter()
        .find(|entry| entry.path == credential_home.join(".gemini"))
        .expect("Gemini home is declared");
    let refused = declaration.path.join("skills");
    let declaration_source = declaration.source.clone();
    let declaration_root = declaration.path.clone();
    fs::create_dir_all(&refused).unwrap();
    let mut input = request(&root);
    input.agy_state = None;
    input.permissions.credential_roots = credentials;
    input.permissions.read_roots = vec![PathGrant {
        path: refused.clone(),
        scope: PathScope::Subtree,
    }];
    let error = ConfinedPlan::new(input)
        .err()
        .expect("credential grant refused");
    assert!(matches!(
        error,
        PlanError::Permission(PermissionError::CredentialStore {
            path,
            root,
            declaration_source: source,
        }) if path == refused && root == declaration_root && source == declaration_source
    ));
}

#[test]
fn private_state_cannot_expose_credentials_or_alias_the_source() {
    let temp = fixture();
    let mut input = request(temp.path());
    input.agy_state = None;
    input.permissions.credential_roots = provenant_core::CredentialRoots::for_home(temp.path());
    input.permissions.private_state.as_mut().unwrap().path = temp.path().join(".gemini");
    assert!(ConfinedPlan::new(input).is_err());

    let root = fs::canonicalize(temp.path()).unwrap();
    symlink(&root, root.join("alias")).unwrap();
    let mut input = request(&root);
    input.permissions.private_state.as_mut().unwrap().path = root.join("alias/user-state");
    let plan = ConfinedPlan::new(input).unwrap();
    assert!(matches!(
        plan.agy_home().unwrap().prepare(),
        Err(HomeError::UnsafeEntry)
    ));
    assert!(!root.join("user-state/agy-home").exists());
}

#[test]
fn write_roots_warn_on_private_overlaps_and_refuse_credentials() {
    let temp = fixture();
    let root = fs::canonicalize(temp.path()).unwrap();
    let credential_home = root.join("credential-home");
    fs::create_dir(&credential_home).unwrap();
    let credentials = provenant_core::CredentialRoots::for_home(&credential_home);
    let with_write = |path: std::path::PathBuf| {
        let mut input = request(&root);
        input.agy_state = None;
        input.permissions.credential_roots = credentials.clone();
        input.permissions.write_roots = vec![PathGrant {
            path,
            scope: PathScope::Subtree,
        }];
        ConfinedPlan::new(input)
    };
    assert!(with_write(root.join("repo/.agent-run")).is_ok());

    let rule = Policy::default()
        .rule("PLAN_CONFINEDPLAN_NEW_DATA_INTEGRITY")
        .rule
        .clone();
    assert_eq!(rule.boundary, Boundary::None);
    assert_eq!(rule.group.as_deref(), Some("confinement"));
    assert_eq!(rule.strength, Strength::Warn);
    assert!(rule.refusable);

    for credential in credentials.entries() {
        assert!(
            with_write(credential.path.clone()).is_err(),
            "{}",
            credential.path.display()
        );
    }

    let scratch_overlap = root.join("scratch/locks");
    let (plan, warnings) = warning::capture(|| with_write(scratch_overlap.clone()));
    let plan = plan.expect("overlapping extra write grant proceeds");
    assert_eq!(plan.permissions().write_roots[0].path, scratch_overlap);
    assert_eq!(warnings.len(), 1);
    assert_eq!(
        warnings[0].code.as_ref(),
        "PLAN_CONFINEDPLAN_NEW_DATA_INTEGRITY"
    );

    let private_overlap = root.join("provider-state");
    let (plan, warnings) = warning::capture(|| with_write(private_overlap.clone()));
    let plan = plan.expect("private state overlap proceeds");
    assert_eq!(plan.permissions().write_roots[0].path, private_overlap);
    assert_eq!(warnings.len(), 1);
    assert_eq!(
        warnings[0].code.as_ref(),
        "PLAN_CONFINEDPLAN_NEW_DATA_INTEGRITY"
    );

    for refused in [
        credential_home.clone(),
        credential_home.parent().unwrap().to_owned(),
    ] {
        assert!(with_write(refused).is_err());
    }
    assert!(with_write(std::path::PathBuf::from("/Users/alice")).is_err());
}

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    for name in ["scratch", "provider-state", "work", "user-state"] {
        fs::create_dir(temp.path().join(name)).unwrap();
        fs::set_permissions(temp.path().join(name), fs::Permissions::from_mode(0o700)).unwrap();
    }
    temp
}

#[test]
fn plan_and_private_home_round_trip_without_exposing_credentials() {
    let temp = fixture();
    let root = temp.path();
    let token = "fixture-secret-never-in-plan";
    fs::write(root.join("user-state/oauth_creds.json"), token).unwrap();
    fs::write(root.join("user-state/installation_id"), "existing").unwrap();
    let controls = [
        "settings.json",
        "config/mcp_config.json",
        "config/plugins/run",
        "sidecars/run",
        "plugin_data/file",
        "knowledge/file",
    ];
    for relative in controls {
        let path = root.join("user-state").join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "user control").unwrap();
    }
    let plan = ConfinedPlan::new(request(root)).unwrap();
    let recipe = plan.agy_home().unwrap();
    assert!(!format!("{recipe:?}").contains(token));
    let prepared = recipe.prepare().unwrap();
    let state = prepared.path().join(".gemini/antigravity-cli");
    assert!(state.join("bin").is_dir());
    assert!(!state.join("oauth_creds.json").exists());
    for relative in controls {
        let path = state.join(relative);
        assert!(!path.exists(), "{relative}");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "lane control").unwrap();
    }
    assert_eq!(
        fs::read(state.join("installation_id")).unwrap(),
        b"existing"
    );
    fs::write(state.join("history.jsonl"), "new history\n").unwrap();
    prepared.copy_back().unwrap();
    for relative in controls {
        assert_eq!(
            fs::read(root.join("user-state").join(relative)).unwrap(),
            b"user control",
            "{relative}"
        );
    }
    assert_eq!(
        fs::read(root.join("user-state/history.jsonl")).unwrap(),
        b"new history\n"
    );
    assert_eq!(
        fs::read(root.join("user-state/oauth_creds.json")).unwrap(),
        token.as_bytes()
    );
}

#[test]
fn copy_back_preserves_newer_user_state_and_retains_the_private_copy() {
    let temp = fixture();
    let plan = ConfinedPlan::new(request(temp.path())).unwrap();
    let prepared = plan.agy_home().unwrap().prepare().unwrap();
    let private = prepared
        .path()
        .join(".gemini/antigravity-cli/history.jsonl");
    fs::write(&private, "lane").unwrap();
    let user = temp.path().join("user-state/history.jsonl");
    fs::write(&user, "newer user data").unwrap();
    assert!(matches!(
        prepared.copy_back(),
        Err(HomeError::Conflict { .. })
    ));
    assert_eq!(fs::read(&user).unwrap(), b"newer user data");
    assert_eq!(fs::read(&private).unwrap(), b"lane");
}

#[test]
fn copy_back_refuses_links_and_private_home_replacement() {
    for replace_parent in [false, true] {
        let temp = fixture();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("history.jsonl"), "outside").unwrap();
        let plan = ConfinedPlan::new(request(temp.path())).unwrap();
        let prepared = plan.agy_home().unwrap().prepare().unwrap();
        let state = prepared.path().join(".gemini/antigravity-cli");
        if replace_parent {
            fs::rename(&state, state.with_extension("moved")).unwrap();
            symlink(outside.path(), &state).unwrap();
        } else {
            symlink(
                outside.path().join("history.jsonl"),
                state.join("history.jsonl"),
            )
            .unwrap();
        }
        assert!(prepared.copy_back().is_err());
        assert!(!temp.path().join("user-state/history.jsonl").exists());
        assert_eq!(
            fs::read(outside.path().join("history.jsonl")).unwrap(),
            b"outside"
        );
    }
}

#[test]
fn read_only_plans_warn_and_proceed_on_extensions_and_inherited_codex_config() {
    let policy = Policy::default();
    let rule = policy.rule("PLAN_CONFINEDPLAN_NEW_AUTHORITY").rule.clone();
    assert_eq!(rule.boundary, Boundary::None);
    assert_eq!(rule.group.as_deref(), Some("confinement"));
    assert_eq!(rule.strength, Strength::Warn);
    assert!(rule.refusable);

    let temp = fixture();
    let mut input = request(temp.path());
    input.extensions = ExtensionPolicy::Unconfined;
    let (plan, warnings) = warning::capture(|| ConfinedPlan::new(input));
    assert!(plan.is_ok(), "{:?}", plan.as_ref().err());
    let warning = warnings.first().expect("confinement warning");
    assert_eq!(warning.code.as_ref(), "PLAN_CONFINEDPLAN_NEW_AUTHORITY");
    let setting = warning.setting.as_ref().expect("losing setting");
    assert_eq!(setting.key, "policy.PLAN_CONFINEDPLAN_NEW_AUTHORITY");

    let mut input = request(temp.path());
    input.codex = CodexPolicy::Inherited;
    let (plan, warnings) = warning::capture(|| ConfinedPlan::new(input));
    assert!(plan.is_ok(), "{:?}", plan.as_ref().err());
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].code.as_ref(), "PLAN_CONFINEDPLAN_NEW_AUTHORITY");
}

#[test]
fn database_without_a_snapshot_refuses_instead_of_silently_losing_context() {
    let temp = fixture();
    let plan = ConfinedPlan::new(request(temp.path())).unwrap();
    let prepared = plan.agy_home().unwrap().prepare().unwrap();
    let state = prepared.path().join(".gemini/antigravity-cli");
    fs::write(
        state.join("conversation_summaries.db-wal"),
        "committed context",
    )
    .unwrap();
    assert!(matches!(
        prepared.copy_back(),
        Err(HomeError::UnsupportedState)
    ));
    fs::write(
        temp.path().join("user-state/conversation_summaries.db"),
        "live database",
    )
    .unwrap();
    assert!(matches!(
        plan.agy_home().unwrap().prepare(),
        Err(HomeError::UnsupportedState)
    ));
}
