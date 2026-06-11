//! User-invokable subcommands. Everything under `commands/` is a handler the
//! CLI dispatches to; everything outside it (gh, jj, git, auth, config, proc,
//! ui, ...) is supporting infrastructure.

pub mod completions;
pub mod debug;
pub mod pr;
