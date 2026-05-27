//! Git-related utilities. Read-side IO lives on the [`crate::jj::Jj`] trait;
//! this module only holds pure helpers.

pub mod real;
pub mod url;
