//! Editor command resolution + edit round-trip.
//!
//! Production [`TempfileEditor`] writes the initial buffer to a tempfile, spawns
//! the editor (inheriting stdio), then reads back. Tests use a fake.

use crate::{cli::CreateArgs, config::Config};
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

/// Resolve the editor argv from CLI, config, and shell env.
///
/// Precedence (high to low):
/// 1. `--editor` CLI flag
/// 2. `editor` in config
/// 3. `$VISUAL`
/// 4. `$EDITOR`
///
/// # Errors
///
/// Returns an error if no source produced a non-empty argv.
pub fn resolve_editor_argv(
    args: &CreateArgs,
    config: &Config,
    visual: Option<&str>,
    editor: Option<&str>,
) -> Result<Vec<String>> {
    if let Some(argv) = args.editor.as_deref().filter(|v| !v.is_empty()) {
        return Ok(argv.to_vec());
    }

    if let Some(argv) = config.editor.as_deref().filter(|v| !v.is_empty()) {
        return Ok(argv.to_vec());
    }

    for (name, value) in [("VISUAL", visual), ("EDITOR", editor)] {
        if let Some(raw) = value.filter(|s| !s.trim().is_empty()) {
            let parts =
                shell_words::split(raw).with_context(|| format!("could not split ${name}"))?;
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

    fn args() -> CreateArgs {
        CreateArgs {
            rev: "@-".into(),
            base: None,
            draft: false,
            no_draft: false,
            template: None,
            no_template: false,
            editor: None,
            gh_askpass: None,
            askpass_timeout_secs: None,
        }
    }

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn cli_takes_priority() {
        let mut a = args();
        a.editor = Some(vec!["nvim".into(), "+7".into()]);
        let argv = resolve_editor_argv(&a, &cfg(), Some("vim"), Some("vi")).unwrap();
        assert_eq!(argv, vec!["nvim".to_string(), "+7".into()]);
    }

    #[test]
    fn falls_back_to_config_when_no_cli() {
        let mut c = cfg();
        c.editor = Some(vec!["code".into(), "--wait".into()]);
        let argv = resolve_editor_argv(&args(), &c, Some("vim"), Some("vi")).unwrap();
        assert_eq!(argv, vec!["code".to_string(), "--wait".into()]);
    }

    #[test]
    fn visual_outranks_editor() {
        let argv = resolve_editor_argv(&args(), &cfg(), Some("nvim +7"), Some("vi")).unwrap();
        assert_eq!(argv, vec!["nvim".to_string(), "+7".into()]);
    }

    #[test]
    fn editor_env_used_when_visual_absent() {
        let argv = resolve_editor_argv(&args(), &cfg(), None, Some("vi")).unwrap();
        assert_eq!(argv, vec!["vi".to_string()]);
    }

    #[test]
    fn empty_strings_are_skipped() {
        let argv = resolve_editor_argv(&args(), &cfg(), Some(""), Some("vi")).unwrap();
        assert_eq!(argv, vec!["vi".to_string()]);
    }

    #[test]
    fn empty_cli_falls_through() {
        let mut a = args();
        a.editor = Some(vec![]);
        let argv = resolve_editor_argv(&a, &cfg(), None, Some("vi")).unwrap();
        assert_eq!(argv, vec!["vi".to_string()]);
    }

    #[test]
    fn no_sources_errors() {
        let err = resolve_editor_argv(&args(), &cfg(), None, None).unwrap_err();
        assert!(err.to_string().contains("no editor configured"));
    }
}
