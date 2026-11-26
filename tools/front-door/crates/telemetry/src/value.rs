//! Per-window value metrics: unused share at reset, trend and credit runway.
//!
//! The ledger owns cost and quota history owns readings. This module joins
//! them without inventing a number: a metric whose inputs are missing carries
//! a named unavailable reason, an unpriced bucket is a lower bound, and a
//! measured zero is the only zero that is reported.

use serde::Serialize;
use std::cmp::Ordering;

/// One quota reading in observation order, oldest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reading {
    pub observed_unix_seconds: i64,
    /// Absent when a quota snapshot did not include this pool.
    pub used_basis_points: Option<u64>,
    /// A reset was detected at this reading: usage fell or the reset advanced.
    pub reset: bool,
}

/// One credit-balance reading in observation order, oldest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CreditPoint {
    pub observed_unix_seconds: i64,
    /// Absent when the provider reported a non-numeric balance (`unlimited`).
    pub balance_microdollars: Option<u64>,
}

/// Named reason a value field has no number. One stable vocabulary for the
/// human line and the JSON field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unavailable {
    NoResetObserved,
    NoReadingBeforeReset,
    MissingReadingBeforeReset,
    NotEnoughReadings,
    NoCreditBalance,
    NoElapsedTime,
    CreditNotDrawn,
    Unpriced,
    NoUsage,
    NoPriceInForce,
    UnsupportedCurrency,
    IncompleteWindow,
}

impl Unavailable {
    /// The serialised name, shared by the JSON field and the human line.
    pub const fn name(self) -> &'static str {
        match self {
            Self::NoResetObserved => "no_reset_observed",
            Self::NoReadingBeforeReset => "no_reading_before_reset",
            Self::MissingReadingBeforeReset => "missing_reading_before_reset",
            Self::NotEnoughReadings => "not_enough_readings",
            Self::NoCreditBalance => "no_credit_balance",
            Self::NoElapsedTime => "no_elapsed_time",
            Self::CreditNotDrawn => "credit_not_drawn",
            Self::Unpriced => "unpriced",
            Self::NoUsage => "no_usage",
            Self::NoPriceInForce => "no_price_in_force",
            Self::UnsupportedCurrency => "unsupported_currency",
            Self::IncompleteWindow => "incomplete_window",
        }
    }
}

/// An unused share is the last reading before the reset, not a measurement
/// taken at the reset instant.
pub const OBSERVED_BEFORE_RESET: &str = "observed_before_reset";

/// Share of a quota window left unused at its last reset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UnusedShare {
    pub basis: &'static str,
    /// Basis points of the window unused at the last reading before the reset.
    pub unused_basis_points: Option<u64>,
    /// Usage at that reading, in basis points.
    pub used_basis_points: Option<u64>,
    pub observed_unix_seconds: Option<i64>,
    /// Readings in the completed cycle, up to but not including the reset.
    pub samples: u64,
    /// True when the window reset with less than half of it used.
    pub below_half_used: bool,
    pub unavailable: Option<Unavailable>,
}

fn share_unavailable(reason: Unavailable, samples: u64, observed: Option<i64>) -> UnusedShare {
    UnusedShare {
        basis: OBSERVED_BEFORE_RESET,
        unused_basis_points: None,
        used_basis_points: None,
        observed_unix_seconds: observed,
        samples,
        below_half_used: false,
        unavailable: Some(reason),
    }
}

/// The most recent completed cycle's unused share. `None` only for a series
/// with no readings at all; every other gap gets a named reason.
pub fn unused_at_reset(readings: &[Reading]) -> Option<UnusedShare> {
    let first = readings.first()?;
    let Some(at) = readings.iter().rposition(|reading| reading.reset) else {
        return Some(share_unavailable(
            Unavailable::NoResetObserved,
            readings.len() as u64,
            Some(first.observed_unix_seconds),
        ));
    };
    let start = readings[..at]
        .iter()
        .rposition(|reading| reading.reset)
        .map_or(0, |index| index + 1);
    let samples = readings[start..at]
        .iter()
        .filter(|reading| reading.used_basis_points.is_some())
        .count() as u64;
    if samples == 0 {
        return Some(share_unavailable(
            Unavailable::NoReadingBeforeReset,
            samples,
            Some(readings[at].observed_unix_seconds),
        ));
    }
    let before = readings[at - 1];
    let Some(used) = before.used_basis_points else {
        return Some(share_unavailable(
            Unavailable::MissingReadingBeforeReset,
            samples,
            Some(before.observed_unix_seconds),
        ));
    };
    let used = used.min(10_000);
    Some(UnusedShare {
        basis: OBSERVED_BEFORE_RESET,
        unused_basis_points: Some(10_000 - used),
        used_basis_points: Some(used),
        observed_unix_seconds: Some(before.observed_unix_seconds),
        samples,
        below_half_used: used < 5_000,
        unavailable: None,
    })
}

/// Direction of a value across completed windows, in basis points of change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Trend {
    pub direction: Option<&'static str>,
    pub change_basis_points: Option<i64>,
    /// Windows compared, not ledger attempts.
    pub samples: u64,
    pub unavailable: Option<Unavailable>,
}

/// Trend from the first to the last observed window value. One window is not
/// a trend and is named unavailable instead of being called flat.
pub fn trend(values: &[u64]) -> Trend {
    let samples = values.len() as u64;
    if values.len() < 2 {
        return Trend {
            direction: None,
            change_basis_points: None,
            samples,
            unavailable: Some(Unavailable::NotEnoughReadings),
        };
    }
    let change = (i128::from(values[values.len() - 1]) - i128::from(values[0]))
        .clamp(i64::MIN as i128, i64::MAX as i128) as i64;
    let direction = match change.cmp(&0) {
        Ordering::Greater => "up",
        Ordering::Less => "down",
        Ordering::Equal => "flat",
    };
    Trend {
        direction: Some(direction),
        change_basis_points: Some(change),
        samples,
        unavailable: None,
    }
}

/// A full-window value figure and the plan price it is compared against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WindowValue {
    /// API-equivalent cost of 100% of the window, in microdollars.
    pub cost_per_full_window_microdollars: Option<u64>,
    /// The same figure against the plan price in force, in basis points.
    pub value_multiple_basis_points: Option<u64>,
    /// Scaled cost when some usage was unpriced: a bound, never a value.
    pub lower_bound_microdollars: Option<u64>,
    pub used_basis_points: u64,
    pub unavailable: Option<Unavailable>,
}

fn scaled(value: u64, numerator: u128, denominator: u64) -> u64 {
    if denominator == 0 {
        return value;
    }
    u64::try_from(u128::from(value) * numerator / u128::from(denominator)).unwrap_or(u64::MAX)
}

/// Scale observed cost to a full window and compare it with that window's
/// share of the plan price. Missing price or usage stays named, never zero.
pub fn window_value(
    cost_microdollars: Option<u64>,
    used_basis_points: u64,
    price_per_window_microdollars: Option<u64>,
    complete: bool,
    full_window: bool,
) -> WindowValue {
    let used = used_basis_points.min(10_000);
    let mut result = WindowValue {
        cost_per_full_window_microdollars: None,
        value_multiple_basis_points: None,
        lower_bound_microdollars: None,
        used_basis_points: used,
        unavailable: None,
    };
    if !full_window {
        result.unavailable = Some(Unavailable::IncompleteWindow);
        return result;
    }
    let cost = match cost_microdollars {
        Some(cost) => cost,
        None => {
            result.unavailable = Some(Unavailable::Unpriced);
            return result;
        }
    };
    if !complete {
        if used > 0 {
            result.lower_bound_microdollars = Some(scaled(cost, 10_000, used));
        }
        result.unavailable = Some(Unavailable::Unpriced);
        return result;
    }
    if used == 0 {
        result.unavailable = Some(Unavailable::NoUsage);
        return result;
    }
    let full = scaled(cost, 10_000, used);
    result.cost_per_full_window_microdollars = Some(full);
    match price_per_window_microdollars {
        Some(price) if price > 0 => {
            result.value_multiple_basis_points = Some(scaled(full, 10_000, price));
        }
        _ => result.unavailable = Some(Unavailable::NoPriceInForce),
    }
    result
}

/// Credit draw-down rate and days to exhaustion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CreditRunway {
    /// Credit used per day in microdollars of the credit currency, rounded up.
    pub draw_down_microdollars_per_day: Option<u64>,
    pub days_to_exhaustion: Option<u64>,
    /// Numeric balance readings compared.
    pub samples: u64,
    pub unavailable: Option<Unavailable>,
}

fn runway_unavailable(reason: Unavailable, samples: u64) -> CreditRunway {
    CreditRunway {
        draw_down_microdollars_per_day: None,
        days_to_exhaustion: None,
        samples,
        unavailable: Some(reason),
    }
}

/// Draw-down between the first and last numeric balance. A balance that did
/// not fall is not a zero-rate runway; it is an unavailable field.
pub fn credit_runway(points: &[CreditPoint]) -> CreditRunway {
    let pairs: Vec<(i64, u64)> = points
        .iter()
        .filter_map(|point| {
            point
                .balance_microdollars
                .map(|balance| (point.observed_unix_seconds, balance))
        })
        .collect();
    let samples = pairs.len() as u64;
    if pairs.is_empty() {
        return runway_unavailable(Unavailable::NoCreditBalance, samples);
    }
    if pairs.len() < 2 {
        return runway_unavailable(Unavailable::NotEnoughReadings, samples);
    }
    let (first_at, first_balance) = pairs[0];
    let (last_at, last_balance) = pairs[pairs.len() - 1];
    if last_balance >= first_balance {
        return runway_unavailable(Unavailable::CreditNotDrawn, samples);
    }
    // Elapsed time is a duration, so convert before the rate arithmetic: a
    // negative or zero span is named unavailable, never a zero-day division.
    let seconds = u64::try_from(last_at.saturating_sub(first_at)).unwrap_or(0);
    if seconds == 0 {
        return runway_unavailable(Unavailable::NoElapsedTime, samples);
    }
    let drawn = first_balance - last_balance;
    let per_day = (u128::from(drawn) * 86_400).div_ceil(u128::from(seconds));
    let per_day = u64::try_from(per_day).unwrap_or(u64::MAX);
    CreditRunway {
        draw_down_microdollars_per_day: Some(per_day),
        days_to_exhaustion: Some(last_balance / per_day.max(1)),
        samples,
        unavailable: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(at: i64, used: Option<u64>, reset: bool) -> Reading {
        Reading {
            observed_unix_seconds: at,
            used_basis_points: used,
            reset,
        }
    }

    #[test]
    fn unused_share_is_the_last_reading_of_the_completed_cycle() {
        let readings = [
            reading(1_000, Some(9_000), false),
            reading(2_000, Some(500), true), // first cycle reset after one reading
            reading(2_500, Some(2_000), false), // last cycle's first reading
            reading(3_000, Some(4_000), false), // last reading before the reset sets the share
            reading(4_000, Some(2_000), true),
        ];
        let share = unused_at_reset(&readings).expect("a series with readings");
        assert_eq!(share.basis, OBSERVED_BEFORE_RESET);
        assert_eq!(share.used_basis_points, Some(4_000));
        assert_eq!(share.unused_basis_points, Some(6_000));
        assert_eq!(share.observed_unix_seconds, Some(3_000));
        assert_eq!(share.samples, 2);
        assert!(share.below_half_used);
        assert_eq!(share.unavailable, None);
    }

    #[test]
    fn a_reset_without_a_reading_before_it_is_named_not_zero() {
        let share = unused_at_reset(&[reading(1_000, Some(500), true)]).expect("a reading");
        assert_eq!(share.unavailable, Some(Unavailable::NoReadingBeforeReset));
        assert_eq!(share.unused_basis_points, None);
        assert_eq!(share.observed_unix_seconds, Some(1_000));
    }

    #[test]
    fn a_series_with_no_reset_is_named_not_zero() {
        let share = unused_at_reset(&[
            reading(1_000, Some(100), false),
            reading(2_000, Some(200), false),
        ])
        .expect("readings");
        assert_eq!(share.unavailable, Some(Unavailable::NoResetObserved));
        assert_eq!(share.unused_basis_points, None);
        assert_eq!(share.used_basis_points, None);
    }

    #[test]
    fn missing_pool_reading_before_reset_is_not_zero_filled() {
        let share = unused_at_reset(&[
            reading(1_000, Some(4_000), false),
            reading(2_000, None, false),
            reading(3_000, Some(500), true),
        ])
        .expect("the reset marker is present");
        assert_eq!(
            share.unavailable,
            Some(Unavailable::MissingReadingBeforeReset)
        );
        assert_eq!(share.unused_basis_points, None);
        assert_eq!(share.observed_unix_seconds, Some(2_000));
        assert_eq!(share.samples, 1);
    }

    #[test]
    fn trend_needs_two_windows_and_states_its_direction() {
        assert_eq!(
            trend(&[1_000]).unavailable,
            Some(Unavailable::NotEnoughReadings)
        );
        let falling = trend(&[8_000, 5_000, 2_000]);
        assert_eq!(falling.direction, Some("down"));
        assert_eq!(falling.change_basis_points, Some(-6_000));
        assert_eq!(falling.samples, 3);
        let rising = trend(&[2_000, 8_000]);
        assert_eq!(rising.direction, Some("up"));
        assert_eq!(trend(&[3_000, 3_000]).direction, Some("flat"));
    }

    #[test]
    fn a_full_window_is_scaled_once_and_an_unpriced_bucket_is_a_bound() {
        let priced = window_value(Some(1_000_000), 2_500, Some(2_000_000), true, true);
        assert_eq!(priced.cost_per_full_window_microdollars, Some(4_000_000));
        assert_eq!(priced.value_multiple_basis_points, Some(20_000));
        assert_eq!(priced.unavailable, None);

        let partial = window_value(Some(1_000_000), 2_500, Some(2_000_000), false, true);
        assert_eq!(partial.cost_per_full_window_microdollars, None);
        assert_eq!(partial.lower_bound_microdollars, Some(4_000_000));
        assert_eq!(partial.unavailable, Some(Unavailable::Unpriced));

        assert_eq!(
            window_value(None, 0, None, true, true).unavailable,
            Some(Unavailable::Unpriced)
        );
        assert_eq!(
            window_value(Some(1), 0, Some(1), true, true).unavailable,
            Some(Unavailable::NoUsage)
        );
        assert_eq!(
            window_value(Some(1), 100, None, true, true).unavailable,
            Some(Unavailable::NoPriceInForce)
        );
    }

    #[test]
    fn since_limited_cost_cannot_be_scaled_as_a_full_window() {
        let partial = window_value(Some(1_000_000), 2_500, Some(2_000_000), true, false);
        assert_eq!(partial.cost_per_full_window_microdollars, None);
        assert_eq!(partial.value_multiple_basis_points, None);
        assert_eq!(partial.unavailable, Some(Unavailable::IncompleteWindow));
    }

    #[test]
    fn a_credit_runway_needs_positive_elapsed_time() {
        let same_instant = [
            CreditPoint {
                observed_unix_seconds: 100,
                balance_microdollars: Some(9_000),
            },
            CreditPoint {
                observed_unix_seconds: 100,
                balance_microdollars: Some(5_000),
            },
        ];
        assert_eq!(
            credit_runway(&same_instant).unavailable,
            Some(Unavailable::NoElapsedTime)
        );
        let backwards = [
            CreditPoint {
                observed_unix_seconds: 200,
                balance_microdollars: Some(9_000),
            },
            CreditPoint {
                observed_unix_seconds: 100,
                balance_microdollars: Some(5_000),
            },
        ];
        assert_eq!(
            credit_runway(&backwards).unavailable,
            Some(Unavailable::NoElapsedTime)
        );
    }

    #[test]
    fn credit_runway_reports_draw_down_and_exhaustion_or_a_named_reason() {
        let points = [
            CreditPoint {
                observed_unix_seconds: 0,
                balance_microdollars: Some(9_000_000),
            },
            CreditPoint {
                observed_unix_seconds: 86_400,
                balance_microdollars: None, // "unlimited" never counts
            },
            CreditPoint {
                observed_unix_seconds: 2 * 86_400,
                balance_microdollars: Some(5_000_000),
            },
        ];
        let runway = credit_runway(&points);
        assert_eq!(runway.samples, 2);
        assert_eq!(runway.draw_down_microdollars_per_day, Some(2_000_000));
        assert_eq!(runway.days_to_exhaustion, Some(2));
        assert_eq!(runway.unavailable, None);

        let one = &points[..1];
        assert_eq!(
            credit_runway(one).unavailable,
            Some(Unavailable::NotEnoughReadings)
        );
        let none = [CreditPoint {
            observed_unix_seconds: 0,
            balance_microdollars: None,
        }];
        assert_eq!(
            credit_runway(&none).unavailable,
            Some(Unavailable::NoCreditBalance)
        );
        let rising = [
            CreditPoint {
                observed_unix_seconds: 0,
                balance_microdollars: Some(1_000),
            },
            CreditPoint {
                observed_unix_seconds: 86_400,
                balance_microdollars: Some(2_000),
            },
        ];
        assert_eq!(
            credit_runway(&rising).unavailable,
            Some(Unavailable::CreditNotDrawn)
        );
    }
}
