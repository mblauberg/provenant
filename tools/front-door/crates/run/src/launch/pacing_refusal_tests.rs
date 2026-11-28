use super::*;

#[cfg(test)]
#[test]
fn pacing_refusal_retains_the_selected_reserve_fact() {
    use provenant_records::diagnostic::{PacingReserveSetting, ResourceFact};
    let fact = ResourceFact::PacingReserve {
        percent: 15,
        setting: PacingReserveSetting::AdapterFiveHour,
        origin: Layer::Default,
        hold: Box::new(provenant_records::diagnostic::PacingHold {
            account: "codex/opaque-account".into(),
            window: "five_hour".into(),
            setting_path: "domains.routing.pacing_reserve.codex.five_hour_percent".into(),
        }),
    };
    let mut skipped = SkippedRoute {
        skipped_accounts: Vec::new(),
        extra: Default::default(),
        route: "fixture/model".into(),
        kind: SkipKind::Unavailable,
        cause: Some(provenant_records::RouteExclusionReason::PacingReserve),
        detail: "opaque source".into(),
        ..Default::default()
    };
    skipped.set_guard_facts(None, Some(fact.clone())).unwrap();
    let mut refusal = PrepareRefusal::boxed(CauseKind::RouteResolutionRefused, "opaque source");
    refusal.skipped_routes.push(skipped);
    let kind = refusal.kind();
    assert!(matches!(&kind, RefusalKind::PacingReserveResource(value) if value == &fact));
    let diagnostic = kind.diagnostic().unwrap();
    assert_eq!(diagnostic.code, "REFUSED");
    let detail = diagnostic.detail.unwrap();
    assert!(detail.contains("domains.routing.pacing_reserve"));
    assert!(detail.contains("shipped default"));
    assert!(!detail.contains("opaque source"));
}
