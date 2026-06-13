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
use jj_gh_config_derive::config_schema;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

config_schema! {
    /// Askpass helper command that prints a GitHub token on stdout.
    #[env("GH_ASKPASS", argv)]
    gh_askpass: Option<Vec<String>> = None,

    /// Timeout in seconds for the askpass helper.
    askpass_timeout_secs: u64 = 20,

    /// GitHub auth token. Never logged. Resolved via askpass when absent.
    /// `SecretString` does not impl `Serialize` (intentional; we never emit
    /// it back to figment), so the serde skip is per-field rather than via
    /// the macro's default `skip_serializing_if = "Option::is_none"`.
    #[serde(skip_serializing)]
    #[cfg_attr(feature = "schema-validation", schemars(with = "Option<String>"))]
    gh_token: Option<SecretString> = None,

    /// Fallback base branch when neither `--base` nor an ancestor bookmark
    /// nor jj `trunk()` resolves. If none of the above resolve, and this
    /// option is not set, an error will occur.
    default_base_branch: Option<String> = None,

    /// Git remote used for the user's own pushes and PR head lookups.
    #[deprecated(since = "0.2.5", note = "jj-gh now auto-detects the default remote via the repository data.")]
    default_remote: Option<String> = None,

    /// Git remote used as the PR target in fork workflows.
    upstream_remote: String = "upstream".into(),

    /// Path to a markdown template file used as the PR body.
    #[env("JJ_GH_TEMPLATE_FILE", path)]
    pr_create_template_file: Option<PathBuf> = None,

    /// jj template string used to render the PR body.
    #[env("JJ_GH_TEMPLATE", string)]
    pr_create_template: Option<String> = None,

    /// jj template string used to render candidate PR titles.
    pr_create_title_template: String = "description.first_line()".into(),

    /// Open new PRs as drafts.
    draft: bool = false,

    /// Enable auto-merge on newly created PRs.
    auto_merge: bool = false,

    /// Merge method used when auto-merge is enabled.
    auto_merge_method: AutoMergeMethod = AutoMergeMethod::Merge,

    /// Editor command for the PR editor flow.
    editor: Option<Vec<String>> = None,

    /// Show a preview of the PR diffs while creating PR body.
    pr_create_show_diffs: bool = true,

    /// jj template used to render the bookmark name in `pr fetch`.
    pr_fetch_bookmark_template: Option<String> = None,

    /// jj template used by `pr log`.
    pr_log_template: Option<String> = None,

    /// jj template used by `pr restack`. Falls back to `pr_log_template`.
    pr_restack_template: Option<String> = None,

    /// Render Nerd-Fonts glyphs in TUI output.
    nerdfonts: bool = true,
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
    fig.merge(Serialized::defaults(env_overlay()))
}

/// A [`Figment`] preloaded with the built-in defaults. Compose [`JjConfProvider`]s
/// onto this for hermetic tests, then hand to [`extract`].
#[must_use]
pub fn defaults_figment() -> Figment {
    Figment::from(Serialized::defaults(Config::default()))
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
    // The three `jj config path` calls are independent cold subprocess spawns;
    // run them concurrently so startup pays one spawn's latency, not three.
    // Order (user, repo, workspace) is load-bearing: later layers override
    // earlier, and `JJ_GH_EXTRA_CONFIG` sits highest.
    let outputs = crate::proc::capture_sync_batch(&[
        ["jj", "config", "path", "--user"].as_slice(),
        ["jj", "config", "path", "--repo"].as_slice(),
        ["jj", "config", "path", "--workspace"].as_slice(),
    ]);
    let mut out = outputs
        .iter()
        .filter_map(|o| parse_config_path(o.as_deref()))
        .collect::<Vec<PathBuf>>();
    if let Some(p) = std::env::var_os("JJ_GH_EXTRA_CONFIG") {
        out.push(PathBuf::from(p));
    }
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
    let stdout = crate::proc::capture_sync(&["jj", "config", "path", level])?;
    parse_config_path(Some(&stdout))
}

/// Parse a `jj config path` stdout into a layer path. `None` when the command
/// failed (no stdout) or jj printed an empty path.
fn parse_config_path(stdout: Option<&[u8]>) -> Option<PathBuf> {
    let s = std::str::from_utf8(stdout?).ok()?.trim();
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
        for &(key, message) in __DEPRECATED_CONFIG_KEYS {
            if table.contains_key(key) {
                log::warn!("{message} Source: {}.", self.source_label());
            }
        }
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
    let parsed =
        toml::from_str::<toml::Value>(contents).map_err(|e| SourceError::Toml(e.to_string()))?;
    let Some(subtree) = parsed.get("jj-gh") else {
        return Ok(None);
    };
    let table = subtree.as_table().ok_or(SourceError::NotATable)?.clone();
    Ok(Some(table))
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
        assert_eq!(config.default_base_branch, None);
        assert!(!config.draft);
        assert!(config.gh_token.is_none());
    }

    #[test]
    fn deprecated_config_keys_include_generated_messages() {
        assert_eq!(
            __DEPRECATED_CONFIG_KEYS,
            &[(
                "default_remote",
                "`jj-gh.default_remote` is deprecated since 0.2.5: jj-gh now auto-detects the default remote via the repository data."
            )]
        );
    }

    #[test]
    fn absent_source_is_non_fatal() {
        let config = from_layers([JjConfProvider::from_absent("global")]).unwrap();
        assert_eq!(config.default_base_branch, None);
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
        assert_eq!(config.default_base_branch.as_deref(), Some("trunk"));
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
        assert_eq!(config.default_base_branch.as_deref(), Some("main"));
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
            pr_create_title_template = "description.first_line().upper()"
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
        assert_eq!(
            config.pr_create_title_template,
            "description.first_line().upper()"
        );
    }
}
