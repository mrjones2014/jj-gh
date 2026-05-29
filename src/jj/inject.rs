//! Reusable builder for the `jj --config-file <tmp>` template-alias injection trick.
//!
//! Callers build a [`TemplateAliases`] via the builder API, write it to a
//! tempfile, then pass that path to `jj`. You can use the full `jj` template
//! language.

use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::Write as _;
use tempfile::NamedTempFile;

/// Builder for the `[template-aliases]` and `[colors]` sections of a jj
/// config file.
///
/// Iteration over a `BTreeMap` is sorted, so [`to_toml`](Self::to_toml) emits
/// a deterministic byte sequence. This keeps snapshot tests stable and makes
/// debugging diffs of the temp config easier.
#[derive(Debug, Default, Serialize)]
pub struct TemplateAliases {
    #[serde(
        rename = "template-aliases",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    aliases: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    colors: BTreeMap<String, String>,
}

impl TemplateAliases {
    #[must_use]
    pub fn builder() -> Self {
        Self::default()
    }

    /// Add a `[template-aliases]` entry. `expr` is a jj template expression
    /// inserted as the alias body.
    #[must_use]
    pub fn alias(mut self, name: impl Into<String>, expr: impl Into<String>) -> Self {
        self.aliases.insert(name.into(), expr.into());
        self
    }

    /// Add a `[colors]` entry. `value` is a jj color string such as `green`,
    /// `bright black`, or `#aabbcc`. Validation is deferred to jj at runtime.
    #[must_use]
    pub fn color(mut self, label: impl Into<String>, value: impl Into<String>) -> Self {
        self.colors.insert(label.into(), value.into());
        self
    }

    /// Render the configured sections as TOML via the [`toml`] crate.
    #[must_use]
    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("TemplateAliases is always TOML-serializable")
    }

    /// Write the rendered TOML to a new tempfile. The returned [`NamedTempFile`]
    /// owns the file; the caller must keep it alive across the `jj` invocation
    /// that reads its path.
    ///
    /// # Errors
    ///
    /// Propagates filesystem errors creating or writing the tempfile.
    pub fn write_temp_config(&self) -> Result<NamedTempFile> {
        let mut tmp = NamedTempFile::with_suffix(".toml").context("creating temp config file")?;
        tmp.write_all(self.to_toml().as_bytes())
            .context("writing template-alias config")?;
        Ok(tmp)
    }
}

/// Escape a string for embedding inside a jj template double-quoted string
/// literal. Handles `\` and `"`; we never embed control characters here.
#[must_use]
pub fn escape_jj_string(s: &str) -> String {
    s.replace('\\', r"\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_builder_renders_empty_toml() {
        let toml = TemplateAliases::builder().to_toml();
        assert_eq!(toml, "");
    }

    #[test]
    fn aliases_render_sorted_under_template_aliases() {
        let toml = TemplateAliases::builder()
            .alias("zeta", r#""z""#)
            .alias("alpha", r#""a""#)
            .to_toml();
        let parsed: toml::Table = toml::from_str(&toml).unwrap();
        let aliases = parsed["template-aliases"].as_table().unwrap();
        let keys: Vec<&str> = aliases.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["alpha", "zeta"]);
        assert_eq!(aliases["alpha"].as_str(), Some(r#""a""#));
        assert_eq!(aliases["zeta"].as_str(), Some(r#""z""#));
    }

    #[test]
    fn colors_render_sorted_under_colors() {
        let toml = TemplateAliases::builder()
            .color("zz-label", "yellow")
            .color("aa-label", "green")
            .to_toml();
        let parsed: toml::Table = toml::from_str(&toml).unwrap();
        let colors = parsed["colors"].as_table().unwrap();
        let keys: Vec<&str> = colors.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["aa-label", "zz-label"]);
    }

    #[test]
    fn aliases_and_colors_both_present() {
        let toml = TemplateAliases::builder()
            .alias("a", r#""x""#)
            .color("c", "red")
            .to_toml();
        let parsed: toml::Table = toml::from_str(&toml).unwrap();
        assert!(parsed.contains_key("template-aliases"));
        assert!(parsed.contains_key("colors"));
    }

    #[test]
    fn color_value_with_quote_round_trips() {
        let toml = TemplateAliases::builder()
            .color("c", r#"weird"name"#)
            .to_toml();
        let parsed: toml::Table = toml::from_str(&toml).unwrap();
        assert_eq!(parsed["colors"]["c"].as_str(), Some(r#"weird"name"#));
    }

    #[test]
    fn alias_body_with_quote_round_trips() {
        let body = r#""https://example.com/""#;
        let toml = TemplateAliases::builder().alias("u", body).to_toml();
        let parsed: toml::Table = toml::from_str(&toml).unwrap();
        assert_eq!(parsed["template-aliases"]["u"].as_str(), Some(body));
    }

    #[test]
    fn write_temp_config_persists_to_disk() {
        let aliases = TemplateAliases::builder().alias("pr_number", r#""42""#);
        let tmp = aliases.write_temp_config().unwrap();
        let on_disk = std::fs::read_to_string(tmp.path()).unwrap();
        assert_eq!(on_disk, aliases.to_toml());
    }

    #[test]
    fn write_temp_config_uses_toml_suffix() {
        let tmp = TemplateAliases::builder().write_temp_config().unwrap();
        assert!(
            tmp.path()
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("toml")),
            "expected .toml suffix, got: {}",
            tmp.path().display()
        );
    }

    #[test]
    fn escape_jj_string_handles_backslash_and_quote() {
        assert_eq!(escape_jj_string(r#"a"b\c"#), r#"a\"b\\c"#);
    }

    #[test]
    fn alias_overwrites_existing_key() {
        let toml = TemplateAliases::builder()
            .alias("a", r#""1""#)
            .alias("a", r#""2""#)
            .to_toml();
        let parsed: toml::Table = toml::from_str(&toml).unwrap();
        assert_eq!(parsed["template-aliases"]["a"].as_str(), Some(r#""2""#));
    }
}
