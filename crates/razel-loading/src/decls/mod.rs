//! E0 declaration store + demand-driven analysis (record → drive/ensure → harvest).

mod store;
mod capture;
mod drive;
mod aspect;
mod resolve;
mod rule_decl;

pub(crate) use {store::*, capture::*, drive::*, aspect::*, resolve::*, rule_decl::*};
