//! Retain typed metadata alongside clap rules that have no public getters.
use clap::{
    Arg,
    builder::{ArgExt, CommandExt},
};
use std::ops::{Bound, RangeBounds};

#[derive(Clone, Debug)]
pub(crate) struct NumericBounds(pub (u64, u64));
impl ArgExt for NumericBounds {}
#[derive(Clone, Debug)]
pub(crate) struct PublishedBounds(pub (u64, u64));
impl ArgExt for PublishedBounds {}
#[derive(Clone, Debug, Default)]
pub(crate) struct Requirements(pub Vec<String>);
impl ArgExt for Requirements {}

pub(crate) trait CatalogueArg {
    fn catalog_bounds(self, bounds: (u64, u64)) -> Self;
    fn catalog_u8_range(self, range: impl RangeBounds<i64>) -> Self;
    fn catalog_duration(self) -> Self;
    fn catalog_observation_wait(self) -> Self;
    fn catalog_u64_range(self, range: impl RangeBounds<u64>) -> Self;
    fn catalog_u32_range(self, range: impl RangeBounds<i64>) -> Self;
    fn catalog_i64_range(self, range: impl RangeBounds<i64>) -> Self;
    fn catalog_requires(self, id: &'static str) -> Self;
    fn catalog_requires_all(self, ids: impl IntoIterator<Item = &'static str>) -> Self;
}
fn unsigned_bounds(range: &impl RangeBounds<u64>) -> (u64, u64) {
    let min = match range.start_bound() {
        Bound::Included(n) => *n,
        Bound::Excluded(n) => n.saturating_add(1),
        Bound::Unbounded => u64::MIN,
    };
    let max = match range.end_bound() {
        Bound::Included(n) => *n,
        Bound::Excluded(n) => n.saturating_sub(1),
        Bound::Unbounded => u64::MAX,
    };
    (min, max)
}
fn signed_bounds(range: &impl RangeBounds<i64>, max: u64) -> (u64, u64) {
    // The published ranges are non-negative; the records contract uses u64.
    let min = match range.start_bound() {
        Bound::Included(n) => (*n).max(0) as u64,
        Bound::Excluded(n) => n.saturating_add(1).max(0) as u64,
        Bound::Unbounded => 0,
    };
    let max = match range.end_bound() {
        Bound::Included(n) => (*n).max(0) as u64,
        Bound::Excluded(n) => n.saturating_sub(1).max(0) as u64,
        Bound::Unbounded => max,
    };
    (min, max)
}
impl CatalogueArg for Arg {
    fn catalog_bounds(self, bounds: (u64, u64)) -> Self {
        self.add(PublishedBounds(bounds))
    }
    fn catalog_u8_range(self, range: impl RangeBounds<i64>) -> Self {
        let bounds = signed_bounds(&range, u8::MAX.into());
        self.value_parser(clap::value_parser!(u8).range(range))
            .add(NumericBounds(bounds))
    }
    fn catalog_duration(self) -> Self {
        self.value_parser(crate::grammar::duration_seconds)
            .catalog_bounds((1, provenant_core::MAX_LANE_RUNTIME_SECONDS))
    }
    fn catalog_observation_wait(self) -> Self {
        self.value_parser(crate::grammar::observation_wait_seconds)
            .add(NumericBounds((0, provenant_core::MAX_LANE_RUNTIME_SECONDS)))
    }
    fn catalog_u64_range(self, range: impl RangeBounds<u64>) -> Self {
        let bounds = unsigned_bounds(&range);
        self.value_parser(clap::value_parser!(u64).range(range))
            .add(NumericBounds(bounds))
    }
    fn catalog_u32_range(self, range: impl RangeBounds<i64>) -> Self {
        let bounds = signed_bounds(&range, u32::MAX.into());
        self.value_parser(clap::value_parser!(u32).range(range))
            .add(NumericBounds(bounds))
    }
    fn catalog_i64_range(self, range: impl RangeBounds<i64>) -> Self {
        let bounds = signed_bounds(&range, i64::MAX as u64);
        self.value_parser(clap::value_parser!(i64).range(range))
            .add(NumericBounds(bounds))
    }
    fn catalog_requires(self, id: &'static str) -> Self {
        self.catalog_requires_all([id])
    }
    fn catalog_requires_all(self, ids: impl IntoIterator<Item = &'static str>) -> Self {
        let mut requirements = self.get::<Requirements>().cloned().unwrap_or_default();
        let ids: Vec<_> = ids.into_iter().collect();
        requirements.0.extend(ids.iter().map(|id| (*id).to_owned()));
        self.requires_all(ids).add(requirements)
    }
}

/// Compact-help marker and example; descriptions remain clap's `about` text.
#[derive(Clone, Debug)]
pub(crate) struct CoreCommand(pub usize, pub &'static str);
impl CommandExt for CoreCommand {}
