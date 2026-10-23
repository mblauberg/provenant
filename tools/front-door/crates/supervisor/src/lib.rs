//! Sans-IO process policy and the safe operating-system boundary.
//!
//! These APIs are inert until the command and S2 custody integration passes.
#![deny(unsafe_code)]

pub mod identity;
#[allow(unsafe_code)]
pub mod sys;
pub mod capture;
#[cfg(test)]
mod test_support;
