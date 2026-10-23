//! Pure usage and API-equivalent cost reduction over terminal attempt facts.
#![forbid(unsafe_code)]

mod prices;
mod reducer;
pub use prices::{
    BillingKind, Categories, ConfiguredModel, ConfiguredPrices, PriceError, PriceIdentity,
    PriceTable, Rates, validate_prices,
};
pub use reducer::*;
