//! Read-side abstraction over `jj`.
//!
//! All repo reads (commits, bookmarks, remotes) go through [`Jj`]. The production
//! impl shells out to `jj` (and to `git` against jj's embedded store for the
//! remote URL); tests use a fake.

use crate::util::EvalWithCfgFallback;
use anyhow::Result;
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub mod inject;
pub mod real;

/// What we read about a single revision.
#[derive(Debug, Clone, Deserialize)]
pub struct CommitInfo {
    pub change_id: String,
    pub commit_id: String,
    pub description: String,
    pub bookmarks: Vec<String>,
}

/// A local bookmark tracked on the `origin` remote, paired with the commit
/// the *local* side currently points at. The local commit may diverge from
/// the remote target (e.g. user rebased without pushing).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PushedBookmark {
    pub name: String,
    /// 40-char hex commit id of the local bookmark target.
    pub local_commit_id: String,
}

pub trait Jj {
    /// Resolve the repository's default push remote, or `Ok(None)` when git
    /// cannot determine one (no `remote.pushDefault`, no branch tracking, and
    /// no `origin`). `Err` is reserved for genuine store-query failures.
    ///
    /// # Errors
    ///
    /// Propagates errors from the embedded git store query.
    async fn default_remote(&self) -> Result<Option<String>>;

    /// Names of every git remote configured in the repository, sorted.
    ///
    /// # Errors
    ///
    /// Propagates errors from the embedded git store query.
    async fn remote_names(&self) -> Result<Vec<String>>;

    /// Resolve a single revision into commit metadata.
    ///
    /// # Errors
    ///
    /// Returns an error if the revset does not resolve to exactly one commit or if
    /// the jj invocation fails.
    async fn resolve_rev(&self, rev: &str) -> Result<CommitInfo>;

    /// Closest ancestor commit (excluding `rev` itself) that carries a bookmark.
    ///
    /// # Errors
    ///
    /// Propagates jj errors. Returns `Ok(None)` when no such ancestor exists.
    async fn stacked_ancestor_bookmark(&self, rev: &str) -> Result<Option<String>>;

    /// First-line description of the oldest commit in `revset`. Used to compute the
    /// default PR title.
    ///
    /// # Errors
    ///
    /// Propagates jj errors.
    async fn first_commit_description(&self, revset: &str) -> Result<String>;

    /// URL configured for the given git remote, or `Ok(None)` if unset.
    ///
    /// # Errors
    ///
    /// Propagates failures from the embedded git store query.
    async fn remote_url(&self, name: &str) -> Result<Option<String>>;

    /// Commit SHA of `bookmark@remote` if it exists, else `Ok(None)`.
    ///
    /// # Errors
    ///
    /// Propagates jj errors.
    async fn remote_bookmark_sha(&self, bookmark: &str, remote: &str) -> Result<Option<String>>;

    /// Pushes `rev` to `remote`. With an existing `bookmark`, pushes it via
    /// `-b <bookmark>`; otherwise `-c <rev>`, which creates a `push-<change_id>`
    /// bookmark.
    ///
    /// # Errors
    ///
    /// Propagates jj failures.
    async fn push(&self, rev: &str, bookmark: Option<&str>, remote: String) -> Result<()>;

    /// Bookmark at jj's `trunk()` revset, or `Ok(None)` if `trunk()` is empty.
    ///
    /// jj's `trunk()` is driven by the repo's `revsets.trunk` setting.
    ///
    /// # Errors
    ///
    /// Propagates jj errors.
    async fn trunk_branch(&self) -> Result<Option<String>>;

    /// Absolute path to the jj workspace root.
    ///
    /// # Errors
    ///
    /// Propagates jj errors.
    async fn workspace_root(&self) -> Result<&PathBuf>;

    /// Run `jj git import` to re-read refs from the underlying git store.
    ///
    /// # Errors
    ///
    /// Propagates jj failures.
    async fn git_import(&self) -> Result<()>;

    /// Bookmarks that have a tracking branch on `remote`, paired with the
    /// commit id the *local* bookmark currently targets. Used to scope GitHub
    /// PR lookups to branches the user has actually pushed and to render PR
    /// badges against the local commit (even when the local bookmark has
    /// diverged from the remote, e.g. local rebase without push). Sorted by
    /// name, deduped.
    ///
    /// # Errors
    ///
    /// Propagates jj errors.
    async fn pushed_bookmarks(&self, remote: &str) -> Result<Vec<PushedBookmark>>;

    /// Render `template` by invoking `jj log` against `revset`. When
    /// `config_file` is `Some`, jj is given `--config-file <path>` so the
    /// template can reference aliases or colors defined there (typically built
    /// via [`inject::TemplateAliases`]).
    ///
    /// Returns raw stdout. Callers trim or otherwise normalize the result
    /// based on what they expect (a bookmark name versus a multi-line PR
    /// body).
    ///
    /// `reversed` sets the `--reversed` flag so multi-commit revsets render oldest
    /// first (chronological order).
    ///
    /// # Errors
    ///
    /// Returns an error if jj exits non-zero (template parse failures land
    /// here with jj's own error in the message). Callers should add their own
    /// context via [`anyhow::Context`].
    async fn eval_template(
        &self,
        revset: &str,
        template: &str,
        config_file: Option<&Path>,
        reversed: bool,
    ) -> Result<String>;

    /// Git-format diff of the commits selected by `revset`. Used to render a
    /// read-only preview in the `pr create` editor buffer.
    ///
    /// Returns raw stdout with no color codes (suitable for embedding in a
    /// markdown `diff` fence).
    ///
    /// # Errors
    ///
    /// Propagates jj failures.
    async fn diff(&self, revset: &str) -> Result<String>;

    /// Git-format three-dot diff between two Git commit OIDs.
    ///
    /// Returns an error when either commit is unavailable locally or when
    /// their merge base cannot be resolved. Callers may use a remote fallback.
    ///
    /// # Errors
    ///
    /// Propagates jj failures.
    async fn pr_diff(&self, base_oid: &str, head_oid: &str) -> Result<String>;
}

pub trait JjExt {
    /// Resolve the remote for the user's own pushes and PR head lookups.
    ///
    /// Precedence: the explicit `--remote` flag, then git's auto-detected
    /// default push remote, then the `default_remote` config fallback. Errors
    /// with a guide when none resolve.
    ///
    /// # Errors
    ///
    /// Returns a teaching error listing the configured remotes when no remote
    /// resolves; propagates store-query failures.
    async fn resolve_default_remote(&self, remote: &EvalWithCfgFallback<String>) -> Result<String>;

    /// Auto-detect the default push remote, logging what git returned and
    /// mapping any store-query error to `None` (so resolution can fall through
    /// to the config fallback rather than aborting).
    async fn auto_detected_remote(&self) -> Option<String>;
}

impl<T> JjExt for T
where
    T: Jj,
{
    async fn auto_detected_remote(&self) -> Option<String> {
        match self.default_remote().await {
            Ok(Some(name)) => {
                log::debug!("remote: git auto-detected default push remote `{name}`");
                Some(name)
            }
            Ok(None) => {
                log::debug!("remote: git found no default push remote");
                None
            }
            Err(e) => {
                log::debug!("remote: default-remote lookup failed: {e:#}");
                None
            }
        }
    }

    async fn resolve_default_remote(&self, remote: &EvalWithCfgFallback<String>) -> Result<String> {
        let Some(name) = remote.resolve(|| self.auto_detected_remote()).await else {
            let names = self.remote_names().await.unwrap_or_default();
            return Err(remote_resolution_error(&names));
        };
        log::debug!("remote: resolved to `{name}`");
        Ok(name)
    }
}

/// Build the teaching error shown when no git remote can be resolved. Explains
/// the resolution order and lists the remotes that *are* configured so the
/// user can pick one.
#[must_use]
pub fn remote_resolution_error(remote_names: &[String]) -> anyhow::Error {
    let configured = if remote_names.is_empty() {
        "(none)".to_string()
    } else {
        remote_names.join(", ")
    };
    anyhow::anyhow!(
        concat!(
            "could not determine the default git remote for this repository.\n",
            "\n",
            "jj-gh resolves the remote in this order:\n",
            "  1. --upstream-remote / --remote flag\n",
            "  2. jj-gh.upstream_remote / jj-gh.default_remote in config\n",
            "  3. git's default push remote (branch.<b>.pushRemote, remote.pushDefault, else `origin`)\n",
            "\n",
            "None matched. Configured remotes: {}\n",
            "Fix: set `jj-gh.default_remote = \"<name>\"` in your jj config, or pass `--remote <name>`.",
        ),
        configured
    )
}

/// Make the revset from the selected PR base revision to `rev`.
#[must_use]
pub fn title_base_revset(rev: &str, base_rev: &str) -> String {
    format!("({base_rev})..({rev})")
}

/// Build a `jj` command line, prepending the program and the
/// `--ignore-working-copy` flag shared by all our invocations.
pub(crate) fn jj_argv<'a>(args: &[&'a str]) -> Vec<&'a str> {
    ["jj", "--ignore-working-copy"]
        .into_iter()
        .chain(args.iter().copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revset_with_stacked_base() {
        assert_eq!(
            title_base_revset("@-", "mrj/push-foo"),
            "(mrj/push-foo)..(@-)"
        );
    }

    #[test]
    fn revset_with_trunk_base() {
        assert_eq!(title_base_revset("@-", "trunk()"), "(trunk())..(@-)");
    }

    #[test]
    fn revset_with_resolved_remote_base() {
        assert_eq!(
            title_base_revset("@-", "0123456789abcdef"),
            "(0123456789abcdef)..(@-)"
        );
    }
}
