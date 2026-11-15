//! Pure usage and API-equivalent cost reduction over terminal attempt facts,
//! and the local demand-counter log (`usage`), written through the
//! OS crate's held filesystem handles.
#![forbid(unsafe_code)]

mod chair;
mod claude;
pub mod context;
pub mod experiments;
pub mod ingest;
pub mod ledger;
pub mod mileage;
pub mod native_attempts;
pub mod outcome;
mod prices;
mod reducer;
pub mod refresh;
pub mod route_outcomes;
mod segments;
pub mod signals;
pub mod suggest;
mod trace;
pub mod usage;
pub use chair::{ChairModelUsage, Unread, attach_usage as attach_chair_usage};
pub use prices::{
    BillingKind, Categories, ConfiguredModel, ConfiguredPrices, Freshness, PriceError,
    PriceIdentity, PriceTable, Rates, validate_prices,
};
pub use reducer::*;
pub use trace::{Hosts, trace};

/// Decode one persisted telemetry record through the shared bounded JSON
/// decoder while preserving fields introduced by later compatible writers.
pub(crate) fn decode_record<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    max_bytes: u64,
) -> Result<T, provenant_records::DecodeError> {
    provenant_records::decode(
        bytes,
        provenant_records::Limits {
            max_bytes,
            ..provenant_records::Limits::default()
        },
    )
}

#[cfg(test)]
pub(crate) fn assert_record_round_trip<T>(record: serde_json::Value)
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let bytes = provenant_records::encode(&record).expect("fixture is valid JSON");
    let decoded: T = decode_record(&bytes, bytes.len() as u64).expect("bounded decode");
    let rewritten = provenant_records::encode(&decoded).expect("stored record encodes");
    let rewritten: serde_json::Value =
        serde_json::from_slice(&rewritten).expect("encoded record is JSON");
    assert_eq!(rewritten, record);
}
