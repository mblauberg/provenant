//! Pure usage and API-equivalent cost reduction over terminal attempt facts,
//! and the local demand-counter log (`usage`), written through the
//! OS crate's held filesystem handles.
#![forbid(unsafe_code)]

mod chair;
mod claude;
pub mod experiments;
pub mod ingest;
pub mod mileage;
mod native;
pub mod outcome;
mod prices;
mod reducer;
pub mod refresh;
pub mod suggest;
mod trace;
pub mod usage;
pub use chair::{ChairModelUsage, Unread, attach_usage as attach_chair_usage};
pub use native::{NativeError, NativeInput, NativeModel, NativeReport, claude_subagents};
pub use prices::{
    BillingKind, Categories, ConfiguredModel, ConfiguredPrices, Freshness, PriceError,
    PriceIdentity, PriceTable, Rates, validate_prices,
};
pub use reducer::*;
pub use trace::{Hosts, trace};
