//! A dispatch profile is sealed into canonical run records, which carry no
//! floats (rust-workspace.md, "Numbers, digests and diagnostics").
#![allow(clippy::unwrap_used)]
use super::*;
use provenant_core::{PathResolver, RootInputs, Roots};
use provenant_records::{BudgetGrace, BudgetPolicy, CanonicalV1};
use std::{io, path::Path};

struct Resolver;
impl PathResolver for Resolver {
    fn resolve_existing_ancestor(&self, path: &Path) -> io::Result<PathBuf> {
        Ok(path.to_path_buf())
    }
}

#[test]
fn every_dispatch_profile_seals_canonically_and_keeps_its_grace() {
    let mut inputs = RootInputs::new("/fixture/home".into(), "/fixture/checkout".into());
    inputs.state_dir = Some("/fixture/state".into());
    let roots: Roots = inputs.resolve(&Resolver).unwrap();
    let config = load::<&[u8]>(&roots, None).unwrap();
    for (overrides, grace) in [
        (vec![], BudgetGrace::DEFAULT),
        (
            vec!["lanes.budget_grace=0.25".to_owned()],
            BudgetGrace::from_fraction(0.25).unwrap(),
        ),
    ] {
        let profile = config.dispatch_profile(&overrides).unwrap();
        CanonicalV1::bytes(&profile).unwrap();
        let sealed: BudgetGrace =
            serde_json::from_value(profile.knobs["lanes.budget_grace"].clone()).unwrap();
        assert_eq!(sealed, grace);
        let policy = BudgetPolicy {
            grace,
            ..BudgetPolicy::default()
        };
        CanonicalV1::bytes(&policy).unwrap();
        let read: BudgetPolicy =
            serde_json::from_slice(&serde_json::to_vec(&policy).unwrap()).unwrap();
        assert_eq!(read, policy);
    }
}
