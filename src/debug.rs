//! Diagnostic subcommands. Built behind the `debug` feature.

use crate::auth;
use crate::cli::DebugAction;
use crate::config;
use anyhow::Result;

/// Dispatch a `jj-gh debug` invocation.
///
/// # Errors
///
/// Returns an error from the underlying operation; for `auth` this means token
/// resolution failed.
pub async fn dispatch(action: DebugAction) -> Result<()> {
    match action {
        DebugAction::Config => print_config(),
        DebugAction::Auth => check_auth().await,
    }
}

fn print_config() -> Result<()> {
    let config = config::load()?;
    println!("{config:#?}");
    Ok(())
}

async fn check_auth() -> Result<()> {
    let config = config::load()?;
    auth::resolve_token(&config).await?;
    println!("ok");
    Ok(())
}
