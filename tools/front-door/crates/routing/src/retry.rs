//! Pure prerequisites for resolving infrastructure successors.

use crate::DEFAULT_SETTLEMENT_RETRIES;

/// Source of the effective infrastructure-successor allowance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryAllowanceOrigin {
    /// The node's `retry.retries`.
    Node,
    /// The preset's `retry.retries`.
    Preset,
    /// An explicitly configured `[domains.routing] settlement_retries`.
    HostConfig,
    /// The shipped default.
    ShippedDefault,
}

/// Effective cumulative infrastructure-successor allowance and its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryAllowance {
    /// Number of successors allowed for the whole lineage.
    pub retries: u8,
    /// Winning source, including when its value is zero.
    pub origin: RetryAllowanceOrigin,
}

/// Resolve overrides in precedence order. Pass `None` for an absent host key,
/// rather than a default-filled config value, to retain its visible origin.
pub fn resolve_retry_allowance(
    node: Option<u8>,
    preset: Option<u8>,
    host: Option<u8>,
) -> RetryAllowance {
    let (retries, origin) = if let Some(value) = node {
        (value, RetryAllowanceOrigin::Node)
    } else if let Some(value) = preset {
        (value, RetryAllowanceOrigin::Preset)
    } else if let Some(value) = host {
        (value, RetryAllowanceOrigin::HostConfig)
    } else {
        (
            DEFAULT_SETTLEMENT_RETRIES,
            RetryAllowanceOrigin::ShippedDefault,
        )
    };
    RetryAllowance { retries, origin }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allowance_precedence_and_origin() {
        for (node, preset, host, value, origin) in [
            (Some(0), Some(3), Some(4), 0, RetryAllowanceOrigin::Node),
            (None, Some(3), Some(4), 3, RetryAllowanceOrigin::Preset),
            (None, None, Some(4), 4, RetryAllowanceOrigin::HostConfig),
            (None, None, None, 2, RetryAllowanceOrigin::ShippedDefault),
        ] {
            assert_eq!(
                resolve_retry_allowance(node, preset, host),
                RetryAllowance {
                    retries: value,
                    origin
                }
            );
        }
    }
}
