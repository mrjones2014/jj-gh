//! Read-side abstraction over `jj`.
//!
//! All repo reads (commits, bookmarks, remotes) go through [`Jj`]. The production
//! impl shells out to `jj` (and to `git` against jj's embedded store for the
//! remote URL); tests use a fake.

use anyhow::Result;
use serde::Deserialize;

pub mod real;

/// What we read about a single revision.
#[derive(Debug, Clone, Deserialize)]
pub struct CommitInfo {
    pub change_id: String,
    pub commit_id: String,
    pub description: String,
    pub bookmarks: Vec<String>,
}

pub trait Jj {
    /// Resolve a single revision into commit metadata.
    ///
    /// # Errors
    ///
    /// Returns an error if the revset does not resolve to exactly one commit or if
    /// the jj invocation fails.
    fn resolve_rev(&self, rev: &str) -> Result<CommitInfo>;

    /// Closest ancestor commit (excluding `rev` itself) that carries a bookmark.
    ///
    /// # Errors
    ///
    /// Propagates jj errors. Returns `Ok(None)` when no such ancestor exists.
    fn stacked_ancestor_bookmark(&self, rev: &str) -> Result<Option<String>>;

    /// First-line description of the oldest commit in `revset`. Used to compute the
    /// default PR title.
    ///
    /// # Errors
    ///
    /// Propagates jj errors.
    fn first_commit_description(&self, revset: &str) -> Result<String>;

    /// URL configured for the given git remote, or `Ok(None)` if unset.
    ///
    /// # Errors
    ///
    /// Propagates failures from the embedded git store query.
    fn remote_url(&self, name: &str) -> Result<Option<String>>;

    /// Commit SHA of `bookmark@remote` if it exists, else `Ok(None)`.
    ///
    /// # Errors
    ///
    /// Propagates jj errors.
    fn remote_bookmark_sha(&self, bookmark: &str, remote: &str) -> Result<Option<String>>;

    /// `jj git push -c <rev>`. Pushes the change and creates a bookmark if needed.
    ///
    /// # Errors
    ///
    /// Propagates jj failures.
    async fn push(&self, rev: &str) -> Result<()>;
}

/// Compose the revset used to compute the default PR title.
///
/// With a stacked ancestor: commits introduced from the ancestor to `rev`. Without:
/// commits introduced from trunk to `rev`.
#[must_use]
pub fn title_base_revset(rev: &str, ancestor: Option<&str>) -> String {
    match ancestor {
        Some(ancestor) => format!("({ancestor})..({rev})"),
        None => format!("trunk()..({rev})"),
    }
}

/// Detect the default branch on `remote` by probing `main` then `master`.
///
/// # Errors
///
/// Propagates errors from [`Jj::remote_bookmark_sha`].
pub fn default_branch<J: Jj>(jj: &J, remote: &str) -> Result<Option<String>> {
    for branch in ["main", "master"] {
        if jj.remote_bookmark_sha(branch, remote)?.is_some() {
            return Ok(Some(branch.to_string()));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    struct FakeJj {
        remote_branches: HashSet<(String, String)>,
    }

    impl Jj for FakeJj {
        fn resolve_rev(&self, _: &str) -> Result<CommitInfo> {
            unimplemented!()
        }
        fn stacked_ancestor_bookmark(&self, _: &str) -> Result<Option<String>> {
            unimplemented!()
        }
        fn first_commit_description(&self, _: &str) -> Result<String> {
            unimplemented!()
        }
        fn remote_url(&self, _: &str) -> Result<Option<String>> {
            unimplemented!()
        }
        fn remote_bookmark_sha(&self, bookmark: &str, remote: &str) -> Result<Option<String>> {
            Ok(self
                .remote_branches
                .contains(&(bookmark.into(), remote.into()))
                .then(|| format!("{remote}/{bookmark}-sha")))
        }
        async fn push(&self, _: &str) -> Result<()> {
            unimplemented!()
        }
    }

    fn fake(branches: &[(&str, &str)]) -> FakeJj {
        FakeJj {
            remote_branches: branches
                .iter()
                .map(|(b, r)| ((*b).to_string(), (*r).to_string()))
                .collect(),
        }
    }

    #[test]
    fn revset_with_ancestor() {
        assert_eq!(
            title_base_revset("@-", Some("mrj/push-foo")),
            "(mrj/push-foo)..(@-)"
        );
    }

    #[test]
    fn revset_without_ancestor() {
        assert_eq!(title_base_revset("@-", None), "trunk()..(@-)");
    }

    #[test]
    fn default_branch_prefers_main() {
        let jj = fake(&[("main", "origin"), ("master", "origin")]);
        assert_eq!(default_branch(&jj, "origin").unwrap(), Some("main".into()));
    }

    #[test]
    fn default_branch_falls_back_to_master() {
        let jj = fake(&[("master", "origin")]);
        assert_eq!(
            default_branch(&jj, "origin").unwrap(),
            Some("master".into())
        );
    }

    #[test]
    fn default_branch_none_when_neither_present() {
        let jj = fake(&[]);
        assert_eq!(default_branch(&jj, "origin").unwrap(), None);
    }

    #[test]
    fn default_branch_honors_remote_name() {
        let jj = fake(&[("main", "upstream")]);
        assert_eq!(default_branch(&jj, "origin").unwrap(), None);
        assert_eq!(
            default_branch(&jj, "upstream").unwrap(),
            Some("main".into())
        );
    }
}
