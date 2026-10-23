//! Sans-IO process policy and the safe operating-system boundary.
//!
//! These APIs are inert until the command and S2 custody integration passes.
#![deny(unsafe_code)]

pub mod bounded;
pub mod capture;
pub mod identity;
pub mod process_info;
pub mod ps;
#[allow(unsafe_code)]
pub mod sys;
#[cfg(test)]
mod test_support;
