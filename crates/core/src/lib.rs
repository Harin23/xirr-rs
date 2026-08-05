//! Financial functions: XIRR, IRR, NPV, MIRR and private-equity metrics.
//!
//! The XIRR implementation is built for **spreadsheet parity** - see
//! [`scheduled::xirr`] for the contract and the module list below for the
//! rest. The full design rationale, including which parts of the solver are
//! load-bearing and must not be changed, is reproduced here from
//! `docs/ALGORITHM.md`:
#![doc = include_str!("../docs/ALGORITHM.md")]

mod models;
mod optimize;
pub mod periodic;
mod scheduled;
mod utils;

pub use models::{DateLike, InvalidPaymentsError};
pub use periodic::*;
pub use scheduled::*;
pub mod private_equity;
