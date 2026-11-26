//! Candidate-walk regressions for launch preparation: each route candidate
//! is judged in order, and a refused account never hides a later one.

use super::*;
use provenant_records::{ResolvedRoute, RouteConfidentiality, RouteEligibility, RouteReadiness};

fn route(model: &str, account: &str) -> ResolvedRoute {
    ResolvedRoute {
        selection_context: Default::default(),
        diagnostics: Default::default(),
        user_skills: true,
        access: Default::default(),
        extra: Default::default(),
        adapter: "cursor".into(),
        model: model.into(),
        effort: None,
        catalogue_revision: "fixture".into(),
        account_key: Some(account.into()),
        host: "fixture".into(),
        alternatives: Default::default(),
        exclusions: vec![],
        eligibility: RouteEligibility::Eligible,
        readiness: RouteReadiness::stale(),
        confidentiality: RouteConfidentiality {
            extra: Default::default(),
            evidence_ref: "fixture".into(),
        },
        pool_selection: None,
    }
}

#[test]
fn pin_incompatible_candidate_does_not_hide_later_compatible_candidate() -> Result<(), PrepareError>
{
    let mut queue = [route("wrong", "other"), route("right", "pinned")].into();
    let mut rejected = vec![];
    let mut reasons = vec![];
    let selected = next_permitted_candidate(
        &mut queue,
        &mut rejected,
        &mut reasons,
        (Some("pinned"), Some("cursor")),
        true,
        provenant_routing::FallbackOutcome::Other,
    )?
    .expect("compatible route");
    assert_eq!(selected.model, "right");
    assert_eq!(reasons[0].route, "cursor/wrong");
    assert_eq!(
        reasons[0].cause,
        Some(provenant_records::RouteExclusionReason::AccountPinned)
    );
    assert!(queue.is_empty());
    Ok(())
}

#[test]
fn refused_accounts_leave_only_untried_accounts() {
    let record = |key: &str| provenant_providers::AvailabilityRecord {
        schema_version: 1,
        adapter: "claude".into(),
        executable: "/fixture/claude".into(),
        version: "fixture".into(),
        last_update: None,
        fixture_ref: "fixture".into(),
        catalogue_revision: "fixture".into(),
        account_key: Some(key.into()),
        plan: None,
        observed_models: vec![],
        observed_unix_seconds: 0,
        expires_unix_seconds: 1,
    };
    let all = [record("spent"), record("ready")];
    let mut refused = std::collections::BTreeSet::new();
    refused.insert(("claude".to_owned(), "spent".to_owned()));
    let left = without_refused_accounts(&all, &refused);
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].account_key.as_deref(), Some("ready"));
    refused.insert(("claude".to_owned(), "ready".to_owned()));
    assert!(without_refused_accounts(&all, &refused).is_empty());
}

#[test]
fn admission_exclusions_become_typed_preparation_skips() -> Result<(), Box<dyn std::error::Error>> {
    let spent = provenant_routing::ModelId::parse("cursor/spent")?;
    let admitted = crate::route::admission::AdmittedRoutes {
        exclusions: vec![crate::route::admission::AdmissionExclusion {
            model: spent.clone(),
            reason: provenant_records::RouteExclusionReason::Exhausted,
            detail: "quota admission refused: allowance exhausted".into(),
        }],
        ..Default::default()
    };
    let mut reasons = vec![];
    let mut rejected = vec![];
    record_admission_exclusions(&admitted, &mut reasons, &mut rejected);
    assert_eq!(reasons[0].route, "cursor/spent");
    assert_eq!(
        reasons[0].cause,
        Some(provenant_records::RouteExclusionReason::Exhausted)
    );
    // A typed cause persists only its bounded safe projection, never the
    // admission error's own text.
    assert!(reasons[0].detail.contains("allowance is exhausted"));
    assert!(!reasons[0].detail.contains("quota admission refused"));
    assert!(rejected.contains(&spent));
    Ok(())
}

#[test]
fn advisory_carve_out_keys_on_the_exact_pinned_candidate() {
    let primary = route("legacy", "spent");
    let alternative = route("other", "spent");
    let choice = provenant_routing::RouteChoice::Model(
        provenant_routing::ModelId::parse(&format!("{}/{}", primary.adapter, primary.model))
            .expect("model"),
    );
    assert!(pinned_candidate(&choice, &primary));
    assert!(!pinned_candidate(&choice, &alternative));
    // An exact account pin is attempted; a model-only pin redraws while
    // another account can serve the model, and is attempted when none can.
    assert!(pinned_advisory_candidate(
        &choice,
        &primary,
        Some("spent"),
        true
    ));
    assert!(pinned_advisory_candidate(
        &choice,
        &primary,
        Some("spent"),
        false
    ));
    assert!(!pinned_advisory_candidate(&choice, &primary, None, true));
    assert!(pinned_advisory_candidate(&choice, &primary, None, false));
    assert!(!pinned_advisory_candidate(
        &choice,
        &alternative,
        Some("spent"),
        true
    ));
}

// Spec: docs/specs/routing.md#unknown-never-means-ready
#[test]
fn advisory_redraw_defers_the_pinned_route_for_one_final_attempt() {
    use provenant_records::RouteExclusionReason;
    let pinned = route("legacy", "spent");
    let choice = provenant_routing::RouteChoice::Model(
        provenant_routing::ModelId::parse("cursor/legacy").expect("model"),
    );
    let deferred = deferred_pinned_attempt(
        &choice,
        &pinned,
        None,
        true,
        &RouteExclusionReason::Exhausted,
        Some("recorded exhaustion".into()),
    )
    .expect("a model-only pin with another account defers");
    assert_eq!(deferred.route.account_key.as_deref(), Some("spent"));
    // A non-advisory cause is never deferred: a missing login or a policy
    // refusal is not evidence that an attempt would succeed.
    assert!(
        deferred_pinned_attempt(
            &choice,
            &pinned,
            None,
            true,
            &RouteExclusionReason::AuthRequired,
            Some("warning".into())
        )
        .is_none()
    );
    assert!(
        deferred_pinned_attempt(
            &choice,
            &pinned,
            None,
            true,
            &RouteExclusionReason::ConfinementUnavailable,
            Some("warning".into())
        )
        .is_none()
    );
    // An exact account pin, a pin with no other account, and a pin with no
    // recorded source are attempted directly or cannot warn truthfully.
    assert!(
        deferred_pinned_attempt(
            &choice,
            &pinned,
            Some("spent"),
            true,
            &RouteExclusionReason::Exhausted,
            Some("warning".into())
        )
        .is_none()
    );
    assert!(
        deferred_pinned_attempt(
            &choice,
            &pinned,
            None,
            false,
            &RouteExclusionReason::Exhausted,
            Some("warning".into())
        )
        .is_none()
    );
    assert!(
        deferred_pinned_attempt(
            &choice,
            &pinned,
            None,
            true,
            &RouteExclusionReason::Exhausted,
            None
        )
        .is_none()
    );

    // A drained redraw re-queues exactly the refused pinned route with its
    // warning and makes its model tryable again.
    let mut queue = std::collections::VecDeque::new();
    let mut rejected =
        vec![provenant_routing::ModelId::parse("cursor/legacy").expect("recorded rejection")];
    assert_eq!(
        resume_deferred_pinned(&mut queue, deferred, &mut rejected),
        "recorded exhaustion"
    );
    assert_eq!(queue.len(), 1);
    assert!(rejected.is_empty());
    assert_eq!(queue[0].account_key.as_deref(), Some("spent"));
}

// Spec: docs/specs/routing.md#unknown-never-means-ready
#[test]
fn redraw_records_the_admission_block_cause() {
    use provenant_records::RouteExclusionReason;
    let blocked =
        |block| crate::route::EdgeError::Quota(provenant_quota::QuotaError::Blocked(block));
    assert_eq!(
        redraw_exclusion_reason(&blocked(provenant_quota::AccountBlock::ScopedRefusal), None),
        RouteExclusionReason::ProviderRefused
    );
    assert_eq!(
        redraw_exclusion_reason(
            &blocked(provenant_quota::AccountBlock::FreeTierRefusal),
            None
        ),
        RouteExclusionReason::FreeTierRefused
    );
    assert_eq!(
        redraw_exclusion_reason(&blocked(provenant_quota::AccountBlock::Mismatch), None),
        RouteExclusionReason::AccountMismatch
    );
    assert_eq!(
        redraw_exclusion_reason(
            &crate::route::EdgeError::Quota(provenant_quota::QuotaError::AllowanceExhausted),
            Some(&RouteExclusionReason::ProviderRefused),
        ),
        RouteExclusionReason::Exhausted
    );
    assert_eq!(
        redraw_exclusion_reason(
            &crate::route::EdgeError::Admission("unclassified".into()),
            Some(&RouteExclusionReason::ProviderRefused),
        ),
        RouteExclusionReason::ProviderRefused
    );
}

// Spec: docs/specs/routing.md#unknown-never-means-ready
#[test]
fn advisory_expiry_must_be_in_the_future() {
    let mut pinned = route("legacy", "spent");
    pinned.diagnostics.allowance_warning =
        Some(provenant_records::route_selection::AllowanceWarning {
            extra: Default::default(),
            warning: "allowance spent".into(),
            source: "quota:fixture".into(),
        });
    let availability = [provenant_providers::AvailabilityRecord {
        schema_version: 1,
        adapter: pinned.adapter.clone(),
        executable: "/fixture/cursor".into(),
        version: "fixture".into(),
        last_update: None,
        fixture_ref: "fixture".into(),
        catalogue_revision: "fixture".into(),
        account_key: pinned.account_key.clone(),
        plan: None,
        observed_models: vec![],
        observed_unix_seconds: 0,
        expires_unix_seconds: 3700,
    }];
    let reason = provenant_records::RouteExclusionReason::Exhausted;
    assert!(
        pinned_refusal_warning(
            &pinned,
            &availability,
            &[],
            &reason,
            3700,
            &provenant_quota::DEFAULT_FRESHNESS,
        )
        .is_none(),
        "an availability expiry at the current second is not in the future"
    );
    assert!(
        pinned_refusal_warning(
            &pinned,
            &availability,
            &[],
            &reason,
            4000,
            &provenant_quota::DEFAULT_FRESHNESS,
        )
        .is_none(),
        "an expired availability record is not evidence for an advisory launch"
    );
    assert!(
        pinned_refusal_warning(
            &pinned,
            &availability,
            &[],
            &reason,
            3699,
            &provenant_quota::DEFAULT_FRESHNESS,
        )
        .is_some()
    );
}

#[test]
fn alternative_account_must_serve_the_pinned_model() {
    let mut pinned = route("gemini-4-hydrogen", "spent");
    pinned.adapter = "agy".into();
    pinned.catalogue_revision = "revision".into();
    let policy = offer_policy(0);
    let catalogue = provenant_routing::Catalogue::bundled().unwrap();
    let record = |key: &str, models: &[&str]| provenant_providers::AvailabilityRecord {
        schema_version: 1,
        adapter: pinned.adapter.clone(),
        executable: "/fixture/agy".into(),
        version: "fixture".into(),
        last_update: None,
        fixture_ref: "fixture".into(),
        catalogue_revision: pinned.catalogue_revision.clone(),
        account_key: Some(key.into()),
        plan: None,
        observed_models: models.iter().map(|model| (*model).to_owned()).collect(),
        observed_unix_seconds: 0,
        expires_unix_seconds: 1,
    };
    let refused = std::collections::BTreeSet::new();
    assert!(!has_alternative_account(
        &pinned,
        &[],
        &refused,
        &policy,
        &catalogue
    ));
    assert!(!has_alternative_account(
        &pinned,
        &[record("spent", &["gemini-4-hydrogen"])],
        &refused,
        &policy,
        &catalogue,
    ));
    assert!(has_alternative_account(
        &pinned,
        &[record("other", &["gemini-4-hydrogen"])],
        &refused,
        &policy,
        &catalogue,
    ));
    assert!(!has_alternative_account(
        &pinned,
        &[record("other", &["gemini-4-argon"])],
        &refused,
        &policy,
        &catalogue,
    ));
    let mut refused = std::collections::BTreeSet::new();
    refused.insert(("agy".to_owned(), "other".to_owned()));
    assert!(!has_alternative_account(
        &pinned,
        &[record("other", &["gemini-4-hydrogen"])],
        &refused,
        &policy,
        &catalogue,
    ));
}

#[test]
fn alternative_account_uses_fresh_snapshot_offer_evidence() {
    let mut pinned = route("gemini-4-hydrogen", "spent");
    pinned.adapter = "agy".into();
    pinned.catalogue_revision = "revision".into();
    let availability = [provenant_providers::AvailabilityRecord {
        schema_version: 1,
        adapter: pinned.adapter.clone(),
        executable: "/fixture/agy".into(),
        version: "fixture".into(),
        last_update: None,
        fixture_ref: "fixture".into(),
        catalogue_revision: "revision".into(),
        account_key: Some("other".into()),
        plan: None,
        observed_models: Vec::new(),
        observed_unix_seconds: 90,
        expires_unix_seconds: 200,
    }];
    let scope = provenant_quota::AccountScope {
        extra: Default::default(),
        adapter: pinned.adapter.clone(),
        account_key: "other".into(),
        label: "other".into(),
    };
    let mut account = provenant_quota::AccountRecord::empty(scope.clone());
    account.revision = 1;
    account.snapshot = Some(provenant_quota::QuotaSnapshot {
        extra: Default::default(),
        schema_version: 1,
        account: scope,
        revision: 1,
        status: provenant_quota::QuotaStatus::Available,
        observed_unix_seconds: 80,
        source_ref: "fixture".into(),
        authority_ref: None,
        details: Some(provenant_quota::QuotaDetails {
            extra: Default::default(),
            models: vec!["gemini-4-hydrogen".into()],
            ..Default::default()
        }),
        windows: Vec::new(),
    });
    let accounts = [account];
    let policy = provenant_routing::OfferEvidencePolicy {
        accounts: &accounts,
        freshness: &provenant_quota::DEFAULT_FRESHNESS,
        quota_policies: &[],
        catalogue_revision: &pinned.catalogue_revision,
        now_seconds: 100,
    };
    let catalogue = provenant_routing::Catalogue::bundled().unwrap();
    assert!(has_alternative_account(
        &pinned,
        &availability,
        &std::collections::BTreeSet::new(),
        &policy,
        &catalogue,
    ));
}

#[test]
fn pinned_refusal_warning_names_the_record_source_and_expiry() {
    let mut pinned = route("cursor/legacy", "spent");
    pinned.diagnostics.allowance_warning =
        Some(provenant_records::route_selection::AllowanceWarning {
            extra: Default::default(),
            warning: "allowance spent".into(),
            source: "quota:fixture".into(),
        });
    // Without a quota record the expiry comes from the matching
    // availability record, never from a fabricated default.
    let availability = [provenant_providers::AvailabilityRecord {
        schema_version: 1,
        adapter: pinned.adapter.clone(),
        executable: "/fixture/cursor".into(),
        version: "fixture".into(),
        last_update: None,
        fixture_ref: "fixture".into(),
        catalogue_revision: "fixture".into(),
        account_key: pinned.account_key.clone(),
        plan: None,
        observed_models: vec![],
        observed_unix_seconds: 0,
        expires_unix_seconds: 3700,
    }];
    let (warning, expiry) = pinned_refusal_warning(
        &pinned,
        &availability,
        &[],
        &provenant_records::RouteExclusionReason::Exhausted,
        0,
        &provenant_quota::DEFAULT_FRESHNESS,
    )
    .expect("availability evidence");
    assert!(warning.contains("cursor/legacy"), "{warning}");
    assert!(warning.contains("quota:fixture"), "{warning}");
    assert!(warning.contains("expiry unix seconds: 3700"), "{warning}");
    assert_eq!(expiry, 3700);
    assert!(
        pinned_refusal_warning(
            &pinned,
            &[],
            &[],
            &provenant_records::RouteExclusionReason::Exhausted,
            0,
            &provenant_quota::DEFAULT_FRESHNESS,
        )
        .is_none(),
        "no record means no source or expiry to claim"
    );
}

#[test]
fn pinned_refusal_warning_reads_the_exhaustion_record() {
    let pinned = route("legacy", "spent");
    let scope = provenant_quota::AccountScope {
        extra: Default::default(),
        adapter: pinned.adapter.clone(),
        account_key: "spent".into(),
        label: "pro".into(),
    };
    let expected_source = format!("quota.provider.{}", pinned.adapter);
    let mut record = provenant_quota::AccountRecord::empty(scope.clone());
    record.revision = 1;
    record.snapshot = Some(provenant_quota::QuotaSnapshot {
        extra: Default::default(),
        schema_version: 1,
        account: scope,
        revision: 1,
        status: provenant_quota::QuotaStatus::Exhausted,
        observed_unix_seconds: 1000,
        source_ref: expected_source.clone(),
        authority_ref: None,
        windows: vec![provenant_quota::QuotaWindow {
            extra: Default::default(),
            pool: "five_hour".into(),
            window_seconds: Some(18_000),
            used_basis_points: 10_000,
            resets_unix_seconds: Some(10_000),
        }],
        details: None,
    });
    let (warning, expiry) = pinned_refusal_warning(
        &pinned,
        &[],
        std::slice::from_ref(&record),
        &provenant_records::RouteExclusionReason::Exhausted,
        0,
        &provenant_quota::DEFAULT_FRESHNESS,
    )
    .expect("record evidence");
    assert!(warning.contains(&expected_source), "{warning}");
    assert!(!warning.contains("quota allowance on"), "{warning}");
    assert!(warning.contains("expiry unix seconds: 10000"), "{warning}");
    assert_eq!(expiry, 10_000);
}

#[test]
fn pinned_refusal_warning_reads_the_provider_refusal_record() {
    let pinned = route("legacy", "spent");
    let scope = provenant_quota::AccountScope {
        extra: Default::default(),
        adapter: pinned.adapter.clone(),
        account_key: "spent".into(),
        label: "pro".into(),
    };
    let mut record = provenant_quota::AccountRecord::empty(scope.clone());
    record.revision = 1;
    record.refusals.push(provenant_quota::Refusal {
        extra: Default::default(),
        account: scope,
        model: pinned.model.clone(),
        pool: None,
        observed_unix_seconds: 2_000,
        resets_unix_seconds: Some(9_000),
    });
    let reason = provenant_records::RouteExclusionReason::ProviderRefused;
    let (warning, expiry) = pinned_refusal_warning(
        &pinned,
        &[],
        std::slice::from_ref(&record),
        &reason,
        3_000,
        &provenant_quota::DEFAULT_FRESHNESS,
    )
    .expect("refusal evidence");
    assert!(warning.contains("cursor/legacy"), "{warning}");
    assert!(
        warning.contains("source: provider refusal observed unix seconds 2000"),
        "{warning}"
    );
    assert!(warning.contains("expiry unix seconds: 9000"), "{warning}");
    assert_eq!(expiry, 9_000);
    assert!(
        pinned_refusal_warning(
            &pinned,
            &[],
            std::slice::from_ref(&record),
            &reason,
            9_000,
            &provenant_quota::DEFAULT_FRESHNESS
        )
        .is_none(),
        "an expired refusal is not evidence for an advisory launch"
    );
}

#[test]
fn persisted_provider_refusal_feeds_the_advisory_warning() -> Result<(), Box<dyn std::error::Error>>
{
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir()?;
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))?;
    crate::quota::persist_provider_refusal(
        temp.path(),
        &provenant_records::run::RouteRecord {
            provider: Some("cursor".into()),
            model: Some("cursor/legacy".into()),
            account_key: Some("spent".into()),
            account_label: Some("pro".into()),
            ..Default::default()
        },
        None,
        5_000,
        Some(9_000),
    )?;
    let records = crate::quota::read_cache_at(temp.path())?;
    let pinned = route("legacy", "spent");
    let (warning, expiry) = pinned_refusal_warning(
        &pinned,
        &[],
        &records,
        &provenant_records::RouteExclusionReason::ProviderRefused,
        5_001,
        &provenant_quota::DEFAULT_FRESHNESS,
    )
    .expect("persisted refusal evidence");
    assert!(
        warning.contains("source: provider refusal observed unix seconds 5000"),
        "{warning}"
    );
    assert!(warning.contains("expiry unix seconds: 9000"), "{warning}");
    assert_eq!(expiry, 9_000);
    Ok(())
}
