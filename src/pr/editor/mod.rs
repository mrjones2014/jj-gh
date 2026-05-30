//! Editor command resolution + edit round-trip.
//!
//! Production [`TempfileEditor`] writes the initial buffer to a tempfile, spawns
//! the editor (inheriting stdio), then reads back. Tests use a fake.

pub(crate) mod create;
pub(crate) mod edit;

use crate::{auth::EnvReader, config::Config};
use anyhow::{Context, Result, anyhow};
use tokio::process::Command;

pub trait EditorRoundTrip {
    /// Edit `initial` with the configured editor and return the resulting buffer.
    ///
    /// # Errors
    ///
    /// Propagates IO and process errors.
    async fn edit(&self, argv: &[String], initial: &str) -> Result<String>;
}

/// Resolve the editor argv from the merged config and shell env. CLI
/// `--editor` is folded into `config.editor` by the figment overlay in
/// `pr::dispatch`.
///
/// Precedence (high to low):
/// 1. `editor` in (merged) config, including `--editor` if passed
/// 2. `$VISUAL`
/// 3. `$EDITOR`
///
/// # Errors
///
/// Returns an error if no source produced a non-empty argv.
#[must_use]
pub fn resolve_editor_argv<E: EnvReader>(config: &Config, env: &E) -> Result<Vec<String>> {
    if let Some(argv) = config.editor.as_deref().filter(|v| !v.is_empty()) {
        return Ok(argv.to_vec());
    }

    for (name, value) in [("VISUAL", env.get("VISUAL")), ("EDITOR", env.get("EDITOR"))] {
        if let Some(raw) = value.filter(|s| !s.trim().is_empty()) {
            let parts =
                shell_words::split(&raw).with_context(|| format!("could not split ${name}"))?;
            if !parts.is_empty() {
                return Ok(parts);
            }
        }
    }

    Err(anyhow!(
        "no editor configured; set --editor, `editor` in config, $VISUAL, or $EDITOR"
    ))
}

/// Production [`EditorRoundTrip`]: tempfile + spawn editor + read back.
pub struct TempfileEditor;

impl EditorRoundTrip for TempfileEditor {
    async fn edit(&self, argv: &[String], initial: &str) -> Result<String> {
        let tmp = tempfile::Builder::new()
            .suffix(".md")
            .tempfile()
            .context("could not create tempfile for editor buffer")?;
        std::fs::write(tmp.path(), initial).context("could not write editor buffer")?;

        let (prog, rest) = argv
            .split_first()
            .ok_or_else(|| anyhow!("editor argv is empty"))?;
        let tmp_arg = tmp.path().to_string_lossy().into_owned();
        let status = Command::new(prog)
            .args(rest)
            .arg(tmp_arg)
            .status()
            .await
            .with_context(|| format!("failed to spawn editor `{prog}`"))?;
        if !status.success() {
            return Err(anyhow!("editor exited with {status}"));
        }

        std::fs::read_to_string(tmp.path()).context("could not read edited buffer")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct FakeEnv(HashMap<String, String>);

    impl FakeEnv {
        fn with(pairs: &[(&str, &str)]) -> Self {
            Self(
                pairs
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                    .collect(),
            )
        }
    }

    impl EnvReader for FakeEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn config_used_when_set() {
        let mut c = cfg();
        c.editor = Some(vec!["code".into(), "--wait".into()]);
        let env = FakeEnv::with(&[("VISUAL", "vim"), ("EDITOR", "vi")]);
        let argv = resolve_editor_argv(&c, &env).unwrap();
        assert_eq!(argv, vec!["code".to_string(), "--wait".into()]);
    }

    #[test]
    fn visual_outranks_editor() {
        let env = FakeEnv::with(&[("VISUAL", "nvim +7"), ("EDITOR", "vi")]);
        let argv = resolve_editor_argv(&cfg(), &env).unwrap();
        assert_eq!(argv, vec!["nvim".to_string(), "+7".into()]);
    }

    #[test]
    fn editor_env_used_when_visual_absent() {
        let env = FakeEnv::with(&[("EDITOR", "vi")]);
        let argv = resolve_editor_argv(&cfg(), &env).unwrap();
        assert_eq!(argv, vec!["vi".to_string()]);
    }

    #[test]
    fn empty_visual_falls_through_to_editor() {
        let env = FakeEnv::with(&[("VISUAL", ""), ("EDITOR", "vi")]);
        let argv = resolve_editor_argv(&cfg(), &env).unwrap();
        assert_eq!(argv, vec!["vi".to_string()]);
    }

    #[test]
    fn empty_config_editor_falls_through() {
        let mut c = cfg();
        c.editor = Some(vec![]);
        let env = FakeEnv::with(&[("EDITOR", "vi")]);
        let argv = resolve_editor_argv(&c, &env).unwrap();
        assert_eq!(argv, vec!["vi".to_string()]);
    }

    #[test]
    fn no_sources_errors() {
        let env = FakeEnv::default();
        let err = resolve_editor_argv(&cfg(), &env).unwrap_err();
        assert!(err.to_string().contains("no editor configured"));
    }
}
