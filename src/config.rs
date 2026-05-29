//! Layered config resolution.
//!
//! Values come from (low to high priority):
//! 1. Built-in defaults via [`Config::default`].
//! 2. jj user config (`jj config path --user`).
//! 3. jj repo config (`jj config path --repo`).
//! 4. jj workspace config (`jj config path --workspace`).
//! 5. File pointed to by `$JJ_GH_EXTRA_CONFIG`.
//! 6. Env overlay (`GH_ASKPASS`, `JJ_GH_TEMPLATE`, `JJ_GH_TEMPLATE_FILE`).
//!
//! Layer paths come from `jj config path --<level>` so we track whatever
//! storage layout jj uses (XDG dirs in 0.41+, legacy `.jj/repo/config.toml`
//! before that). Each file source reads from its `[jj-gh]` subtree via
//! [`JjConfProvider`].
//!
//! `pr_create_template` / `pr_create_template_file` are also exposed via
//! per-layer extraction ([`user_layer_template`], [`repo_layer_template`]) so
//! body resolution can prefer a repo-local `.github/PULL_REQUEST_TEMPLATE.md`
//! over a globally-configured jj template.

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
#[cfg_attr(feature = "schema-validation", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct Config {
    pub gh_askpass: Option<Vec<String>>,
    pub askpass_timeout_secs: u64,
    #[cfg_attr(feature = "schema-validation", schemars(with = "Option<String>"))]
    pub gh_token: Option<SecretString>,
    pub default_base_branch: String,
    pub default_remote: String,
    pub upstream_remote: String,
    pub pr_create_template_file: Option<PathBuf>,
    pub pr_create_template: Option<String>,
    pub draft: bool,
    pub auto_merge: bool,
    pub auto_merge_method: AutoMergeMethod,
    pub editor: Option<Vec<String>>,
    pub pr_fetch_bookmark_template: Option<String>,
    pub nerdfonts: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gh_askpass: None,
            askpass_timeout_secs: 20,
            gh_token: None,
            default_base_branch: "master".into(),
            default_remote: "origin".into(),
            upstream_remote: "upstream".into(),
            pr_create_template_file: None,
            pr_create_template: None,
            draft: false,
            auto_merge: false,
            auto_merge_method: AutoMergeMethod::default(),
            editor: None,
            pr_fetch_bookmark_template: None,
            nerdfonts: true,
        }
    }
}

/// GitHub merge method used when enabling auto-merge on a PR.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[cfg_attr(feature = "schema-validation", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
#[clap(rename_all = "lowercase")]
pub enum AutoMergeMethod {
    #[default]
    Merge,
    Squash,
    Rebase,
}

/// Build the layered figment without extracting. Callers (e.g. CLI dispatch)
/// can stack additional providers (like `Serialized::defaults(&args)`) before
/// calling [`extract`].
#[must_use]
pub fn load_figment() -> Figment {
    let mut fig = defaults_figment();
    for path in discover_layers() {
        fig = fig.merge(JjConfProvider::from_file(path));
    }
    fig.merge(Serialized::defaults(EnvOverlay::from_env()))
}

/// A [`Figment`] preloaded with the built-in defaults. Compose [`JjToolsProvider`]s
/// onto this for hermetic tests, then hand to [`extract`].
#[must_use]
pub fn defaults_figment() -> Figment {
    Figment::from(Serialized::defaults(DefaultsOverlay::from_defaults()))
}

/// PR-body template values resolved against a single jj config layer (or a
/// bucket of layers). Used by [`user_layer_template`] and
/// [`repo_layer_template`] so body resolution can prefer a per-repo
/// `.github/PULL_REQUEST_TEMPLATE.md` over a globally-set jj template while
/// still honoring repo-local jj-template overrides.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct LayerTemplate {
    pub pr_create_template: Option<String>,
    pub pr_create_template_file: Option<PathBuf>,
}

/// Extract the PR-body template values defined at the user-level jj config
/// only (`jj config path --user`). Values set in repo or workspace configs are
/// ignored.
///
/// # Errors
///
/// Returns an error if a layer's TOML cannot be parsed.
pub fn user_layer_template() -> Result<LayerTemplate> {
    extract_layer_template(&user_layer_paths())
}

/// Extract the PR-body template values defined in repo, workspace, or
/// `JJ_GH_EXTRA_CONFIG` layers. Values set in the user-level config are
/// ignored.
///
/// # Errors
///
/// Returns an error if a layer's TOML cannot be parsed.
pub fn repo_layer_template() -> Result<LayerTemplate> {
    extract_layer_template(&repo_layer_paths())
}

fn extract_layer_template(paths: &[PathBuf]) -> Result<LayerTemplate> {
    let mut fig = Figment::from(Serialized::defaults(LayerTemplate::default()));
    for path in paths {
        fig = fig.merge(JjConfProvider::from_file(path.clone()));
    }
    fig.extract::<LayerTemplate>().map_err(Into::into)
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
    let mut out = user_layer_paths();
    out.extend(repo_layer_paths());
    out
}

fn user_layer_paths() -> Vec<PathBuf> {
    jj_config_path("--user").into_iter().collect()
}

fn repo_layer_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for level in ["--repo", "--workspace"] {
        if let Some(p) = jj_config_path(level) {
            out.push(p);
        }
    }
    if let Some(p) = std::env::var_os("JJ_GH_EXTRA_CONFIG") {
        out.push(PathBuf::from(p));
    }
    out
}

/// Ask jj for one of its config-file paths. Returns `None` when jj is missing,
/// the layer is unavailable (e.g. `--repo` outside any repo), or jj prints an
/// empty path.
fn jj_config_path(level: &str) -> Option<PathBuf> {
    let output = std::process::Command::new("jj")
        .args(["config", "path", level])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = std::str::from_utf8(&output.stdout).ok()?.trim();
    if s.is_empty() {
        return None;
    }
    Some(PathBuf::from(s))
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
    pub(crate) fn from_memory(name: impl Into<String>, contents: impl Into<String>) -> Self {
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
    default_remote: String,
    upstream_remote: String,
    draft: bool,
    auto_merge: bool,
    auto_merge_method: AutoMergeMethod,
    nerdfonts: bool,
}

impl DefaultsOverlay {
    fn from_defaults() -> Self {
        let Config {
            askpass_timeout_secs,
            auto_merge,
            auto_merge_method,
            default_base_branch,
            default_remote,
            upstream_remote,
            draft,
            nerdfonts,
            editor: _,
            gh_askpass: _,
            gh_token: _,
            pr_fetch_bookmark_template: _,
            pr_create_template: _,
            pr_create_template_file: _,
        } = Config::default();
        Self {
            askpass_timeout_secs,
            default_base_branch,
            default_remote,
            upstream_remote,
            draft,
            auto_merge,
            auto_merge_method,
            nerdfonts,
        }
    }
}

#[derive(Serialize)]
struct EnvOverlay {
    #[serde(skip_serializing_if = "Option::is_none")]
    gh_askpass: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr_create_template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr_create_template_file: Option<PathBuf>,
}

impl EnvOverlay {
    fn from_env() -> Self {
        Self {
            gh_askpass: read_argv_env("GH_ASKPASS"),
            pr_create_template: read_string_env("JJ_GH_TEMPLATE"),
            pr_create_template_file: read_path_env("JJ_GH_TEMPLATE_FILE"),
        }
    }
}

fn read_path_env(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn read_string_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty())
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
            pr_fetch_bookmark_template = '"pr-" ++ pr_number'
            "#,
        )])
        .unwrap();
        assert_eq!(
            config.pr_fetch_bookmark_template.as_deref(),
            Some(r#""pr-" ++ pr_number"#)
        );
    }

    #[test]
    fn pr_create_template_fields_round_trip() {
        let config = from_layers([JjConfProvider::from_memory(
            "tmpl",
            r#"
            [jj-gh]
            pr_create_template = "description.first_line()"
            pr_create_template_file = "/repo/.github/PULL_REQUEST_TEMPLATE.md"
            "#,
        )])
        .unwrap();
        assert_eq!(
            config.pr_create_template.as_deref(),
            Some("description.first_line()")
        );
        assert_eq!(
            config.pr_create_template_file.as_deref(),
            Some(std::path::Path::new(
                "/repo/.github/PULL_REQUEST_TEMPLATE.md"
            ))
        );
    }
}
