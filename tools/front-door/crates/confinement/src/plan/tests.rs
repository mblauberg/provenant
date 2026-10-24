use super::*;
use crate::permissions::{AccessMode, PathGrant, PathScope, PlanPermissions};
use provenant_core::{ProjectId, WorkScope};
use provenant_supervisor::{fs::LocalFs, home::HomeError};
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
            mode: AccessMode::ReadOnly,
            scratch: grant("scratch"),
            private_state: Some(grant("provider-state")),
            read_roots: vec![grant("work")],
            protected_roots: vec![],
            secret_roots: vec![],
            selected_authentication: None,
            include_continuation: false,
            network: vec![],
            capabilities: vec![],
            nested_confinement: false,
        },
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: Some(root.join("user-state")),
    }
}

#[test]
fn private_state_cannot_expose_credentials_or_alias_the_source() {
    let temp = fixture();
    let mut input = request(temp.path());
    input.agy_state = None;
    input.permissions.private_state.as_mut().unwrap().path = temp.path().join(".gemini");
    assert!(ConfinedPlan::new(input).is_err());

    let root = fs::canonicalize(temp.path()).unwrap();
    symlink(&root, root.join("alias")).unwrap();
    let mut input = request(&root);
    input.permissions.private_state.as_mut().unwrap().path = root.join("alias/user-state");
    let plan = ConfinedPlan::new(input).unwrap();
    assert!(matches!(
        plan.agy_home().unwrap().prepare(&LocalFs),
        Err(HomeError::UnsafeEntry)
    ));
    assert!(!root.join("user-state/agy-home").exists());
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
    let plan = ConfinedPlan::new(request(root)).unwrap();
    let recipe = plan.agy_home().unwrap();
    assert!(!format!("{recipe:?}").contains(token));
    let prepared = recipe.prepare(&LocalFs).unwrap();
    let state = prepared.path().join(".gemini/antigravity-cli");
    assert!(state.join("bin").is_dir());
    assert!(!state.join("oauth_creds.json").exists());
    assert_eq!(
        fs::read(state.join("installation_id")).unwrap(),
        b"existing"
    );
    fs::write(state.join("history.jsonl"), "new history\n").unwrap();
    prepared.copy_back().unwrap();
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
    let prepared = plan.agy_home().unwrap().prepare(&LocalFs).unwrap();
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
        let prepared = plan.agy_home().unwrap().prepare(&LocalFs).unwrap();
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
fn read_only_plans_refuse_unconfined_extensions_and_inherited_codex_config() {
    let temp = fixture();
    let mut input = request(temp.path());
    input.extensions = ExtensionPolicy::Unconfined;
    assert!(matches!(
        ConfinedPlan::new(input),
        Err(PlanError::UnconfinedExtensions)
    ));
    let mut input = request(temp.path());
    input.codex = CodexPolicy::Inherited;
    assert!(matches!(
        ConfinedPlan::new(input),
        Err(PlanError::UnconfinedExtensions)
    ));
}

#[test]
fn database_without_a_snapshot_refuses_instead_of_silently_losing_context() {
    let temp = fixture();
    let plan = ConfinedPlan::new(request(temp.path())).unwrap();
    let prepared = plan.agy_home().unwrap().prepare(&LocalFs).unwrap();
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
        plan.agy_home().unwrap().prepare(&LocalFs),
        Err(HomeError::UnsupportedState)
    ));
}
