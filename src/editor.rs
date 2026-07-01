//! Editor command resolution + edit round-trip.
//!
//! Production [`TempfileEditor`] writes the initial buffer to a tempfile, spawns
//! the editor (inheriting stdio), then reads back. Tests use a fake.

use crate::{
    auth::EnvReader,
    frontmatter::Frontmatter,
    gh::{Gh, Reviewer, UpdatePr, remote},
    util::ShellCommand,
};
use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;

pub trait Editor {
    /// Edit `initial` with the configured editor and return the resulting buffer.
    ///
    /// # Errors
    ///
    /// Propagates IO and process errors.
    async fn edit(&self, argv: &[String], initial: &str) -> Result<String>;
}

/// Editor argv from the shell environment: `$VISUAL` then `$EDITOR`, each
/// shell-split. Returns the first non-empty, parseable value.
///
/// Last resort in the precedence chain (flag > config > `$VISUAL` > `$EDITOR`),
/// consulted by `resolve_editor` only when neither the `--editor` flag nor the
/// config `editor` is set. A value that fails to shell-split is logged and
/// skipped so a malformed env var does not abort the whole flow.
pub fn editor_from_env<E: EnvReader>(env: &E) -> Option<ShellCommand> {
    for (name, value) in [("VISUAL", env.get("VISUAL")), ("EDITOR", env.get("EDITOR"))] {
        let Some(raw) = value.filter(|s| !s.trim().is_empty()) else {
            continue;
        };
        match shell_words::split(&raw) {
            Ok(parts) if !parts.is_empty() => return Some(ShellCommand(parts)),
            Ok(_) => {}
            Err(e) => log::warn!("ignoring ${name}: could not shell-split: {e}"),
        }
    }
    None
}

/// Resolve the editor argv. `editor` carries the `--editor` flag merged over the
/// config `editor` (flag wins); when both are absent it falls back to
/// `$VISUAL`/`$EDITOR`. Precedence: flag > config > `$VISUAL` > `$EDITOR`.
///
/// # Errors
///
/// Returns an error when every source is empty.
pub fn resolve_editor<E: EnvReader>(
    editor: Option<&ShellCommand>,
    env: &E,
) -> Result<ShellCommand> {
    editor
        .filter(|c| !c.is_empty())
        .cloned()
        .or_else(|| editor_from_env(env))
        .ok_or_else(|| {
            anyhow!("no editor configured; set --editor, `editor` in config, $VISUAL, or $EDITOR")
        })
}

/// Render `fm` + `body` into the editor buffer, run the editor, and parse the
/// edited buffer back into `(Frontmatter, body)`. The buffer round-trip is the
/// only thing `pr create` and `pr edit` share verbatim.
///
/// When `preview` is `Some`, append a sentinel marker heading + fenced
/// `diff` block after the body. The parser strips everything from
/// the marker onward, so the preview never lands in the submitted PR body.
///
/// # Errors
///
/// Propagates render, IO/process, and parse errors.
pub async fn round_trip<E: Editor>(
    editor: &E,
    argv: &[String],
    fm: &Frontmatter,
    body: &str,
    preview: Option<&str>,
) -> Result<(Frontmatter, String)> {
    let initial = fm.render(body, preview)?;
    let edited = editor.edit(argv, &initial).await?;
    Frontmatter::parse(&edited).context("parsing PR frontmatter")
}

/// Identifiers and context needed by [`apply_frontmatter_diff`] to address the
/// right PR and translate label-name removals to GraphQL node IDs. Bundling
/// these keeps the diff function's signature tractable.
pub struct ApplyChangesCtx<'a> {
    pub owner: &'a str,
    pub repo: &'a str,
    pub pr_number: u64,
    pub pr_node_id: &'a str,
    pub has_merge_queue: bool,
    /// `label_name -> label_node_id` for labels on the PR at fetch time.
    /// Empty for `pr create` since a brand-new PR has no labels to remove.
    pub before_label_ids: HashMap<String, String>,
}

/// Apply the diff between `before` and `after` (and their bodies) to an
/// existing PR via [`Gh`]. Used by `pr edit` (real diff) and `pr create`
/// (post-creation: text fields match the create-request, so only labels,
/// reviewers, and auto-merge actually fire).
///
/// # Errors
///
/// Returns the first GH API error. Earlier successful mutations are not
/// rolled back; the caller is responsible for surfacing partial-success
/// context (e.g. `"PR created, but applying labels failed"`).
pub async fn apply_frontmatter_diff<G: Gh>(
    gh: &G,
    ctx: &ApplyChangesCtx<'_>,
    before: &Frontmatter,
    before_body: &str,
    after: &Frontmatter,
    after_body: &str,
) -> Result<()> {
    let before_base = remote::branch_from_base_spec(ctx.owner, &before.base)?;
    let after_base = remote::branch_from_base_spec(ctx.owner, &after.base)?;
    gh.update_pr(UpdatePr {
        pr_node_id: ctx.pr_node_id.to_string(),
        title: (before.title != after.title).then(|| after.title.clone()),
        body: (before_body != after_body).then(|| after_body.to_string()),
        base_ref_name: (before_base != after_base).then_some(after_base),
    })
    .await?;

    if before.draft != after.draft {
        gh.set_draft(ctx.pr_node_id, after.draft).await?;
    }

    let labels_added = after
        .labels
        .iter()
        .filter(|l| !before.labels.contains(l))
        .cloned()
        .collect::<Vec<String>>();
    let labels_removed_ids = before
        .labels
        .iter()
        .filter(|name| !after.labels.contains(name))
        .filter_map(|name| ctx.before_label_ids.get(name).cloned())
        .collect::<Vec<String>>();
    gh.add_labels(ctx.owner, ctx.repo, ctx.pr_number, &labels_added)
        .await?;
    gh.remove_labels(ctx.pr_node_id, &labels_removed_ids)
        .await?;

    let reviewers_added = after
        .reviewers
        .iter()
        .filter(|r| !before.reviewers.contains(r))
        .cloned()
        .collect::<Vec<Reviewer>>();
    let reviewers_removed = before
        .reviewers
        .iter()
        .filter(|r| !after.reviewers.contains(r))
        .cloned()
        .collect::<Vec<Reviewer>>();
    gh.add_reviewers(ctx.owner, ctx.repo, ctx.pr_number, reviewers_added)
        .await?;
    gh.remove_reviewers(ctx.owner, ctx.repo, ctx.pr_number, reviewers_removed)
        .await?;

    // Auto-merge needs disable+enable when the user keeps it on but changes
    // the merge method, since `enablePullRequestAutoMerge` does not update the
    // method on an already-enabled PR.
    match (before.auto_merge, after.auto_merge) {
        (false, true) => {
            ensure_not_merge_queue(ctx)?;
            gh.enable_auto_merge(ctx.pr_node_id, after.auto_merge_method)
                .await?;
        }
        (true, false) => {
            gh.disable_auto_merge(ctx.pr_node_id).await?;
        }
        (true, true) if before.auto_merge_method != after.auto_merge_method => {
            ensure_not_merge_queue(ctx)?;
            gh.disable_auto_merge(ctx.pr_node_id).await?;
            gh.enable_auto_merge(ctx.pr_node_id, after.auto_merge_method)
                .await?;
        }
        _ => {}
    }

    Ok(())
}

fn ensure_not_merge_queue(ctx: &ApplyChangesCtx<'_>) -> Result<()> {
    if ctx.has_merge_queue {
        bail!(
            "auto-merge not supported for repos with merge queues enabled; this is a limitation of the GitHub API. See https://github.com/mrjones2014/jj-gh/issues/103"
        );
    }
    Ok(())
}

/// Production [`EditorRoundTrip`]: tempfile + spawn editor + read back.
pub struct TempfileEditor;

impl Editor for TempfileEditor {
    async fn edit(&self, argv: &[String], initial: &str) -> Result<String> {
        let tmp = tempfile::Builder::new()
            .suffix(".md")
            .tempfile()
            .context("could not create tempfile for editor buffer")?;
        std::fs::write(tmp.path(), initial).context("could not write editor buffer")?;

        if argv.is_empty() {
            return Err(anyhow!("editor argv is empty"));
        }
        let tmp_arg = tmp.path().to_string_lossy().into_owned();
        let full = argv
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(tmp_arg.as_str()))
            .collect::<Vec<&str>>();
        crate::proc::interactive(&full).await?;

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

    fn cmd(parts: &[&str]) -> ShellCommand {
        ShellCommand(parts.iter().map(|s| (*s).to_string()).collect())
    }

    /// Mirror how `pr create` / `pr edit` resolve the editor: CLI flag, then
    /// config, then `$VISUAL`/`$EDITOR`. `flag.or(cfg)` models the figment merge
    /// where a present `--editor` wins over the config value.
    fn resolve(
        flag: Option<ShellCommand>,
        cfg: Option<ShellCommand>,
        env: &FakeEnv,
    ) -> Option<ShellCommand> {
        let merged = flag.or(cfg);
        resolve_editor(merged.as_ref(), env).ok()
    }

    #[test]
    fn visual_outranks_editor() {
        let env = FakeEnv::with(&[("VISUAL", "nvim +7"), ("EDITOR", "vi")]);
        assert_eq!(editor_from_env(&env), Some(cmd(&["nvim", "+7"])));
    }

    #[test]
    fn editor_env_used_when_visual_absent() {
        let env = FakeEnv::with(&[("EDITOR", "vi")]);
        assert_eq!(editor_from_env(&env), Some(cmd(&["vi"])));
    }

    #[test]
    fn empty_visual_falls_through_to_editor() {
        let env = FakeEnv::with(&[("VISUAL", ""), ("EDITOR", "vi")]);
        assert_eq!(editor_from_env(&env), Some(cmd(&["vi"])));
    }

    #[test]
    fn malformed_env_is_skipped() {
        let env = FakeEnv::with(&[("VISUAL", "nvim '"), ("EDITOR", "vi")]);
        assert_eq!(editor_from_env(&env), Some(cmd(&["vi"])));
    }

    #[test]
    fn no_env_returns_none() {
        assert_eq!(editor_from_env(&FakeEnv::default()), None);
    }

    #[tokio::test]
    async fn flag_outranks_env_and_config() {
        let env = FakeEnv::with(&[("VISUAL", "vim"), ("EDITOR", "vi")]);
        let got = resolve(Some(cmd(&["code", "--wait"])), Some(cmd(&["nano"])), &env);
        assert_eq!(got, Some(cmd(&["code", "--wait"])));
    }

    #[tokio::test]
    async fn config_outranks_env() {
        let env = FakeEnv::with(&[("VISUAL", "vim")]);
        let got = resolve(None, Some(cmd(&["nano"])), &env);
        assert_eq!(got, Some(cmd(&["nano"])));
    }

    #[tokio::test]
    async fn config_used_when_no_flag_or_env() {
        let env = FakeEnv::default();
        let got = resolve(None, Some(cmd(&["nano"])), &env);
        assert_eq!(got, Some(cmd(&["nano"])));
    }

    #[tokio::test]
    async fn env_used_when_no_flag_or_config() {
        let env = FakeEnv::with(&[("EDITOR", "vi")]);
        let got = resolve(None, None, &env);
        assert_eq!(got, Some(cmd(&["vi"])));
    }

    #[tokio::test]
    async fn none_when_all_empty() {
        let env = FakeEnv::default();
        assert_eq!(resolve(None, None, &env), None);
    }
}
