//! Layered config resolution.
//!
//! Values come from (low to high priority):
//! 1. Built-in defaults via [`Config::default`].
//! 2. jj global config at `$JJ_CONFIG` or `$XDG_CONFIG_HOME/jj/config.toml`.
//! 3. jj repo-local config at `<repo>/.jj/repo/config.toml`.
//! 4. File pointed to by `$JJ_GH_EXTRA_CONFIG`.
//! 5. Env overlay (`GH_ASKPASS`, `JJ_GH_TEMPLATE`).
//!
//! Each file source reads from its `[jj-gh]` subtree via [`JjToolsProvider`].

use anyhow::Result;
use figment::{
    Figment, Metadata, Profile, Provider,
    providers::Serialized,
    value::{Dict, Map},
};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub gh_askpass: Option<Vec<String>>,
    pub askpass_timeout_secs: u64,
    pub gh_token: Option<SecretString>,
    pub default_base_branch: String,
    pub template_path: Option<PathBuf>,
    pub draft: bool,
    pub editor: Option<Vec<String>>,
    pub pr_fetch_bookmark_template: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gh_askpass: None,
            askpass_timeout_secs: 20,
            gh_token: None,
            default_base_branch: "master".into(),
            template_path: None,
            draft: false,
            editor: None,
            pr_fetch_bookmark_template: None,
        }
    }
}

/// Discover config layers and merge them into a [`Config`].
///
/// # Errors
///
/// Returns an error if any layer is malformed or fails serde extraction, or if
/// `pr_fetch_bookmark_template` references an unknown placeholder.
pub fn load() -> Result<Config> {
    let mut fig = defaults_figment();
    for path in discover_layers() {
        fig = fig.merge(JjConfProvider::from_file(path));
    }
    fig = fig.merge(Serialized::defaults(EnvOverlay::from_env()));
    let config: Config = extract(&fig)?;
    validate(&config)?;
    Ok(config)
}

/// Validate cross-field invariants on a merged [`Config`].
///
/// # Errors
///
/// Returns an error if `pr_fetch_bookmark_template` references an unknown
/// placeholder.
pub fn validate(config: &Config) -> Result<()> {
    if let Some(t) = config.pr_fetch_bookmark_template.as_deref() {
        crate::pr::fetch::bookmark_template::validate(t)
            .map_err(|e| anyhow::anyhow!("invalid `pr_fetch_bookmark_template`: {e}"))?;
    }
    Ok(())
}

/// A [`Figment`] preloaded with the built-in defaults. Compose [`JjToolsProvider`]s
/// onto this for hermetic tests, then hand to [`extract`].
#[must_use]
pub fn defaults_figment() -> Figment {
    Figment::from(Serialized::defaults(DefaultsOverlay::from_defaults()))
}

/// Extract a [`Config`] from a fully composed [`Figment`].
///
/// # Errors
///
/// Returns an error if serde extraction fails.
pub fn extract(fig: &Figment) -> Result<Config> {
    fig.extract().map_err(Into::into)
}

fn discover_layers() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = jj_global_config_path() {
        out.push(p);
    }

    if let Some(p) = jj_repo_config_path() {
        out.push(p);
    }

    if let Some(p) = std::env::var_os("JJ_GH_EXTRA_CONFIG") {
        out.push(PathBuf::from(p));
    }

    out
}

fn jj_global_config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("JJ_CONFIG") {
        return Some(PathBuf::from(p));
    }

    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;

    Some(base.join("jj").join("config.toml"))
}

fn jj_repo_config_path() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    for dir in cwd.ancestors() {
        let candidate = dir.join(".jj").join("repo").join("config.toml");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Source for a [`JjToolsProvider`].
enum TomlSource {
    /// Read from a file on disk. Missing files contribute no values.
    File(PathBuf),
    /// In-memory TOML, labelled by source name. Test-only.
    #[cfg(test)]
    Memory { name: String, contents: String },
    /// Simulated "file is missing"; contributes no values. Test-only.
    #[cfg(test)]
    Absent { name: String },
}

/// Figment provider that extracts the `jj-gh` subtree from a TOML source.
pub struct JjConfProvider {
    source: TomlSource,
}

impl JjConfProvider {
    #[must_use]
    pub fn from_file(path: impl Into<PathBuf>) -> Self {
        Self {
            source: TomlSource::File(path.into()),
        }
    }

    #[cfg(test)]
    fn from_memory(name: impl Into<String>, contents: impl Into<String>) -> Self {
        Self {
            source: TomlSource::Memory {
                name: name.into(),
                contents: contents.into(),
            },
        }
    }

    #[cfg(test)]
    fn from_absent(name: impl Into<String>) -> Self {
        Self {
            source: TomlSource::Absent { name: name.into() },
        }
    }

    fn read(&self) -> Result<Option<String>, SourceError> {
        match &self.source {
            TomlSource::File(path) => match std::fs::read_to_string(path) {
                Ok(s) => Ok(Some(s)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(SourceError::Io(e.to_string())),
            },
            #[cfg(test)]
            TomlSource::Memory { contents, .. } => Ok(Some(contents.clone())),
            #[cfg(test)]
            TomlSource::Absent { .. } => Ok(None),
        }
    }

    fn source_label(&self) -> String {
        match &self.source {
            TomlSource::File(path) => path.display().to_string(),
            #[cfg(test)]
            TomlSource::Memory { name, .. } => format!("<memory:{name}>"),
            #[cfg(test)]
            TomlSource::Absent { name } => format!("<absent:{name}>"),
        }
    }
}

impl Provider for JjConfProvider {
    fn metadata(&self) -> Metadata {
        Metadata::named("jj config (jj-gh)").source(self.source_label())
    }

    fn data(&self) -> Result<Map<Profile, Dict>, figment::Error> {
        let contents = self
            .read()
            .map_err(|e| figment::Error::from(e.to_string()))?;
        let Some(contents) = contents else {
            return Ok(Map::new());
        };
        let table =
            extract_jj_gh_subtree(&contents).map_err(|e| figment::Error::from(e.to_string()))?;
        let Some(table) = table else {
            return Ok(Map::new());
        };
        Serialized::defaults(table).data()
    }
}

#[derive(Debug, thiserror::Error)]
enum SourceError {
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Toml(String),
    #[error("`jj-gh` must be a TOML table")]
    NotATable,
}

fn extract_jj_gh_subtree(contents: &str) -> Result<Option<toml::Table>, SourceError> {
    let parsed: toml::Value =
        toml::from_str(contents).map_err(|e| SourceError::Toml(e.to_string()))?;
    let Some(subtree) = parsed.get("jj-gh") else {
        return Ok(None);
    };
    let table = subtree.as_table().ok_or(SourceError::NotATable)?.clone();
    Ok(Some(table))
}

#[derive(Serialize)]
struct DefaultsOverlay {
    askpass_timeout_secs: u64,
    default_base_branch: String,
    draft: bool,
}

impl DefaultsOverlay {
    fn from_defaults() -> Self {
        let d = Config::default();
        Self {
            askpass_timeout_secs: d.askpass_timeout_secs,
            default_base_branch: d.default_base_branch,
            draft: d.draft,
        }
    }
}

#[derive(Serialize)]
struct EnvOverlay {
    #[serde(skip_serializing_if = "Option::is_none")]
    gh_askpass: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    template_path: Option<PathBuf>,
}

impl EnvOverlay {
    fn from_env() -> Self {
        Self {
            gh_askpass: read_argv_env("GH_ASKPASS"),
            template_path: read_path_env("JJ_GH_TEMPLATE"),
        }
    }
}

fn read_path_env(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn read_argv_env(key: &str) -> Option<Vec<String>> {
    let raw = std::env::var(key).ok().filter(|s| !s.is_empty())?;
    shell_words::split(&raw).ok().filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    fn from_layers<I: IntoIterator<Item = JjConfProvider>>(layers: I) -> Result<Config> {
        let mut fig = defaults_figment();
        for layer in layers {
            fig = fig.merge(layer);
        }
        extract(&fig)
    }

    #[test]
    fn defaults_when_no_layers() {
        let config = from_layers([]).unwrap();
        assert_eq!(config.askpass_timeout_secs, 20);
        assert_eq!(config.default_base_branch, "master");
        assert!(!config.draft);
        assert!(config.gh_token.is_none());
    }

    #[test]
    fn absent_source_is_non_fatal() {
        let config = from_layers([JjConfProvider::from_absent("global")]).unwrap();
        assert_eq!(config.default_base_branch, "master");
    }

    #[test]
    fn later_layers_override_earlier_layers() {
        let config = from_layers([
            JjConfProvider::from_memory(
                "lo",
                r#"
                [jj-gh]
                default_base_branch = "develop"
                askpass_timeout_secs = 5
                "#,
            ),
            JjConfProvider::from_memory(
                "hi",
                r#"
                [jj-gh]
                default_base_branch = "trunk"
                "#,
            ),
        ])
        .unwrap();
        assert_eq!(config.default_base_branch, "trunk");
        assert_eq!(config.askpass_timeout_secs, 5);
    }

    #[test]
    fn token_deserializes_as_redacted_secret() {
        let config = from_layers([JjConfProvider::from_memory(
            "with-token",
            r#"
            [jj-gh]
            gh_token = "ghp_super_secret"
            "#,
        )])
        .unwrap();
        let token = config.gh_token.as_ref().expect("gh_token field present");
        assert_eq!(token.expose_secret(), "ghp_super_secret");

        let debug_output = format!("{config:?}");
        assert!(
            !debug_output.contains("ghp_super_secret"),
            "Debug output leaked token: {debug_output}"
        );
    }

    #[test]
    fn ignores_unrelated_jj_config_keys() {
        let config = from_layers([JjConfProvider::from_memory(
            "noise",
            r#"
            [user]
            name = "user"
            email = "user@example.com"

            [jj-gh]
            default_base_branch = "main"
            "#,
        )])
        .unwrap();
        assert_eq!(config.default_base_branch, "main");
    }

    #[test]
    fn rejects_non_table_subtree() {
        let err = from_layers([JjConfProvider::from_memory(
            "bad",
            r#"
            "jj-gh" = "not a table"
            "#,
        )])
        .unwrap_err();
        assert!(err.to_string().contains("must be a TOML table"));
    }

    #[test]
    fn extract_returns_none_when_subtree_absent() {
        let parsed = extract_jj_gh_subtree("[other]\nkey = 1\n").unwrap();
        assert!(parsed.is_none());
    }

    #[test]
    fn extract_returns_table_when_present() {
        let parsed = extract_jj_gh_subtree(
            r#"
            [jj-gh]
            default_base_branch = "trunk"
            "#,
        )
        .unwrap();
        let table = parsed.expect("subtree present");
        assert_eq!(
            table.get("default_base_branch").and_then(|v| v.as_str()),
            Some("trunk")
        );
    }

    #[test]
    fn pr_fetch_bookmark_template_round_trips() {
        let config = from_layers([JjConfProvider::from_memory(
            "tmpl",
            r#"
            [jj-gh]
            pr_fetch_bookmark_template = "pr-{number}-{user}"
            "#,
        )])
        .unwrap();
        assert_eq!(
            config.pr_fetch_bookmark_template.as_deref(),
            Some("pr-{number}-{user}")
        );
        validate(&config).unwrap();
    }

    #[test]
    fn validate_rejects_unknown_placeholder_in_template() {
        let config = Config {
            pr_fetch_bookmark_template: Some("pr-{nope}".into()),
            ..Config::default()
        };
        let err = validate(&config).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("pr_fetch_bookmark_template"), "msg: {msg}");
        assert!(msg.contains("{nope}"), "msg: {msg}");
    }
}
