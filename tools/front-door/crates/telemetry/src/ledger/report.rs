//! Cost over usage ledger rows: the one producer behind `telemetry cost` and
//! the usage half of `telemetry value`.
//!
//! A figure that is not known is absent, never zero. An attempt the price
//! table could not fully price is counted as unpriced and the cost becomes a
//! lower bound. An attempt with no account is reported under the gap group
//! `unattributed`, not folded into another account.
use super::Row;
use crate::{TelemetryError, reducer::Money};
use provenant_records::AccountIdentity;
use provenant_records::AccountIdentity;
use serde::Serialize;
use std::collections::BTreeMap;

/// The key of a group whose rows lack the field it groups by.
pub const GAP: &str = "unattributed";

/// What `telemetry cost --by` groups on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum By {
    Route,
    Account,
    Model,
    Graph,
    Node,
}

/// Counts and cost over some rows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Totals {
    pub attempts: u64,
    /// Attempts whose cost is fully known.
    pub priced_attempts: u64,
    /// Attempts with no cost or only part of one: the cost is then a lower bound.
    pub unpriced_attempts: u64,
    /// Attempts the table marks as subscription-billed.
    pub subscription_attempts: u64,
    /// Attempts that carry no account.
    pub unattributed_attempts: u64,
    /// Attempts whose provider left a token category unreported.
    pub unreported_token_attempts: u64,
    /// Known cost in whole microdollars; absent when no attempt priced anything.
    pub microdollars: Option<u64>,
    /// `microdollars` as dollars with six decimals.
    pub usd: Option<String>,
    /// True when every attempt is fully priced and there is at least one.
    pub complete: bool,
}

/// One group of a breakdown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Group {
    pub key: String,
    #[serde(flatten)]
    pub totals: Totals,
}

fn unreported(row: &Row) -> bool {
    let t = &row.tokens;
    [
        t.uncached_input,
        t.cache_read,
        t.cache_write,
        t.output_excluding_reasoning,
        t.reasoning,
    ]
    .iter()
    .any(Option::is_none)
}

/// Total some rows, rounding the cost once.
pub fn totals<'a>(rows: impl IntoIterator<Item = &'a Row>) -> Result<Totals, TelemetryError> {
    let mut totals = Totals::default();
    let (mut numerator, mut any) = (0u128, false);
    for row in rows {
        totals.attempts += 1;
        totals.unattributed_attempts += u64::from(row.identity.is_unknown());
        totals.unreported_token_attempts += u64::from(unreported(row));
        match &row.cost {
            Some(cost) => {
                totals.subscription_attempts += u64::from(cost.subscription);
                if cost.complete || cost.picodollars > 0 {
                    any = true;
                    numerator = numerator
                        .checked_add(u128::from(cost.picodollars))
                        .ok_or(TelemetryError::Overflow)?;
                }
                if cost.complete {
                    totals.priced_attempts += 1;
                } else {
                    totals.unpriced_attempts += 1;
                }
            }
            None => totals.unpriced_attempts += 1,
        }
    }
    if any {
        let money = Money::from_numerator(numerator)?;
        totals.microdollars = Some(money.microdollars);
        totals.usd = Some(money.usd);
    }
    totals.complete = totals.attempts > 0 && totals.unpriced_attempts == 0;
    Ok(totals)
}

fn key(row: &Row, by: By) -> String {
    match by {
        By::Account => row.identity.account_key(),
        By::Model => some(&row.model),
        By::Graph => row.graph.clone(),
        By::Node => format!("{}/{}", row.graph, row.node),
        By::Route => match (&row.provider, &row.model) {
            (Some(provider), Some(model)) => match &row.effort {
                Some(effort) => format!("{provider}/{model}/{effort}"),
                None => format!("{provider}/{model}"),
            },
            _ => GAP.to_owned(),
        },
    }
}

/// Total the rows by `by`, groups in key order.
pub fn group<'a>(
    rows: impl IntoIterator<Item = &'a Row>,
    by: By,
) -> Result<Vec<Group>, TelemetryError> {
    let mut groups = BTreeMap::<String, Vec<&Row>>::new();
    for row in rows {
        groups.entry(key(row, by)).or_default().push(row);
    }
    groups
        .into_iter()
        .map(|(key, rows)| {
            Ok(Group {
                key,
                totals: totals(rows)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{Cost, FORMAT, Source, Tokens};
    use provenant_core::{ProjectId, WallTime};

    fn row(attempt: &str, account: Option<&str>, cost: Option<(u64, bool)>) -> Row {
        let token = Some(1);
        Row {
            format: FORMAT.into(),
            source: Source::Settlement,
            project: ProjectId::from_bytes([1; 16]),
            graph: "graph".into(),
            node: "node".into(),
            attempt: attempt.into(),
            provider: Some("claude".into()),
            model: Some("m".into()),
            effort: None,
            identity: account.map_or_else(AccountIdentity::unknown, |key| {
                AccountIdentity::known("claude", key, "fixture-epoch")
            }),
            outcome: "succeeded".into(),
            started: None,
            finished: WallTime::from_micros(0).expect("epoch"),
            tokens: Tokens {
                uncached_input: token,
                cache_read: token,
                cache_write: token,
                cache_write_1h: None,
                output_excluding_reasoning: token,
                reasoning: token,
            },
            cost: cost.map(|(picodollars, complete)| Cost {
                picodollars,
                complete,
                subscription: false,
                prices: "p".into(),
            }),
            context_band: None,
        }
    }

    #[test]
    fn an_unpriced_attempt_makes_the_cost_a_bound_and_none_priced_is_not_zero() {
        let rows = [
            row("a", Some("work"), Some((2_000_000_000_000, true))),
            row("b", Some("work"), None),
        ];
        let total = totals(&rows).expect("totals");
        assert_eq!(total.usd.as_deref(), Some("2.000000"));
        assert_eq!((total.priced_attempts, total.unpriced_attempts), (1, 1));
        assert!(!total.complete);
        let none = totals(&rows[1..]).expect("totals");
        assert_eq!((none.microdollars, none.usd), (None, None));
        assert!(!none.complete);
        assert_eq!(totals(&[]).expect("totals").usd, None);
    }

    #[test]
    fn rows_without_an_account_form_their_own_group() {
        let rows = [
            row("a", Some("work"), Some((1_000_000, true))),
            row("b", None, Some((3_000_000, true))),
        ];
        let groups = group(&rows, By::Account).expect("groups");
        let keys: Vec<_> = groups.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(keys, ["claude/work", GAP]);
        assert_eq!(groups[0].totals.unattributed_attempts, 0);
        assert_eq!(groups[1].totals.unattributed_attempts, 1);
        assert_eq!(groups[0].totals.unreported_token_attempts, 0);
    }
}
