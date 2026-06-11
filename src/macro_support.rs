//! Stable internal paths for the `jj-gh-config-derive` macros.
//!
//! Code emitted by `subcommand_args!` references the crate's config/CLI types
//! only through this module, so the real definitions can live anywhere; only
//! these re-exports track their location. Do not depend on this module outside
//! generated macro output.

pub(crate) use crate::cli::GlobalOpts;
pub(crate) use crate::config::{__schema, Config};
pub(crate) use crate::util::EvalWithCfgFallback;
