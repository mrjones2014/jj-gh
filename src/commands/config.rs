//! Inspection of jj-gh's own configuration.

use crate::{cli::ConfigAction, config::Config};
use anyhow::{Context, Result};

/// Dispatch a `jj-gh config` invocation.
///
/// # Errors
///
/// Returns an error if the schema cannot be serialized.
///
/// Takes `action` by value to match the other `dispatch` handlers; the lint
/// resolves itself once a variant carries data.
#[expect(clippy::needless_pass_by_value)]
pub fn dispatch(action: ConfigAction) -> Result<()> {
    match action {
        ConfigAction::Schema => print_schema(),
    }
}

/// Print the JSON Schema for [`Config`] on stdout. Also consumed by the
/// `hm-module-schema` flake check, which asserts that the schema's property
/// names match the options exposed by `nix/hm-module.nix`.
fn print_schema() -> Result<()> {
    let schema = schemars::schema_for!(Config);
    let rendered =
        serde_json::to_string_pretty(&schema).context("serialize the config JSON schema")?;
    println!("{rendered}");
    Ok(())
}
