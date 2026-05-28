//! Prints the JSON schema for [`jj_gh::config::Config`] on stdout. Consumed
//! by the `hm-module-schema` flake check, which asserts that the schema's
//! property names match the options exposed by `nix/hm-module.nix`.

use jj_gh::config::Config;

fn main() {
    let schema = schemars::schema_for!(Config);
    println!(
        "{}",
        serde_json::to_string_pretty(&schema).expect("serialize schema")
    );
}
